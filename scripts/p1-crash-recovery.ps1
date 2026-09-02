<#
 P1 acceptance: R5 (readiness) + R3 (crash recovery).
 Runs against javaboot-daemon over the Named Pipe.

 Sub-test 1 (R5 readiness):
   spawn a fake java that echoes the Spring "Started ... in ... seconds" banner on stdout;
   poll proc.list until status becomes running (regex fallback path).

 Sub-test 2 (R3 crash recovery):
   spawn a long-lived fake "java.exe" whose stdout is redirected to a file (so it survives
   daemon death without broken-pipe); force-kill the daemon; verify the child survives;
   start a NEW daemon; hello reports pending recovery; recovery.list classifies it as exact;
   recovery.takeover adopts it (proc.list shows the pid); then recovery.ignore cleans it.

 Usage:  powershell -ExecutionPolicy Bypass -File scripts\p1-crash-recovery.ps1
#>

$ErrorActionPreference = 'Stop'
$daemonExe = Join-Path $PSScriptRoot "..\src-tauri\target\debug\javaboot-daemon.exe"
$pipeName  = 'javaboot-daemon'
$dir       = Join-Path $env:TEMP "jb-p1-recover"
$fakeJava  = Join-Path $dir "java.exe"   # copy of cmd.exe, but named java.exe

function Stop-Daemon { Get-Process -Name 'javaboot-daemon' -ErrorAction SilentlyContinue | Stop-Process -Force; Start-Sleep -Milliseconds 500 }

function Start-Client {
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
    for (;;) {
        $line = $s.R.ReadLine(); if ($null -eq $line) { throw 'closed' }
        $msg = $line | ConvertFrom-Json; if ($msg.id -eq $id) { return $msg }
    }
}

# ---------- prepare ----------
Stop-Daemon
New-Item -ItemType Directory -Force -Path $dir | Out-Null
Copy-Item "$env:SystemRoot\System32\cmd.exe" $fakeJava -Force

# ================= Sub-test 1 : R5 readiness =================
Start-Process -FilePath $daemonExe -WorkingDirectory (Split-Path $daemonExe) -WindowStyle Hidden
Start-Sleep -Seconds 1
$A = Start-Client
Send-Req $A 'daemon.hello' @{ client_version='0.16.0' } | Out-Null

$argv1 = @($fakeJava, '/c', 'echo Started DemoApplication in 1.234 seconds & ping -n 3 127.0.0.1 >nul')
$sp1 = Send-Req $A 'proc.spawn' @{ project_id='p1'; module_name='banner-svc'; main_class=$null; classpath_key=$null;
    argv=$argv1; env_vars=@{}; working_dir=$dir; dev_mode=$false; auto_restart=$false; startup_port=$null }
$run1 = $sp1.result.run_id
Write-Host ("[R5] spawned run_id={0}" -f $run1)

$ready = $false
for ($i=0; $i -lt 30; $i++) {
    Start-Sleep -Milliseconds 300
    $lst = Send-Req $A 'proc.list' @{}
    $me = $lst.result | Where-Object { $_.run_id -eq $run1 }
    if ($me -and $me.status -eq 'running') { $ready = $true; break }
}
if (-not $ready) { Write-Error '[R5] FAIL: banner service never reached running' }
Write-Host '[R5] PASS: regex readiness -> running'

# cleanup sub-test1
Send-Req $A 'proc.stop' @{ run_id=$run1 } | Out-Null
Start-Sleep -Seconds 1
$A.Pipe.Dispose()

# ================= Sub-test 2 : R3 crash recovery =================
# spawn a long-lived fake java writing to a FILE (not the pipe) so it survives daemon death
$tickFile = Join-Path $dir "svc.log"
if (Test-Path $tickFile) { Remove-Item $tickFile }
$argv2 = @($fakeJava, '/c', "for /L %i in (1,1,4000) do @echo tick-%i>> $tickFile & ping -n 1 127.0.0.1 >nul")
$B = Start-Client
Send-Req $B 'daemon.hello' @{ client_version='0.16.0' } | Out-Null
$sp2 = Send-Req $B 'proc.spawn' @{ project_id='p1'; module_name='recover-svc'; main_class=$null; classpath_key=$null;
    argv=$argv2; env_vars=@{}; working_dir=$dir; dev_mode=$false; auto_restart=$false; startup_port=$null }
$run2 = $sp2.result.run_id; $pid2 = $sp2.result.pid
Write-Host ("[R3] spawned run_id={0} pid={1}" -f $run2, $pid2)

Stop-Daemon
Write-Host '[R3] daemon force-killed; child should survive (job no KILL_ON_JOB_CLOSE)'
Start-Sleep -Seconds 1
$alive = (Get-Process -Id $pid2 -ErrorAction SilentlyContinue) -ne $null
if (-not $alive) { Write-Error "[R3] FAIL: child pid=$pid2 died with daemon" }
Write-Host ('[R3] child pid={0} alive after daemon death: {1}' -f $pid2, $alive)

# start a NEW daemon -> recovery should enumerate it as exact
Start-Process -FilePath $daemonExe -WorkingDirectory (Split-Path $daemonExe) -WindowStyle Hidden
Start-Sleep -Seconds 2
$C = Start-Client
$hello = Send-Req $C 'daemon.hello' @{ client_version='0.16.0' }
Write-Host ("[R3] hello.has_pending_recovery={0}" -f $hello.result.has_pending_recovery)
$rec = Send-Req $C 'recovery.list' @{}
$entry = $rec.result.pending | Where-Object { $_.pid -eq $pid2 }
if ($null -eq $entry) { Write-Error "[R3] FAIL: pid=$pid2 not in recovery.list" }
Write-Host ("[R3] classified kind={0} had_spec={1} run_id={2}" -f $entry.kind, $entry.had_spec, $entry.run_id)
if ($entry.kind -ne 'exact' -or -not $entry.had_spec) { Write-Error '[R3] FAIL: expected exact + had_spec' }

# takeover -> adopt into tracking
Send-Req $C 'recovery.takeover' @{ pid=$pid2 } | Out-Null
$lst = Send-Req $C 'proc.list' @{}
$adopted = $lst.result | Where-Object { $_.pid -eq $pid2 }
if ($null -eq $adopted) { Write-Error '[R3] FAIL: takeover not reflected in proc.list' }
Write-Host ('[R3] PASS: takeover -> run_id={0} status={1}' -f $adopted.run_id, $adopted.status)

# stop the adopted process (cleanup)
Send-Req $C 'proc.stop' @{ run_id=$adopted.run_id } | Out-Null
Start-Sleep -Seconds 1
# ensure remaining recovery entries are ignored to empty the list
$rec2 = Send-Req $C 'recovery.list' @{}
foreach ($e in $rec2.result.pending) { Send-Req $C 'recovery.ignore' @{ pid=$e.pid } | Out-Null }
$C.Pipe.Dispose()

Write-Host ''
Write-Host 'P1 ACCEPTED: R5 readiness (regex) + R3 crash-recovery exact classify/takeover'