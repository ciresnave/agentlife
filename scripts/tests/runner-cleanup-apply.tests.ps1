# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Tests scripts\runner-cleanup-apply.ps1: Update-EnvText and Get-ListenerState (pure), then the script itself against a fake runner dir
  (with -NoRestart, so no process is touched). Exit 0 = all passed. Run by tests\runner_cleanup_hook.rs on Windows CI, or by hand.
#>
$ErrorActionPreference = 'Stop'
$applyPs1 = Join-Path (Split-Path -Parent $PSScriptRoot) 'runner-cleanup-apply.ps1'
$failures = New-Object System.Collections.Generic.List[string]
function Check([string]$name, [bool]$ok, [string]$detail = '') { if ($ok) { Write-Host "ok   $name" } else { Write-Host "FAIL $name $detail"; $failures.Add($name) } }
. $applyPs1 -Dir @() | Out-Null
$p = [ordered]@{ ACTIONS_RUNNER_HOOK_JOB_COMPLETED = 'C:\r\h.ps1'; CARGO_TARGET_DIR = 'C:\r\cargo-target' }
$crlf = "`r`n"
$t = Update-EnvText '' $p
Check 'an empty .env gets both lines, CRLF, ending in a newline' ($t -eq "ACTIONS_RUNNER_HOOK_JOB_COMPLETED=C:\r\h.ps1${crlf}CARGO_TARGET_DIR=C:\r\cargo-target${crlf}") $t
$t = Update-EnvText "PATH=C:\x${crlf}FOO=1" $p
Check 'a missing trailing newline is handled: the last line is not glued to the first appended one' ($t -eq "PATH=C:\x${crlf}FOO=1${crlf}ACTIONS_RUNNER_HOOK_JOB_COMPLETED=C:\r\h.ps1${crlf}CARGO_TARGET_DIR=C:\r\cargo-target${crlf}") $t
$t = Update-EnvText "A=1`nB=2`n" $p
Check 'an LF file stays LF' (($t -notmatch "`r") -and $t.StartsWith("A=1`nB=2`n"))
$once = Update-EnvText "PATH=C:\x${crlf}" $p
Check 'idempotent: applying twice changes nothing' ((Update-EnvText $once $p) -eq $once)
$t = Update-EnvText "cargo_target_dir=D:\old${crlf}PATH=C:\x${crlf}" $p
Check 'a wrong value of the same key (any case) is replaced in place, not duplicated' (($t -split "`r`n" | Where-Object { $_ -match '^(?i)cargo_target_dir=' }).Count -eq 1 -and $t -match 'CARGO_TARGET_DIR=C:\\r\\cargo-target' -and $t -match '^CARGO_TARGET_DIR=' ) $t
$t = Update-EnvText "CARGO_TARGET_DIR=D:\a${crlf}X=1${crlf}CARGO_TARGET_DIR=D:\b${crlf}" $p
Check 'a duplicated key is collapsed to one' (@($t -split "`r`n" | Where-Object { $_ -like 'CARGO_TARGET_DIR=*' }).Count -eq 1)
Check 'other lines are untouched and in order' ((Update-EnvText "# c${crlf}A=1${crlf}B=2${crlf}" $p).StartsWith("# c${crlf}A=1${crlf}B=2${crlf}"))

$old = Get-OldRunnerCopies @('bin', 'bin.2.338.0', 'externals', 'externals.2.338.2', '_work', 'bin.zip', 'bin.2.338', 'xbin.1.2.3', 'cargo-target')
Check 'only bin.<x.y.z> and externals.<x.y.z> are old copies; the live bin and externals are not' (($old -join ',') -eq 'bin.2.338.0,externals.2.338.2') ($old -join ',')
$procs = @(
  [pscustomobject]@{ Name = 'Runner.Listener'; Id = 10; Path = 'C:\actions-runners\h\bin\Runner.Listener.exe'; ParentId = 1 },
  [pscustomobject]@{ Name = 'Runner.Worker'; Id = 11; Path = 'C:\actions-runners\h\bin\Runner.Worker.exe'; ParentId = 10 },
  [pscustomobject]@{ Name = 'Runner.Listener'; Id = 20; Path = 'C:\actions-runners\h2\bin\Runner.Listener.exe'; ParentId = 1 },
  [pscustomobject]@{ Name = 'Runner.Listener'; Id = 30; Path = $null; ParentId = 1 })
$a = @(Get-ListenerState 'C:\actions-runners\h' $procs); $b = @(Get-ListenerState 'C:\actions-runners\h2' $procs)
Check 'a listener with a worker child is busy' ($a.Count -eq 1 -and $a[0].Busy -and $a[0].Id -eq 10)
Check 'a listener with no worker child is idle, and only this dir''s listener counts' ($b.Count -eq 1 -and -not $b[0].Busy -and $b[0].Id -eq 20)
Check 'a listener whose path is unreadable is attributed to no dir' (@(Get-ListenerState 'C:\actions-runners\zz' $procs).Count -eq 0)

$root = Join-Path ([IO.Path]::GetTempPath()) ('apply-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
try {
  $r = "$root\run1"; New-Item -ItemType Directory -Force -Path $r | Out-Null; Set-Content "$r\config.cmd" 'x'
  [IO.File]::WriteAllText("$r\.env", "PATH=C:\x${crlf}SECRET_TOKEN=hunter2")
  $dry = & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $applyPs1 -Dir $r 2>&1 | Out-String
  Check 'a dry run writes nothing' (-not (Test-Path "$r\job-completed-cleanup.ps1") -and ((Get-Content "$r\.env" -Raw) -eq "PATH=C:\x${crlf}SECRET_TOKEN=hunter2"))
  Check 'the output never prints the .env values' ($dry -notmatch 'hunter2' -and $dry -notmatch 'SECRET_TOKEN')
  $out = & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $applyPs1 -Dir $r -Apply -NoRestart 2>&1 | Out-String
  $env1 = Get-Content "$r\.env" -Raw
  Check 'apply copies the hook and sets both lines, keeping the secret line' ((Test-Path "$r\job-completed-cleanup.ps1") -and $env1 -match "(?m)^ACTIONS_RUNNER_HOOK_JOB_COMPLETED=" -and $env1 -match 'SECRET_TOKEN=hunter2' -and $env1 -match '(?m)^CARGO_TARGET_DIR=.*cargo-target') $env1
  Check 'the output never prints the .env values (apply)' ($out -notmatch 'hunter2')
  $before = Get-Content "$r\.env" -Raw
  & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $applyPs1 -Dir $r -Apply -NoRestart 2>&1 | Out-Null
  Check 'a second apply leaves the .env byte-identical' ((Get-Content "$r\.env" -Raw) -eq $before)
  $nr = "$root\notrunner"; New-Item -ItemType Directory -Force -Path $nr | Out-Null
  $o2 = & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $applyPs1 -Dir $nr -Apply -NoRestart 2>&1 | Out-String
  New-Item -ItemType Directory -Force -Path "$r\bin.9.9.9", "$r\externals.9.9.9", "$r\bin", "$r\externals" | Out-Null
  $dry2 = & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $applyPs1 -Dir $r 2>&1 | Out-String
  Check 'a dry run names the old copies and deletes none' ($dry2 -match 'would delete' -and (Test-Path "$r\bin.9.9.9"))
  & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $applyPs1 -Dir $r -Apply -NoRestart 2>&1 | Out-Null
  Check 'apply deletes the old copies and keeps the live bin and externals' (-not (Test-Path "$r\bin.9.9.9") -and -not (Test-Path "$r\externals.9.9.9") -and (Test-Path "$r\bin") -and (Test-Path "$r\externals"))
  Check 'a dir with no config.cmd is skipped and nothing is created in it' ($o2 -match 'SKIPPED' -and -not (Test-Path "$nr\.env"))
} finally { if (Test-Path $root) { & cmd.exe /c rd /s /q "`"$root`"" | Out-Null } }
if ($failures.Count -gt 0) { Write-Host "`n$($failures.Count) FAILED: $($failures -join ', ')"; exit 1 }
Write-Host "`nall passed"
exit 0
