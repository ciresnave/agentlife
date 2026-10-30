#Requires -Version 7.0
<#
.SYNOPSIS
  Weekly Synapse lane-certificate renewal: runs `synapse id provision` for the soak roles and re-copies the daemon
  announce file into every lane home. Certificates last 30 days and a lane cannot renew its own, so something with
  the account key has to run this more often than the renew-below window (14 days); until now nothing did.

.DESCRIPTION
  DRY RUN BY DEFAULT: with no switch nothing on disk changes. -Apply acts.

  Step 1  `synapse id provision --roles-file F --lane-home-root R [--apply]`, with SYNAPSE_HOME = the account home
          and SYNAPSE_NO_AUTOSTART=1 (this script must never start a daemon). Its `<role> <status>` lines are
          parsed and counted (ok / created / renewed / refused / other). Raw output is NOT echoed: only counts and
          the names of refused roles are printed. No key, certificate or path is printed.
  Step 2  Re-copies <account home>\synapsed.json into <lane home>\<role>\ (same logic as tools\synapse-announce-sync.sh).
          The file changes on every daemon start; a role-only home has none, and the lane's MCP adapter then cannot
          find the daemon. Needed until synapse#298 is installed everywhere. Dry run counts what would be copied.

  Exit 0   everything fine.
  Exit 2   at least one role was refused.
  Exit 1   anything else that went wrong: synapse failed or printed nothing parseable, a role name is unsafe, the
           announce file is missing or a copy failed.

  Does NOT register a Scheduled Task: that waits for CireSnave's go.

.EXAMPLE
  pwsh -File scripts\synapse-provision-weekly.ps1          # dry run
  pwsh -File scripts\synapse-provision-weekly.ps1 -Apply   # act
#>
[CmdletBinding()]
param(
  [string]$RolesFile = 'C:\Projects\.lane-state\roles-soak.txt',
  [string]$LaneHomeRoot = (Join-Path $env:LOCALAPPDATA 'synapse-lanes'),
  [string]$SynapseHome = (Join-Path $env:LOCALAPPDATA 'synapse-pilot'),
  [string]$SynapseExe = 'synapse',
  [switch]$Apply
)
$ErrorActionPreference = 'Stop'
$mode = if ($Apply) { 'APPLY' } else { 'DRY RUN' }
Write-Host "synapse-provision-weekly: $mode"

$problems = 0
$refused = New-Object System.Collections.Generic.List[string]
$counts = [ordered]@{ ok = 0; created = 0; renewed = 0; refused = 0; other = 0 }

# --- roles: one per line, blank lines and # comments skipped. A name that could leave the lane-home root is refused.
if (-not (Test-Path -LiteralPath $RolesFile -PathType Leaf)) { Write-Host 'FAIL: roles file not found'; exit 1 }
$roles = @()
foreach ($line in Get-Content -LiteralPath $RolesFile) {
  $t = $line.Trim()
  if ($t -eq '' -or $t.StartsWith('#')) { continue }
  if ($t -notmatch '^[A-Za-z0-9][A-Za-z0-9_.-]*$' -or $t -match '\.\.') {
    Write-Host 'FAIL: the roles file holds an unsafe role name; nothing was run'
    exit 1
  }
  $roles += $t
}
if ($roles.Count -eq 0) { Write-Host 'FAIL: the roles file holds no roles'; exit 1 }

# --- step 1: provision
$synArgs = @('id', 'provision', '--roles-file', $RolesFile, '--lane-home-root', $LaneHomeRoot)
if ($Apply) { $synArgs += '--apply' }
$oldHome = $env:SYNAPSE_HOME
$oldAuto = $env:SYNAPSE_NO_AUTOSTART
$env:SYNAPSE_HOME = $SynapseHome
$env:SYNAPSE_NO_AUTOSTART = '1'
$synExit = $null
$out = @()
try {
  $out = @(& $SynapseExe @synArgs 2>&1 | ForEach-Object { "$_" })
  $synExit = $LASTEXITCODE
} catch {
  $synExit = -1
} finally {
  $env:SYNAPSE_HOME = $oldHome
  $env:SYNAPSE_NO_AUTOSTART = $oldAuto
}

$parsed = 0
foreach ($l in $out) {
  if ($l -match '^(?<role>[A-Za-z0-9][A-Za-z0-9_.-]*)\s+(?<status>[a-z][a-z-]*)\s*$') {
    $parsed++
    $s = $Matches['status']
    if ($counts.Contains($s)) { $counts[$s]++ } else { $counts['other']++ }
    if ($s -eq 'refused') { $refused.Add($Matches['role']) }
  }
}
Write-Host ("provision: ok={0} created={1} renewed={2} refused={3} other={4}" -f $counts.ok, $counts.created, $counts.renewed, $counts.refused, $counts.other)
foreach ($r in $refused) { Write-Host "refused: $r" }
if ($parsed -eq 0) { Write-Host 'FAIL: synapse printed no parseable role lines'; $problems++ }
elseif ($parsed -ne $roles.Count) { Write-Host "FAIL: expected $($roles.Count) role lines, parsed $parsed"; $problems++ }
if ($synExit -ne 0 -and $refused.Count -eq 0) { Write-Host "FAIL: synapse exited $synExit with no refused role"; $problems++ }

# --- step 2: announce file into every lane home
$src = Join-Path $SynapseHome 'synapsed.json'
if (-not (Test-Path -LiteralPath $src -PathType Leaf)) {
  Write-Host 'announce: FAIL: the daemon announce file is missing (daemon never started?)'
  $problems++
} else {
  $srcHash = (Get-FileHash -LiteralPath $src -Algorithm SHA256).Hash
  $same = 0; $copied = 0; $nohome = 0; $bad = 0
  foreach ($r in $roles) {
    $dir = Join-Path $LaneHomeRoot $r
    if (-not (Test-Path -LiteralPath $dir -PathType Container)) { $nohome++; continue }
    $dst = Join-Path $dir 'synapsed.json'
    if ((Test-Path -LiteralPath $dst -PathType Leaf) -and ((Get-FileHash -LiteralPath $dst -Algorithm SHA256).Hash -eq $srcHash)) { $same++; continue }
    if (-not $Apply) { $copied++; continue }
    try { Copy-Item -LiteralPath $src -Destination $dst -Force; $copied++ } catch { $bad++ }
  }
  $verb = if ($Apply) { 'copied' } else { 'would-copy' }
  Write-Host ("announce: current={0} {1}={2} no-home={3} failed={4}" -f $same, $verb, $copied, $nohome, $bad)
  # A missing home is only a problem under -Apply: a dry run before the first provision legitimately has none.
  if ($bad -gt 0 -or ($Apply -and $nohome -gt 0)) { $problems++ }
}

if ($refused.Count -gt 0) { exit 2 }
if ($problems -gt 0) { exit 1 }
exit 0
