# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Puts the job-completed cleanup hook on the live, owner-run runners (C:\actions-runners\<name>, started by run.cmd or
  start-runner.cmd, not services). Dry run unless -Apply.
.DESCRIPTION
  Per runner dir (must hold config.cmd): copies scripts\runner-job-cleanup-hook.ps1 to <dir>\job-completed-cleanup.ps1, and sets
  two lines in <dir>\.env: ACTIONS_RUNNER_HOOK_JOB_COMPLETED=<dir>\job-completed-cleanup.ps1 and CARGO_TARGET_DIR=<dir>\cargo-target
  (so everything a job builds lands inside the folder the hook empties). The .env is created when absent. Idempotent: a line that is
  already right is left alone, a wrong one of the same name is replaced, nothing else in the file is touched, a missing trailing
  newline is handled and the file's own line ending kept. The .env is NEVER printed: the output names keys and counts only.
  The runner reads .env at start, so a running listener needs a restart. Only an IDLE listener is restarted (Runner.Listener.exe
  of that dir with no Runner.Worker.exe child); a busy one is reported and left alone, to be restarted after its job.
  A listener whose path this account cannot read (another user's) is reported, never touched.
.PARAMETER Dir      The runner dirs. Default C:\actions-runners\baracuda-gpu, humboldt, humboldt-2.
.PARAMETER Apply    Write the files and restart idle listeners. Without it, print what would be done.
.PARAMETER NoRestart With -Apply: write the files but restart nothing.
#>
[CmdletBinding()]
param(
  [string[]]$Dir = @('C:\actions-runners\baracuda-gpu', 'C:\actions-runners\humboldt', 'C:\actions-runners\humboldt-2'),
  [switch]$Apply,
  [switch]$NoRestart
)
$ErrorActionPreference = 'Stop'

# Pure: the .env text with each key set to its value. Keeps the file's line ending (CRLF when there is none to copy), replaces a
# line of the same key (case-insensitive) in place, appends a missing one, ends the text with a newline.
function Update-EnvText([string]$text, [System.Collections.IDictionary]$pairs) {
  $eol = if ($text -match "`r`n") { "`r`n" } elseif ($text -match "`n") { "`n" } else { "`r`n" }
  $lines = New-Object System.Collections.Generic.List[string]
  if ($text.Length -gt 0) { foreach ($l in ($text -split "`r?`n")) { $lines.Add($l) } }
  if ($lines.Count -gt 0 -and $lines[$lines.Count - 1] -eq '') { $lines.RemoveAt($lines.Count - 1) }
  foreach ($k in $pairs.Keys) {
    $want = "$k=$($pairs[$k])"; $done = $false
    for ($i = 0; $i -lt $lines.Count; $i++) {
      if ($lines[$i] -match '^\s*([^=\s#][^=]*?)\s*=' -and $Matches[1] -ieq $k) {
        if (-not $done) { $lines[$i] = $want; $done = $true } else { $lines[$i] = $null }
      }
    }
    if (-not $done) { $lines.Add($want) }
  }
  (@($lines | Where-Object { $null -ne $_ }) -join $eol) + $eol
}

# Pure: the listener pids of this dir that are idle / busy, from (Name, Id, Path, ParentId) records.
function Get-ListenerState([string]$dir, $procs) {
  $bin = (Join-Path $dir 'bin').TrimEnd('\') + '\'
  $mine = @($procs | Where-Object { $_.Name -like 'Runner.Listener*' -and $_.Path -and $_.Path.StartsWith($bin, [StringComparison]::OrdinalIgnoreCase) })
  $unreadable = @($procs | Where-Object { $_.Name -like 'Runner.Listener*' -and -not $_.Path }).Count
  foreach ($l in $mine) {
    $kids = @($procs | Where-Object { $_.Name -like 'Runner.Worker*' -and $_.ParentId -eq $l.Id })
    [pscustomobject]@{ Id = $l.Id; Busy = ($kids.Count -gt 0) }
  }
}

if ($MyInvocation.InvocationName -eq '.') { return }   # dot-sourced by the tests: functions only

$hookSrc = Join-Path $PSScriptRoot 'runner-job-cleanup-hook.ps1'
if (-not (Test-Path -LiteralPath $hookSrc)) { throw "missing $hookSrc" }
Write-Host ("{0} (dry run: nothing is written)" -f $(if ($Apply) { 'APPLY' } else { 'PLAN' })) -ForegroundColor Cyan
$procs = @(Get-Process -Name Runner.Listener, Runner.Worker -ErrorAction SilentlyContinue | ForEach-Object {
    $pp = try { $_.Parent.Id } catch { 0 }
    [pscustomobject]@{ Name = $_.Name; Id = $_.Id; Path = $_.Path; ParentId = $pp }
  })
$busyLeft = @()
foreach ($d in $Dir) {
  $d = $d.TrimEnd('\')
  Write-Host "`n[$d]"
  if (-not (Test-Path -LiteralPath "$d\config.cmd")) { Write-Host "  SKIPPED: no config.cmd, not a runner dir"; continue }
  $pairs = [ordered]@{ ACTIONS_RUNNER_HOOK_JOB_COMPLETED = "$d\job-completed-cleanup.ps1"; CARGO_TARGET_DIR = "$d\cargo-target" }
  $envPath = "$d\.env"
  $old = if (Test-Path -LiteralPath $envPath) { [IO.File]::ReadAllText($envPath) } else { '' }
  $new = Update-EnvText $old $pairs
  $changed = ($new -ne $old)
  Write-Host ("  hook file : {0}" -f $(if ((Test-Path -LiteralPath "$d\job-completed-cleanup.ps1") -and ((Get-FileHash $hookSrc).Hash -eq (Get-FileHash "$d\job-completed-cleanup.ps1").Hash)) { 'already current' } else { 'to be copied' }))
  Write-Host ("  .env      : {0} (keys set: {1}; the file is never printed)" -f $(if ($changed) { if ($old -eq '') { 'to be created' } else { 'to be updated' } } else { 'already right' }), ($pairs.Keys -join ', '))
  if ($Apply) {
    Copy-Item -LiteralPath $hookSrc -Destination "$d\job-completed-cleanup.ps1" -Force
    if ($changed) { [IO.File]::WriteAllText($envPath, $new, (New-Object Text.ASCIIEncoding)) }
    $back = [IO.File]::ReadAllText($envPath)
    foreach ($k in $pairs.Keys) { if ($back -notmatch ('(?m)^' + [regex]::Escape($k) + '=' + [regex]::Escape($pairs[$k]) + '\r?$')) { throw "$envPath does not hold $k after the write" } }
    Write-Host "  written and read back."
  }
  $ls = @(Get-ListenerState $d $procs)
  if ($ls.Count -eq 0) { Write-Host "  listener  : none of this dir visible to this account (not running, or another user's): start it normally; it will read the new .env" ; continue }
  foreach ($l in $ls) {
    if ($l.Busy) { Write-Host "  listener  : pid $($l.Id) is BUSY (a Runner.Worker child): NOT restarted; restart it after its job to pick up the new .env"; $busyLeft += $d; continue }
    if (-not $Apply -or $NoRestart) { Write-Host "  listener  : pid $($l.Id) is idle: would be restarted"; continue }
    $again = @(Get-Process -Name Runner.Worker -ErrorAction SilentlyContinue | Where-Object { try { $_.Parent.Id -eq $l.Id } catch { $false } })
    if ($again.Count -gt 0) { Write-Host "  listener  : pid $($l.Id) took a job while we wrote: NOT restarted"; $busyLeft += $d; continue }
    Stop-Process -Id $l.Id -Force
    $launcher = if (Test-Path -LiteralPath "$d\start-runner.cmd") { "$d\start-runner.cmd" } else { "$d\run.cmd" }
    Start-Process -FilePath "$env:SystemRoot\System32\cmd.exe" -ArgumentList @('/c', "`"$launcher`"") -WorkingDirectory $d -WindowStyle Hidden
    Write-Host "  listener  : pid $($l.Id) was idle: stopped, started again with $(Split-Path -Leaf $launcher)"
  }
}
if ($busyLeft.Count -gt 0) { Write-Host "`nBUSY, restart later: $($busyLeft -join ', ')" -ForegroundColor Yellow }
Write-Host "`ndone."
