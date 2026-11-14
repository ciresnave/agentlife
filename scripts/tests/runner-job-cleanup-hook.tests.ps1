# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Runs scripts\runner-job-cleanup-hook.ps1 as a real process against a fake runner directory: it must delete what a job leaves
  (workspace, _temp, cargo target/home/build dirs, caches) and keep the runner, and it must refuse a folder that is not a runner.
  Exit 0 = all passed. Run by tests\runner_cleanup_hook.rs on the Windows CI leg, or by hand.
#>
$ErrorActionPreference = 'Stop'
$hook = Join-Path (Split-Path -Parent $PSScriptRoot) 'runner-job-cleanup-hook.ps1'
$failures = New-Object System.Collections.Generic.List[string]
function Check([string]$name, [bool]$ok, [string]$detail = '') { if ($ok) { Write-Host "ok   $name" } else { Write-Host "FAIL $name $detail"; $failures.Add($name) } }
function New-File([string]$p) { New-Item -ItemType Directory -Force -Path (Split-Path -Parent $p) | Out-Null; Set-Content -LiteralPath $p -Value 'x' }
function Run-Hook([string]$dir) { Copy-Item -LiteralPath $hook -Destination "$dir\job-completed-cleanup.ps1" -Force; & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$dir\job-completed-cleanup.ps1" | Out-Null; $LASTEXITCODE }

$root = Join-Path ([IO.Path]::GetTempPath()) ('hook-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
try {
  $r = "$root\runner"; $outside = "$root\outside-target"
  foreach ($f in 'config.cmd', '.env', '.runner', '.credentials', 'bin\Runner.Listener.exe', 'externals\node\node.exe', '_diag\log.txt',
    '_work\_PipelineMapping\map.json', '_work\owner-repo\owner-repo\src\main.rs', '_work\_actions\a\b.txt', '_work\_tool\t\x.txt', '_work\_temp\step.ps1',
    'cargo-target\debug\big.bin', 'cargo-home\registry\crate.crate', 'cb\x.o', 'npm-cache\n.tgz', 'playwright-browsers\chromium\c.exe') { New-File "$r\$f" }
  New-File "$outside\precious.txt"
  & cmd.exe /c mklink /J "$r\_work\owner-repo\link" "$outside" | Out-Null
  $code = Run-Hook $r
  Check 'the hook exits 0' ($code -eq 0) "exit $code"
  foreach ($gone in '_work\owner-repo', '_work\_actions', '_work\_tool', '_work\_temp\step.ps1', 'cargo-target', 'cargo-home', 'cb', 'npm-cache', 'playwright-browsers') {
    Check "deleted $gone" (-not (Test-Path -LiteralPath "$r\$gone")) 
  }
  foreach ($kept in 'config.cmd', '.env', '.runner', '.credentials', 'bin\Runner.Listener.exe', 'externals\node\node.exe', '_diag\log.txt', '_work\_PipelineMapping\map.json', '_work', '_work\_temp') {
    Check "kept $kept" (Test-Path -LiteralPath "$r\$kept")
  }
  Check 'a junction inside the workspace is removed as a link: its target survives' (Test-Path -LiteralPath "$outside\precious.txt")
  Check 'a log line records the cleanup' ((Test-Path -LiteralPath "$r\cleanup.log") -and ((Get-Content -LiteralPath "$r\cleanup.log" -Raw) -match 'job-completed cleanup'))
  $code2 = Run-Hook $r
  Check 'running it again on a clean runner is harmless' ($code2 -eq 0)

  $notRunner = "$root\not-a-runner"; New-File "$notRunner\_work\keep.txt"; New-File "$notRunner\cargo-target\keep.bin"
  $code3 = Run-Hook $notRunner
  Check 'a folder with no config.cmd is refused: nothing deleted, exit 0' ($code3 -eq 0 -and (Test-Path -LiteralPath "$notRunner\_work\keep.txt") -and (Test-Path -LiteralPath "$notRunner\cargo-target\keep.bin"))
  Check 'the refusal is logged' ((Get-Content -LiteralPath "$notRunner\cleanup.log" -Raw) -match 'refused')
} finally { if (Test-Path -LiteralPath $root) { & cmd.exe /c rd /s /q "`"$root`"" | Out-Null } }
if ($failures.Count -gt 0) { Write-Host "`n$($failures.Count) FAILED: $($failures -join ', ')"; exit 1 }
Write-Host "`nall passed"
exit 0
