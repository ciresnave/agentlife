<#
.SYNOPSIS
  Tests the ACL preflight of scripts\gh-runner-setup.ps1 (board 224) on real ACLs in a temp directory.
  Needs no elevation and changes nothing outside the temp directory. Exit 0 = all passed.
  Run by tests\runner_setup_acl.rs on the Windows CI leg, or by hand: powershell -NoProfile -File <this file>
#>
$ErrorActionPreference = 'Stop'
$setup = Join-Path (Split-Path -Parent $PSScriptRoot) 'gh-runner-setup.ps1'
if (-not (Test-Path -LiteralPath $setup)) { throw "setup script not found: $setup" }
$me = "$env:USERDOMAIN\$env:USERNAME"
$failures = New-Object System.Collections.Generic.List[string]

function Check([string]$name, [bool]$ok, [string]$detail = '') {
  if ($ok) { Write-Host "ok   $name" } else { Write-Host "FAIL $name $detail"; $failures.Add($name) }
}

# Runs the preflight on one path; returns @{ Passed; Text }. Write-Host output is on the information stream, so
# everything is merged (*>&1) and the throw is turned into a marker line inside the same capture.
function Preflight([string]$path, [switch]$DryRun) {
  $text = & {
    try {
      if ($DryRun) { & $setup -Repo ciresnave/x -ProtectedPaths $path }
      else { & $setup -Repo ciresnave/x -ProtectedPaths $path -CheckAclsOnly }
    } catch { "THREW: " + $_.Exception.Message }
  } *>&1 | Out-String
  return @{ Passed = ($text -notlike '*THREW:*'); Text = $text }
}

function NewDir([string]$name) {
  $d = Join-Path ([IO.Path]::GetTempPath()) ("ghrunner-acl-" + $name + "-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))
  New-Item -ItemType Directory -Path $d | Out-Null
  # Deterministic base: only this user, nothing inherited.
  icacls $d /inheritance:r /grant:r "${me}:(OI)(CI)F" | Out-Null
  if ($LASTEXITCODE -ne 0) { throw "icacls base on $d exited $LASTEXITCODE" }
  return $d
}

$made = @()
try {
  # Positive control: a directory only its owner can touch passes.
  $clean = NewDir 'clean'; $made += $clean
  $r = Preflight $clean
  Check 'a directory only the owner can write passes' $r.Passed $r.Text

  # The hole: each broad principal with each write-capable right is refused, naming the path and the fix.
  $cases = @(
    @('Authenticated Users', '*S-1-5-11', 'M'),
    @('Authenticated Users, write only', '*S-1-5-11', 'W'),
    @('Everyone', '*S-1-1-0', 'M'),
    @('Users', '*S-1-5-32-545', 'M'),
    @('Interactive', '*S-1-5-4', 'M'),
    @('Authenticated Users, full', '*S-1-5-11', 'F'),
    @('Authenticated Users, delete only', '*S-1-5-11', 'D')
  )
  foreach ($c in $cases) {
    $d = NewDir 'bad'; $made += $d
    icacls $d /grant "$($c[1]):(OI)(CI)$($c[2])" | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "icacls grant $($c[0]) exited $LASTEXITCODE" }
    $r = Preflight $d
    Check "$($c[0]) ($($c[2])) is refused" (-not $r.Passed -and $r.Text -like '*ACL preflight FAILED*') $r.Text
    Check "$($c[0]) ($($c[2])): the message names the path and the icacls fix" ($r.Text -like "*$d*" -and $r.Text -like '*icacls*/inheritance:r*') $r.Text
  }

  # Read-only for everyone is fine (the runner may read; it must not write).
  foreach ($c in @(@('Authenticated Users', '*S-1-5-11'), @('Everyone', '*S-1-1-0'), @('Users', '*S-1-5-32-545'))) {
    $d = NewDir 'readonly'; $made += $d
    icacls $d /grant "$($c[1]):(OI)(CI)RX" | Out-Null
    $r = Preflight $d
    Check "$($c[0]) read-and-execute only passes" $r.Passed $r.Text
  }

  # A file, not just a directory (with-secret.exe is a file).
  $d = NewDir 'file'; $made += $d
  $f = Join-Path $d 'tool.exe'
  Set-Content -LiteralPath $f -Value 'x'
  icacls $f /grant "*S-1-5-11:M" | Out-Null
  $r = Preflight $f
  Check 'a file writable by Authenticated Users is refused' (-not $r.Passed) $r.Text

  # A path that does not exist is skipped, not a failure.
  $r = Preflight (Join-Path ([IO.Path]::GetTempPath()) 'ghrunner-acl-does-not-exist')
  Check 'a missing path is skipped' $r.Passed $r.Text

  # The dry run reports but does not throw, and says it would refuse.
  $d = NewDir 'dry'; $made += $d
  icacls $d /grant "*S-1-5-11:(OI)(CI)M" | Out-Null
  $r = Preflight $d -DryRun
  Check 'the dry run does not throw on an exposed path' $r.Passed $r.Text
  Check 'the dry run says -Apply would be refused' ($r.Text -like '*WOULD REFUSE -Apply*') $r.Text
  $r = Preflight $clean -DryRun
  Check 'the dry run says the preflight is clean for a clean path' ($r.Text -like '*ACL preflight: clean*') $r.Text

  # -Apply without elevation never reaches account creation; with an exposed path it says so first when elevated.
  # (Elevation is not available here, so only the non-elevated refusal is asserted.)
  try { & $setup -Repo ciresnave/x -ProtectedPaths $clean -Apply 6>&1 | Out-Null; $applied = $true } catch { $applied = $false; $msg = $_.Exception.Message }
  $isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
  if (-not $isAdmin) { Check '-Apply is refused when not elevated' (-not $applied -and $msg -like '*elevated*') $msg }
  else { Write-Host 'skip -Apply refusal test: this shell is elevated and would really run' }
}
finally {
  foreach ($d in $made) { if (Test-Path -LiteralPath $d) { icacls $d /reset /t /c | Out-Null; Remove-Item -LiteralPath $d -Recurse -Force -ErrorAction SilentlyContinue } }
}

if ($failures.Count -gt 0) { Write-Host "`n$($failures.Count) FAILED: $($failures -join '; ')"; exit 1 }
Write-Host "`nall passed"
exit 0
