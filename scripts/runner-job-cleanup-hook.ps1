# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  GitHub Actions runner job-completed hook (ACTIONS_RUNNER_HOOK_JOB_COMPLETED): after every job, delete everything the job
  wrote inside THIS runner's directory, so the next job (or the next registration) never starts on a dirty folder.
.DESCRIPTION
  Copied to <runner dir>\job-completed-cleanup.ps1 by gh-runner-setup.ps1, which also puts the hook path, CARGO_HOME,
  CARGO_TARGET_DIR and CARGO_BUILD_BUILD_DIR (all inside the runner dir) in the runner's .env. The runner dir is this
  script's own folder, never a parameter or the job's environment, so a job cannot aim the hook elsewhere.
  Deleted: the CONTENTS of _work (job workspaces, _actions, _tool) and of _work\_temp, and the folders cargo-target, cargo-home,
  cb, npm-cache, playwright-browsers. Kept: the runner itself (bin, externals, config.cmd, .env, .runner, .credentials, _diag)
  and _work\_PipelineMapping. Refuses to run unless the folder holds config.cmd and _work. `cmd /c rd /s /q` removes a junction
  as a link and never follows it. A failure is logged to cleanup.log and never fails the job (exit 0).
#>
$ErrorActionPreference = 'Continue'
$dir = $PSScriptRoot
$log = Join-Path $dir 'cleanup.log'
function Log([string]$m) { try { Add-Content -LiteralPath $log -Value ("{0:u} {1}" -f (Get-Date).ToUniversalTime(), $m) -Encoding Ascii } catch {} }
function Remove-Tree([string]$p) {
  if (-not (Test-Path -LiteralPath $p)) { return }
  if ((Get-Item -LiteralPath $p -Force).PSIsContainer) { & cmd.exe /c rd /s /q "`"$p`"" 2>&1 | Out-Null } else { Remove-Item -LiteralPath $p -Force -ErrorAction SilentlyContinue }
  if (Test-Path -LiteralPath $p) { Log "could not fully delete $p" }
}
if (-not ((Test-Path -LiteralPath "$dir\config.cmd") -and (Test-Path -LiteralPath "$dir\_work"))) {
  Log "refused: $dir holds no config.cmd and _work, so it is not a runner dir"
  exit 0
}
$started = Get-Date
$keep = @('_PipelineMapping', '_temp')
foreach ($c in @(Get-ChildItem -LiteralPath "$dir\_work" -Force -ErrorAction SilentlyContinue)) {
  if ($keep -contains $c.Name) { continue }
  Remove-Tree $c.FullName
}
if (Test-Path -LiteralPath "$dir\_work\_temp") {
  foreach ($c in @(Get-ChildItem -LiteralPath "$dir\_work\_temp" -Force -ErrorAction SilentlyContinue)) { Remove-Tree $c.FullName }
}
foreach ($n in 'cargo-target', 'cargo-home', 'cb', 'npm-cache', 'playwright-browsers') { Remove-Tree (Join-Path $dir $n) }
Log ("job-completed cleanup of {0} took {1:N1}s" -f $dir, ((Get-Date) - $started).TotalSeconds)
exit 0
