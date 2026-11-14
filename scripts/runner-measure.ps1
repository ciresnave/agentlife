# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Read-only: how big are the runner folders and Windows profiles? Needs an elevated shell (the runner accounts' folders are
  unreadable to anyone else); started without elevation it asks for UAC once and re-runs itself. Writes a result file.
.DESCRIPTION
  For C:\gh-runner*, C:\ghr-*, C:\actions-runners\* and every folder under C:\Users: size per top-level folder inside it (MB, file
  count, the folder's own write time), and for a C:\Users profile whether the registry's ProfileList still names it, whether a
  local account of that name exists, and when NTUSER.DAT was last written (the last logon or logoff). Sizes, counts, dates and
  top-level folder names ONLY: no file is opened, no .env is read, no content printed. Nothing is deleted or changed. This
  answers "is ghr-baracuda.TUF-LAPTOP.000/.001/.002 unused?" with data (an old NTUSER.DAT, no ProfileList entry, no account) but
  deletes nothing: that stays CireSnave's call. Sizes come from `robocopy /L` (lists, copies nothing, skips junctions).
.PARAMETER Root  Test hook (one string, folders separated by ;): measure only these folders instead of the standard set (the C:\Users listing still runs).
.PARAMETER Out   The result file. Default C:\Users\Public\runner-measure-<date>.txt (readable by the owner after the elevated run).
#>
[CmdletBinding()]
param([string[]]$Root = @(), [string]$Out = ("C:\Users\Public\runner-measure-{0:yyyyMMdd-HHmm}.txt" -f (Get-Date)), [switch]$NoUsers)
$ErrorActionPreference = 'Continue'
$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin -and -not $env:RUNNER_MEASURE_NO_ELEVATE) {
  Write-Host "Not elevated: asking for UAC once; the result goes to $Out"
  Start-Process -FilePath (Get-Process -Id $PID).Path -Verb RunAs -Wait -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Out', "`"$Out`"")
  if (Test-Path -LiteralPath $Out) { Write-Host "Result: $Out" } else { Write-Host "No result file: UAC declined or the run failed." }
  return
}
$lines = New-Object System.Collections.Generic.List[string]
function Out([string]$s) { $lines.Add($s) }
function Measure-Tree([string]$p) {
  $empty = Join-Path $env:TEMP 'runner-measure-empty-target'
  $opts = @('/L', '/E', '/XJ', '/R:0', '/W:0', '/NP', '/NFL', '/NDL', '/BYTES', '/NJH')
  if ($admin) { $opts += '/B' }   # backup rights exist only when elevated
  $o = & robocopy.exe $p $empty @opts 2>&1 | Out-String
  $b = 0L; $n = 0; $failed = 0
  if ($o -match '(?m)^\s*Bytes\s*:\s*(\d+)') { $b = [int64]$Matches[1] }
  if ($o -match '(?m)^\s*Files\s*:\s*(\d+)') { $n = [int]$Matches[1] }
  if ($o -match '(?m)^\s*Files\s*:\s*\d+\s+\d+\s+\d+\s+\d+\s+(\d+)') { $failed = [int]$Matches[1] }
  $w = (Get-Item -LiteralPath $p -Force -ErrorAction SilentlyContinue).LastWriteTime
  [pscustomobject]@{ MB = [math]::Round($b / 1MB, 1); Files = $n; Written = $(if ($w) { $w.ToString('yyyy-MM-dd HH:mm') } else { '-' }); Failed = $failed }
}
function Report([string]$dir) {
  $kids = @(Get-ChildItem -LiteralPath $dir -Force -Directory -ErrorAction SilentlyContinue)
  $rows = @(foreach ($c in $kids) { [pscustomobject]@{ Name = $c.Name; X = (Measure-Tree $c.FullName) } })
  $loose = @(Get-ChildItem -LiteralPath $dir -Force -File -ErrorAction SilentlyContinue)
  $looseMb = if ($loose.Count -gt 0) { ($loose | Measure-Object Length -Sum).Sum / 1MB } else { 0 }
  $mb = [math]::Round((($rows | ForEach-Object { $_.X.MB } | Measure-Object -Sum).Sum + $looseMb), 1)
  $n = ($rows | ForEach-Object { $_.X.Files } | Measure-Object -Sum).Sum + $loose.Count
  $fail = ($rows | ForEach-Object { $_.X.Failed } | Measure-Object -Sum).Sum
  Out ("{0,-58} {1,12:N1} MB {2,8} files  unreadable-files {3}" -f $dir, $mb, $n, $fail)
  foreach ($r in $rows) { if ($r.X.MB -ge 1) { Out ("    {0,-54} {1,12:N1} MB {2,8} files  folder written {3}" -f $r.Name, $r.X.MB, $r.X.Files, $r.X.Written) } }
}
Out "runner-measure $(Get-Date -Format 'yyyy-MM-dd HH:mm') on $env:COMPUTERNAME, elevated=$admin; sizes only (folders of 1 MB or more shown)"
Out ""
$roots = @()
if ($Root.Count -gt 0) { $roots = @($Root | ForEach-Object { $_ -split ';' } | Where-Object { $_ }) }
else {
  $roots += @(Get-ChildItem C:\ -Directory -Force -ErrorAction SilentlyContinue | Where-Object { $_.Name -like 'gh-runner*' -or $_.Name -like 'ghr-*' } | ForEach-Object { $_.FullName })
  $roots += @(Get-ChildItem C:\actions-runners -Directory -Force -ErrorAction SilentlyContinue | ForEach-Object { $_.FullName })
}
foreach ($r in $roots) { Report $r }
if (-not $NoUsers) {
  Out ""
  Out "--- C:\Users profiles (ProfileList = does the registry still name the folder; Account = does a local account of that name exist)"
  $pl = @{}
  foreach ($k in Get-ChildItem 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList' -ErrorAction SilentlyContinue) {
    $path = (Get-ItemProperty -LiteralPath $k.PSPath -ErrorAction SilentlyContinue).ProfileImagePath
    if ($path) { $pl[$path.ToLowerInvariant()] = $k.PSChildName }
  }
  $accounts = @(Get-LocalUser -ErrorAction SilentlyContinue | ForEach-Object { $_.Name.ToLowerInvariant() })
  foreach ($u in @(Get-ChildItem C:\Users -Directory -Force -ErrorAction SilentlyContinue)) {
    $base = ($u.Name -split '\.')[0].ToLowerInvariant()
    $nt = Get-Item -LiteralPath (Join-Path $u.FullName 'NTUSER.DAT') -Force -ErrorAction SilentlyContinue
    Out ("[{0}] ProfileList={1} Account={2} NTUSER.DAT last written={3}" -f $u.Name, $(if ($pl.ContainsKey($u.FullName.ToLowerInvariant())) { 'yes' } else { 'NO' }), $(if ($accounts -contains $base) { 'yes (' + $base + ')' } else { 'no' }), $(if ($nt) { $nt.LastWriteTime.ToString('yyyy-MM-dd HH:mm') } else { 'none' }))
    Report $u.FullName
  }
}
Out ""
Out "done."
[IO.File]::WriteAllLines($Out, $lines)
Write-Host "Result: $Out"
