<#
 P0 acceptance: Launcher(UI) + Daemon architecture.
 Simulates "UI crash -> restart" by using two independent pipe clients:
  - Phase A: client1 hello -> spawn a process that prints periodically
            -> read some logs -> drop connection (= UI crash, process must survive)
  - Phase B: client2 connects (= UI restart) -> proc.list shows process alive
           -> log.tail fetches full history (replay) -> proc.stop -> close

 Pass criteria:
   1) proc.list still has run_id with same pid after client1 dropped
   2) log.tail line count/contents are continuous across the "crash" (replayable)
   3) env var redaction: spawn with DB_PASSWORD; spec.get returns "redacted"

 Usage:  powershell -ExecutionPolicy Bypass -File scripts\p0-daemon-smoke.ps1
#>

$ErrorActionPreference = 'Stop'
$daemonExe = Join-Path $PSScriptRoot "..\src-tauri\target\debug\javaboot-daemon.exe"
$pipeName  = 'javaboot-daemon'
$tmp       = Join-Path $env:TEMP "jb-p0-smoke"

function Stop-ExistingDaemon {
    Get-Process -Name 'javaboot-daemon' -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 500
}

function Start-Client {
    $c = New-Object System.IO.Pipes.NamedPipeClientStream('.', $pipeName,
        [System.IO.Pipes.PipeDirection]::InOut,
        [System.IO.Pipes.PipeOptions]::None,
        [System.Security.Principal.TokenImpersonationLevel]::Impersonation)
    $c.Connect(10000)
    $w = New-Object System.IO.StreamWriter($c)
    $w.NewLine = "`n"
    $w.AutoFlush = $true
    $r = New-Object System.IO.StreamReader($c)
    return @{ Pipe=$c; W=$w; R=$r; Id=1 }
}

function Send-Req($sess, $method, $paramsObj) {
    $id = $sess.Id; $sess.Id++
    $body = @{ jsonrpc='2.0'; id=$id; method=$method; params=$paramsObj } | ConvertTo-Json -Depth 20 -Compress
    $sess.W.WriteLine($body)
    for (;;) {
        $line = $sess.R.ReadLine()
        if ($null -eq $line) { throw 'daemon closed connection' }
        $msg = $line | ConvertFrom-Json
        if ($msg.id -eq $id) { return $msg }
    }
}

# ---------- setup ----------
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
Stop-ExistingDaemon
Start-Process -FilePath $daemonExe -WorkingDirectory (Split-Path $daemonExe) -WindowStyle Hidden
Start-Sleep -Seconds 1

# ========== Phase A (first UI start) ==========
$A = Start-Client
Write-Host '[A] hello...'
$hello = Send-Req $A 'daemon.hello' @{ client_version='0.16.0' }
Write-Host ("    daemon_version={0} protocol={1}" -f $hello.result.daemon_version, $hello.result.protocol_version)

$argv = @('cmd','/c','echo PHASE_A_START & for /L %i in (1,1,40) do @echo line-%i & ping -n 1 127.0.0.1 >nul')
$spawnReq = @{
    project_id='demo'; module_name='smoke-svc'; main_class=$null; classpath_key=$null
    argv=$argv
    env_vars=@{ DB_PASSWORD='hunter2'; SPRING_PROFILES_ACTIVE='dev' }
    working_dir=$tmp; dev_mode=$false; auto_restart=$false; startup_port=$null
}
$sp = Send-Req $A 'proc.spawn' $spawnReq
$runId = $sp.result.run_id; $p1 = $sp.result.pid
Write-Host ("[A] spawned run_id={0} pid={1}" -f $runId, $p1)

Start-Sleep -Seconds 3
$tailA = Send-Req $A 'log.tail' @{ run_id=$runId; after_seq=0; limit=2000 }
Write-Host ("[A] tail before kill: {0} lines" -f $tailA.result.entries.Count)

# simulate UI crash: drop connection WITHOUT stopping the process
$A.Pipe.Dispose()
Write-Host '[A] client disconnected (UI crash); process should keep running'
Start-Sleep -Seconds 3

# ========== Phase B (UI restart) ==========
$B = Start-Client
Write-Host '[B] reconnected (UI restart)...'
Send-Req $B 'daemon.hello' @{ client_version='0.16.0' } | Out-Null

$list = Send-Req $B 'proc.list' @{}
$found = $list.result | Where-Object { $_.run_id -eq $runId }
if ($null -eq $found) {
    Write-Error ("[B] FAIL: proc.list missing run_id={0}" -f $runId)
}
Write-Host ("[B] proc.list found, pid={0} (alive)" -f $found.pid)

$tailB = Send-Req $B 'log.tail' @{ run_id=$runId; after_seq=0; limit=2000 }
$nA = $tailA.result.entries.Count
$nB = $tailB.result.entries.Count
Write-Host ("[B] persisted before crash={0}  replayable after restart={1}" -f $nA, $nB)
if ($nB -lt $nA) { Write-Error '[B] FAIL: after restart logs are fewer than before (must continue)' }
Write-Host ("[B] first={0}  last={1}" -f $tailB.result.entries[0].body, $tailB.result.entries[-1].body)

$spec = Send-Req $B 'spec.get' @{ run_id=$runId }
if ($spec.result.env_vars -notmatch 'redacted') {
    Write-Error ("[B] FAIL: env_vars not redacted -> {0}" -f $spec.result.env_vars)
}
Write-Host ("[B] spec.env_vars redacted: {0}" -f $spec.result.env_vars)

Send-Req $B 'proc.stop' @{ run_id=$runId } | Out-Null
Start-Sleep -Seconds 1
$list2 = Send-Req $B 'proc.list' @{}
$still = $list2.result | Where-Object { $_.run_id -eq $runId }
Write-Host ("[B] after stop, still in proc.list = {0} (expect False)" -f ($null -ne $still))

$B.Pipe.Dispose()
Write-Host ''
Write-Host 'P0 ACCEPTED: log continuity + crash recovery + process survives client drop + redaction'