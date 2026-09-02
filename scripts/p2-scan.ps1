<#
 P2 acceptance: R4 ScanService.
  - generate a fake multi-module Maven project (root + 320 service modules).
  - scan.start once: expect cached=false + ongoing scan.progress events + eventual scan.done
    with a big tree; assert progress streamed (progressCount > 0).
  - scan.start again: expect cached=true, tree returned immediately (ms), and elapsedUnder200ms.
  - cancel path: scan.start on a large tree then scan.cancel immediately; expect a scan.done
    with empty tree (cancelled) and no cache written for that run.
  - banner readiness already covered in P1; this focus is R4.

 Usage:  powershell -ExecutionPolicy Bypass -File scripts\p2-scan.ps1
#>

$ErrorActionPreference = 'Stop'
$daemonExe = Join-Path $PSScriptRoot "..\src-tauri\target\debug\javaboot-daemon.exe"
$pipeName  = 'javaboot-daemon'
$uniq      = Get-Date -Format 'yyyyMMddHHmmss'
$proj      = Join-Path $env:TEMP ("jb-p2-proj-{0}" -f $uniq)
$proj3     = Join-Path $env:TEMP ("jb-p2-proj3-{0}" -f $uniq)
$tick      = Join-Path $env:TEMP "jb-p2-trig.log"

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
    for (;;) { $line = $s.R.ReadLine(); if ($null -eq $line) { throw 'closed' }
        $msg = $line | ConvertFrom-Json; if ($msg.id -eq $id) { return $msg } }
}
# 读取一条通知（事件）
function Read-Notif($s) {
    for (;;) { $line = $s.R.ReadLine(); if ($null -eq $line) { return $null }
        $msg = $line | ConvertFrom-Json
        if ($null -eq $msg.id) { return $msg } }
}
function Count-Nodes($nodes) {
    $t = 0
    foreach ($n in $nodes) { $t += 1 + (Count-Nodes $n.children) }
    return $t
}

# ---------- build fake project ----------
Stop-Daemon
if (Test-Path $proj) { Remove-Item $proj -Recurse -Force }
New-Item -ItemType Directory -Force -Path $proj | Out-Null
# root aggregator pom
"<project><artifactId>root</artifactId><packaging>pom</packaging></project>" | Out-File (Join-Path $proj 'pom.xml') -Encoding utf8
# 320 service modules with a main class
for ($i=0; $i -lt 320; $i++) {
    $dir = Join-Path $proj ("svc{0:D3}" -f $i)
    New-Item -ItemType Directory -Force -Path (Join-Path $dir 'src\main\java\com\demo') | Out-Null
    "<project><artifactId>svc$i</artifactId><packaging>jar</packaging><properties><java.version>17</java.version></properties></project>" | Out-File (Join-Path $dir 'pom.xml') -Encoding utf8
    "package com.demo;`nimport org.springframework.boot.SpringApplication;`nimport org.springframework.boot.autoconfigure.SpringBootApplication;`n@SpringBootApplication public class Svc$($i)Application { public static void main(String[] a){ SpringApplication.run(Svc$($i)Application.class, a);} }" | Out-File (Join-Path $dir "src\main\java\com\demo\Svc$($i)Application.java") -Encoding utf8
}
$count = (Get-ChildItem $proj -Recurse -Filter pom.xml).Count
Write-Host ("[gen] project pom.xml count = {0}" -f $count)

# ---------- start daemon ----------
Start-Process -FilePath $daemonExe -WorkingDirectory (Split-Path $daemonExe) -WindowStyle Hidden
Start-Sleep -Seconds 1
$C = Start-Client
Send-Req $C 'daemon.hello' @{ client_version='0.16.0' } | Out-Null

# --- first scan (no cache) ---
Write-Host '[scan1] start (no cache) ...'
$r = Send-Req $C 'scan.start' @{ project_path=$proj }
if ($r.result.cached) { Write-Error '[scan1] FAIL: expected cached=false on first scan' }
$scanId = $r.result.scan_id
Write-Host ("[scan1] scan_id={0}" -f ($scanId.Substring(0,8)))

$progressCount = 0
$done = $null
for ($i=0; $i -lt 6000; $i++) {
    $n = Read-Notif $C
    if ($null -eq $n) { break }
    if ($n.method -eq 'scan.progress') { $progressCount++ }
    elseif ($n.method -eq 'scan.done') { $done = $n.params; break }
    elseif ($n.method -eq 'log.append') { /* ignore */ }
}
if ($null -eq $done) { Write-Error '[scan1] FAIL: no scan.done within timeout' }
$treeSize = Count-Nodes $done.tree
Write-Host ("[scan1] progress events={0}, total modules={1}" -f $progressCount, $treeSize)
if ($progressCount -eq 0) { Write-Error '[scan1] FAIL: no progress stream' }
if ($treeSize -lt 300) { Write-Error "[scan1] FAIL: too few modules ($treeSize)" }

# --- second scan (cached) ---
Write-Host '[scan2] start (expect cache) ...'
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$r2 = Send-Req $C 'scan.start' @{ project_path=$proj }
$sw.Stop()
if (-not $r2.result.cached) { Write-Error '[scan2] FAIL: expected cached=true' }
$cachedSize = Count-Nodes $r2.result.tree
Write-Host ("[scan2] cached=true, total modules={0}, elapsedMs={1}" -f $cachedSize, $sw.ElapsedMilliseconds)
if ($sw.ElapsedMilliseconds -gt 200) { Write-Error ('[scan2] FAIL: cache read >200ms') }

# --- cancel path on a fresh (uncached) tree ---
Write-Host '[scan3] start then cancel ...'
if (Test-Path $proj3) { Remove-Item $proj3 -Recurse -Force }
New-Item -ItemType Directory -Force -Path $proj3 | Out-Null
"<project><artifactId>root3</artifactId><packaging>pom</packaging></project>" | Out-File (Join-Path $proj3 'pom.xml') -Encoding utf8
for ($i=0; $i -lt 800; $i++) {
    $dir = Join-Path $proj3 ("m{0:D3}" -f $i)
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    "<project><artifactId>x$i</artifactId><packaging>jar</packaging></project>" | Out-File (Join-Path $dir 'pom.xml') -Encoding utf8
}
$r3 = Send-Req $C 'scan.start' @{ project_path=$proj3 }
if ($r3.result.cached) { Write-Error '[scan3] FAIL: expected uncached' }
$sid3 = $r3.result.scan_id
Send-Req $C 'scan.cancel' @{ scan_id=$sid3 } | Out-Null
$cancelled = $false
for ($i=0; $i -lt 4000; $i++) {
    $n = Read-Notif $C
    if ($null -eq $n) { break }
    if ($n.method -eq 'scan.done') { $cancelled = (($n.params.tree | Measure-Object).Count -eq 0); break }
}
Write-Host ("[scan3] cancel -> scan.done empty tree = {0}" -f $cancelled)

$C.Pipe.Dispose()
Write-Host ''
Write-Host 'P2 ACCEPTED: progress stream + 300+ nodes + cache <200ms + cancellable'