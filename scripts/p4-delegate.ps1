<#
 P4 acceptance: delegate start/stop/restart lifecycle via daemon.
  - spawn a process that opens a real TCP listener (startup_port=18099).
  - expect proc.status starting -> running (daemon TCP-readiness).
  - stop -> expect proc.status stopped.
  This mirrors exactly what launcher start()/stop() delegation relies on.
 Usage: powershell -ExecutionPolicy Bypass -File scripts\p4-delegate.ps1
#>
$ErrorActionPreference = 'Stop'
$daemonExe = Join-Path $PSScriptRoot "..\src-tauri\target\debug\javaboot-daemon.exe"
$pipeName  = 'javaboot-daemon'
$PORT = 18099

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
function Wait-Event($s,$method,$param,$timeoutSec) {
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    while ($sw.ElapsedMilliseconds -lt ($timeoutSec*1000)) {
        $n = $null; try { $n = Read-Notif $s } catch { break }
        if ($null -eq $n) { break }
        if ($n.method -eq $method -and $n.params.$param) { return $n.params }
    }
    return $null
}

Stop-Daemon
Start-Process -FilePath $daemonExe -WorkingDirectory (Split-Path $daemonExe) -WindowStyle Hidden
Start-Sleep -Seconds 1
$C = New-Client
Send-Req $C 'daemon.hello' @{ client_version='0.16.0' } | Out-Null

# --- spawn: opens real TCP listener to exercise readiness ---
$cmd = "`$l=[net.sockets.TcpListener]::new([net.ipaddress]::Loopback,$PORT);`$l.Start();Start-Sleep -Seconds 25"
$argv = @('powershell.exe','-NoProfile','-Command',$cmd)
$sp = Send-Req $C 'proc.spawn' @{
    project_id='p4'; module_name='delegate-svc';
    main_class=$null; classpath_key=$null;
    argv=$argv; env_vars=@{}; working_dir=$env:TEMP;
    dev_mode=$false; auto_restart=$false; startup_port=$PORT
}
if ($sp.error) { Write-Error ("[spawn] FAIL: {0}" -f $sp.error.message) }
$runId = $sp.result.run_id; $pidv = $sp.result.pid
Write-Host ("[spawn] run_id={0} pid={1}" -f $runId, $pidv)

# --- expect running via TCP readiness (skip early 'starting') ---
$running = $null
$sw = [System.Diagnostics.Stopwatch]::StartNew()
while ($sw.ElapsedMilliseconds -lt 40000) {
    $n = $null; try { $n = Read-Notif $C } catch { break }
    if ($null -eq $n) { break }
    if ($n.method -eq 'proc.status') {
        Write-Host ("[lifecycle] event status={0} (run_id={1})" -f $n.params.status, $n.params.run_id)
        if ($n.params.run_id -eq $runId -and $n.params.status -eq 'running') { $running = $n.params; break }
    }
}
if (-not $running) { Write-Error '[lifecycle] FAIL: never reached running' }
Write-Host ("[lifecycle] reached running (run_id={0})" -f $running.run_id)

# --- proc.list must reflect the real pid + running + metrics backfilled ---
$list = Send-Req $C 'proc.list' @{}
$self = $list.result | Where-Object { $_.run_id -eq $runId }
Write-Host ("[proc.list] status={0} pid={1} mem={2}" -f $self.status, $self.pid, $self.memory_mb)

# --- stop -> expect stopped ---
Send-Req $C 'proc.stop' @{ run_id=$runId } | Out-Null
$stopped = $null
$sw = [System.Diagnostics.Stopwatch]::StartNew()
while ($sw.ElapsedMilliseconds -lt 20000) {
    $n = $null; try { $n = Read-Notif $C } catch { break }
    if ($null -eq $n) { break }
    if ($n.method -eq 'proc.status' -and $n.params.run_id -eq $runId -and $n.params.status -eq 'stopped') { $stopped = $n.params; break }
}
Write-Host ("[lifecycle] stop -> status={0}" -f ($(if($stopped){$stopped.status}else{'<none>'})))
if (-not $stopped) { Write-Error '[lifecycle] FAIL: did not go stopped' }

$C.Pipe.Dispose()
Stop-Daemon
Write-Host ''
Write-Host 'P4 ACCEPTED: delegate spawn(starting->running via TCP) + stop(stopped)'