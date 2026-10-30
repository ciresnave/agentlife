#Requires -Version 7.0
<#
.SYNOPSIS
  Tests scripts\synapse-provision-weekly.ps1 against a fake `synapse` (a .cmd that prints a canned role list, records its
  arguments and the two environment variables it was given, and exits with a chosen code). Needs no Synapse, no network,
  no elevation; writes only to a temp directory. Exit 0 = all passed. Run by tests\synapse_provision_weekly.rs on the
  Windows CI leg, or by hand:
    pwsh -NoProfile -File scripts\tests\synapse-provision-weekly.tests.ps1
#>
$ErrorActionPreference = 'Stop'
$script = Join-Path (Split-Path -Parent $PSScriptRoot) 'synapse-provision-weekly.ps1'
if (-not (Test-Path -LiteralPath $script)) { throw "script not found: $script" }
$pwsh = (Get-Process -Id $PID).Path
$work = Join-Path ([IO.Path]::GetTempPath()) ("synprov-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $work | Out-Null
$failures = New-Object System.Collections.Generic.List[string]
function Check([string]$name, [bool]$ok, [string]$detail = '') {
  if ($ok) { Write-Host "ok   $name" } else { Write-Host "FAIL $name $detail"; $failures.Add($name) }
}

# fake synapse: prints out.txt, records args and env to rec.txt, exits with %FAKE_EXIT%
$fakeDir = Join-Path $work 'fake'; New-Item -ItemType Directory -Path $fakeDir | Out-Null
$fake = Join-Path $fakeDir 'synapse.cmd'
Set-Content -LiteralPath $fake -Encoding ascii -Value @(
  '@echo off',
  'echo ARGS=%*> "%~dp0rec.txt"',
  'echo HOME=%SYNAPSE_HOME%>> "%~dp0rec.txt"',
  'echo AUTO=%SYNAPSE_NO_AUTOSTART% >> "%~dp0rec.txt"',
  'type "%~dp0out.txt"',
  'exit /b %FAKE_EXIT%')

$acct = Join-Path $work 'account'; New-Item -ItemType Directory -Path $acct | Out-Null
$root = Join-Path $work 'lanes'; New-Item -ItemType Directory -Path $root | Out-Null
$roles = Join-Path $work 'roles.txt'
Set-Content -LiteralPath $roles -Value @('# soak', 'alpha', '', 'beta', 'gamma')
foreach ($r in 'alpha', 'beta', 'gamma') { New-Item -ItemType Directory -Path (Join-Path $root $r) | Out-Null }
Set-Content -LiteralPath (Join-Path $acct 'synapsed.json') -Value '{"addr":"x","instance_id":"abcdef12","pid":1}'
Set-Content -LiteralPath (Join-Path $root 'beta\synapsed.json') -Value 'STALE'

function Run([string]$outText, [int]$exit, [string[]]$extra = @(), [string]$rolesFile = $roles) {
  Set-Content -LiteralPath (Join-Path $fakeDir 'out.txt') -Value $outText
  Remove-Item -LiteralPath (Join-Path $fakeDir 'rec.txt') -ErrorAction SilentlyContinue
  $env:FAKE_EXIT = "$exit"
  $o = & $pwsh -NoProfile -File $script -RolesFile $rolesFile -LaneHomeRoot $root -SynapseHome $acct -SynapseExe $fake @extra 2>&1 | Out-String
  [pscustomobject]@{ Code = $LASTEXITCODE; Text = $o; Rec = (Get-Content -LiteralPath (Join-Path $fakeDir 'rec.txt') -Raw -ErrorAction SilentlyContinue) }
}
$allOk = "alpha ok`nbeta ok`ngamma ok"

# 1 dry run: no --apply to synapse, env set, nothing copied, exit 0
$r = Run $allOk 0
Check 'dry run exits 0' ($r.Code -eq 0) "code=$($r.Code) $($r.Text)"
Check 'dry run counts 3 ok' ($r.Text -match 'ok=3 created=0 renewed=0 refused=0 other=0')
Check 'dry run does not pass --apply' ($r.Rec -notmatch '--apply')
Check 'synapse gets SYNAPSE_HOME = the account home' ($r.Rec -match [regex]::Escape("HOME=$acct"))
Check 'synapse gets SYNAPSE_NO_AUTOSTART=1' ($r.Rec -match 'AUTO=1')
Check 'dry run announces would-copy=3 (alpha, gamma missing; beta stale)' ($r.Text -match 'would-copy=3')
Check 'dry run copied nothing' ((Get-Content -LiteralPath (Join-Path $root 'beta\synapsed.json') -Raw).Trim() -eq 'STALE' -and -not (Test-Path (Join-Path $root 'alpha\synapsed.json')))
Check 'the script cleans up its environment' (-not $env:SYNAPSE_NO_AUTOSTART)

# 2 apply: --apply passed, announce copied, a second apply finds all current
$r = Run $allOk 0 @('-Apply')
Check 'apply exits 0' ($r.Code -eq 0) "code=$($r.Code) $($r.Text)"
Check 'apply passes --apply' ($r.Rec -match '--apply')
Check 'apply copies the announce file' ($r.Text -match 'copied=3' -and (Get-Content -LiteralPath (Join-Path $root 'beta\synapsed.json') -Raw) -match 'abcdef12')
$r = Run $allOk 0 @('-Apply')
Check 'second apply finds every home current' ($r.Text -match 'current=3 copied=0')

# 3 counts of created and renewed
$r = Run "alpha created`nbeta renewed`ngamma ok" 0 @('-Apply')
Check 'created/renewed/ok are counted' ($r.Code -eq 0 -and $r.Text -match 'ok=1 created=1 renewed=1')

# 4 refused: exit 2, names the refused role, still exit 2 when synapse itself exits 2
$r = Run "alpha ok`nbeta refused`ngamma ok" 2 @('-Apply')
Check 'a refused role exits 2' ($r.Code -eq 2) "code=$($r.Code)"
Check 'a refused role is named' ($r.Text -match 'refused: beta' -and $r.Text -match 'refused=1')

# 5 fail closed: no output, wrong count, synapse failing without a refusal
$r = Run '' 0
Check 'no parseable output exits 1' ($r.Code -eq 1) "code=$($r.Code)"
$r = Run "alpha ok`nbeta ok" 0
Check 'a missing role line exits 1' ($r.Code -eq 1) "code=$($r.Code)"
$r = Run $allOk 7
Check 'synapse failing with no refusal exits 1' ($r.Code -eq 1) "code=$($r.Code)"

# 6 nothing sensitive or path-like is echoed: a hostile output line is not printed
$r = Run "alpha ok`nbeta ok`ngamma ok`nC:\secret\path\key.pem BEGIN PRIVATE KEY" 0
Check 'raw synapse output is never echoed' ($r.Text -notmatch 'secret|PRIVATE KEY|key\.pem')

# 7 unsafe role name: nothing is run
$bad = Join-Path $work 'bad.txt'; Set-Content -LiteralPath $bad -Value @('alpha', '..\evil')
Remove-Item -LiteralPath (Join-Path $fakeDir 'rec.txt') -ErrorAction SilentlyContinue
$r = Run $allOk 0 @() $bad
Check 'an unsafe role name exits 1 and runs nothing' ($r.Code -eq 1 -and -not $r.Rec) "code=$($r.Code)"

# 8 announce file missing
Rename-Item -LiteralPath (Join-Path $acct 'synapsed.json') -NewName 'gone'
$r = Run $allOk 0
Check 'a missing announce file exits 1' ($r.Code -eq 1 -and $r.Text -match 'announce: FAIL') "code=$($r.Code)"

Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
if ($failures.Count -gt 0) { Write-Host "$($failures.Count) FAILED: $($failures -join '; ')"; exit 1 }
Write-Host 'all passed'
exit 0
