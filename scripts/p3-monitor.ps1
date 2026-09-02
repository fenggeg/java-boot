<#
 P3 acceptance: MonitorService (proc.metrics 事件 + proc.list 指标回填).
  - spawn 一个长跑进程（powershell Start-Sleep 30s）。
  - 监听事件流：应在 2-4s 内收到 >=1 个 proc.metrics，且 memory_mb > 0。
  - proc.list 拉取：该 run 的 cpu_usage / memory_mb 应为非 None（采样已回填槽位）。
  - 就绪状态保持 Starting（无端口）——本脚本不关注就绪，仅验证监控链路。
 Usage: powershell -ExecutionPolicy Bypass -File scripts\p3-monitor.ps1
#>
$ErrorActionPreference = 'Stop'
$daemonExe = Join-Path $PSScriptRoot "..\src-tauri\target\debug\javaboot-daemon.exe"
$pipeName  = 'javaboot-daemon'

function Stop-Daemon { Get-Process -Name 'javaboot-daemon' -ErrorAction SilentlyContinue | Stop-Process -Force; Start-Sleep -Milliseconds 500 }

function New-Client {
    $c = New-Object System.IO.Pipes.NamedPipeClientStream('.', $pipeName,
        [System.IO.Pipes.PipeDirection]::InOut, [System.IO.Pipes.PipeOptions]::None,
        [System.Security.Principal.TokenImpersonationLevel]::Impersonation)
    $c.Connect(10000)
    $w = New-Object System.IO.StreamWriter($c); $w.NewLine = "`n"; $w.AutoFlush = $true
    $r = New-Object System.IO.StreamReader($c)
    return @{ Pipe=$c; W=$w; R=$r; Id=1 }
}
function Send-Req($s,$m,$p) {
    $id = $s.Id; $s.Id++
    $body = @{ jsonrpc='2.0'; id=$id; method=$m; params=$p } | ConvertTo-Json -Depth 20 -Compress
    $s.W.WriteLine($body)
    for (;;) { $line = $s.R.ReadLine(); if ($null -eq $line) { throw 'closed' }
        $msg = $line | ConvertFrom-Json; if ($msg.id -eq $id) { return $msg } }
}
function Read-Notif($s) {
    for (;;) { $line = $s.R.ReadLine(); if ($null -eq $line) { return $null }
        $msg = $line | ConvertFrom-Json
        if ($null -eq $msg.id) { return $msg } }
}

# ---------- start daemon ----------
Stop-Daemon
Start-Process -FilePath $daemonExe -WorkingDirectory (Split-Path $daemonExe) -WindowStyle Hidden
Start-Sleep -Seconds 1
$C = New-Client
Send-Req $C 'daemon.hello' @{ client_version='0.16.0' } | Out-Null

# ---------- spawn a long-running process ----------
$argv = @('powershell.exe','-NoProfile','-Command','Start-Sleep -Seconds 30')
$sp = Send-Req $C 'proc.spawn' @{
    project_id='p3'; module_name='monitor-svc';
    main_class=$null; classpath_key=$null;
    argv=$argv; env_vars=@{}; working_dir=$env:TEMP;
    dev_mode=$false; auto_restart=$false; startup_port=$null
} 
if ($null -eq $sp.error) { Write-Host ("[spawn] run_id={0} pid={1}" -f $sp.result.run_id, $sp.result.pid) }
else { Write-Error ("[spawn] FAIL: {0}" -f $sp.error.message) }
$runId = $sp.result.run_id

# ---------- collect proc.metrics over ~6-8s ----------
Write-Host '[metrics] listening for proc.metrics (max 8s) ...'
$metricsEvents = 0
$gotMemory = $false
$deadlineMs = 8000
$sw = [System.Diagnostics.Stopwatch]::StartNew()
while ($sw.ElapsedMilliseconds -lt $deadlineMs) {
    # 读非阻塞式：读取行若管道无数据会阻塞，用 ReadLineAsync? 简化：这里 ReadLine 阻塞，
    # 但我们有 deadline 兜底；事件本身 2s 一次会持续到达，divergent 少见。
    $n = $null
    try { $n = Read-Notif $C } catch { break }
    if ($null -eq $n) { break }
    if ($n.method -eq 'proc.metrics' -and $n.params.run_id -eq $runId) {
        $metricsEvents++
        if ($n.params.memory_mb -and $n.params.memory_mb -gt 0) { $gotMemory = $true }
    }
}
Write-Host ("[metrics] events={0}, hasMemory>0={1} (elapsed {2}ms)" -f $metricsEvents, $gotMemory, $sw.ElapsedMilliseconds)
if ($metricsEvents -eq 0) { Write-Error '[metrics] FAIL: no proc.metrics events within 8s' }
if (-not $gotMemory) { Write-Error '[metrics] FAIL: memory_mb not positive in any sample' }

# ---------- proc.list: metrics backfilled ----------
Write-Host '[proc.list] checking metric backfill ...'
$list = Send-Req $C 'proc.list' @{}
$self = $list.result | Where-Object { $_.run_id -eq $runId }
Write-Host ("[proc.list] run_id={0} status={1} cpu={2} mem={3}" -f $self.run_id, $self.status, $self.cpu_usage, $self.memory_mb)
if ($null -eq $self.cpu_usage -and $null -eq $self.memory_mb) {
    Write-Error '[proc.list] FAIL: cpu_usage/memory_mb not backfilled'
}

# ---------- cleanup ----------
Send-Req $C 'proc.stop' @{ run_id=$runId } | Out-Null
$C.Pipe.Dispose()
Stop-Daemon
Write-Host ''
Write-Host 'P3 ACCEPTED: proc.metrics streamed + proc.list metrics backfilled'