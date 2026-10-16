<#
.SYNOPSIS
  The Task Scheduler task that starts `synapsed` at logon (Synapse spec 2026-10-15-daemon-contract-and-pilot.md, section 1).

.DESCRIPTION
  DRY RUN BY DEFAULT: with no switch it prints the wrapper script and the task XML and changes nothing.
    -Register  writes the wrapper next to the logs and creates the task `synapsed` (replacing a same-named one).
    -Remove    deletes the task. It leaves the wrapper, the logs and the daemon's home alone.
  Per-user, "run only when the user is logged on" (InteractiveToken, LeastPrivilege): the keystore is
  owner-only, so no other account can run the daemon, and no password is stored or elevation needed
  (the same shape `agentlife install-task` was measured to create unelevated, docs/TASKS.md).

  The contract, and where each line of it lands here:
    - Fixed environment: SYNAPSE_HOME, SYNAPSE_ADDR, SYNAPSE_LAUNCHER_ROLES are SET in the wrapper, so a lane's
      environment never reaches the daemon. Every lane launch must pass the same SYNAPSE_HOME and SYNAPSE_ADDR.
    - Loopback only: -Addr is refused unless it is 127.0.0.1 or [::1] with a port (the daemon exits 2 otherwise).
    - Restart policy: Task Scheduler restarts a failed task at most 3 times, 1 minute apart. `synapsed` exits 2
      for a configuration or lock problem (not an address, not loopback, cannot bind, store error, second
      instance) and restarting cannot fix that, so the wrapper turns exit 2 into task exit 0 and writes the
      fact to the log. Any other non-zero exit is a crash and is restarted.
    - Single instance: a second `synapsed` on the same home exits 2, so a lane's auto-start and this task cannot
      both run; the later one stops harmlessly (and, by the rule above, is not restarted).
    - Logs: stderr is appended to <LogDir>\synapsed.log; the daemon never logs tokens or key material.
    - Liveness: `synapsectl list` against the same home. Never probe the port with bare curl.
  Logon, not startup: after a reboot with nobody logged in there is no daemon, which the spec accepts for a
  desktop lane host.

.PARAMETER Exe      Full path to synapsed.exe. Keep it beside synapsectl.exe and synapse-mcp-server.exe: the client looks next to its own executable first.
.PARAMETER SynapseHome     SYNAPSE_HOME. Default %LOCALAPPDATA%\synapse.
.PARAMETER Addr     SYNAPSE_ADDR. Default 127.0.0.1:7920.
.PARAMETER LauncherRoles  SYNAPSE_LAUNCHER_ROLES. Empty until the launch poller service is live; then `agentlife-launcher`.
.PARAMETER LogDir   Where synapsed.log and the wrapper go. Default <Home>\task.
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory)][string]$Exe,
  [string]$SynapseHome = (Join-Path $env:LOCALAPPDATA 'synapse'),
  [string]$Addr = '127.0.0.1:7920',
  [string]$LauncherRoles = '',
  [string]$LogDir,
  [switch]$Register,
  [switch]$Remove
)
$ErrorActionPreference = 'Stop'
$TaskName = 'synapsed'
if (-not $LogDir) { $LogDir = Join-Path $SynapseHome 'task' }

if ($Register -and $Remove) { throw '-Register and -Remove are exclusive' }
if ($Remove) {
  schtasks /Delete /TN $TaskName /F
  if ($LASTEXITCODE -ne 0) { throw "schtasks /Delete exited $LASTEXITCODE" }
  schtasks /Query /TN $TaskName 2>$null | Out-Null
  if ($LASTEXITCODE -eq 0) { throw "task $TaskName still exists after delete" }
  Write-Host "removed task $TaskName (read back: absent)"
  return
}

if ($Addr -notmatch '^(127\.0\.0\.1|\[::1\]):\d{1,5}$') {
  throw "Addr '$Addr' is not loopback host:port; synapsed exits 2 on anything else"
}
if ($LauncherRoles -match '[%"&|<>^\r\n]') { throw 'LauncherRoles holds a character the wrapper cannot carry' }
foreach ($p in @($Exe, $SynapseHome, $LogDir)) {
  if ($p -match '[%"&|<>^\r\n]') { throw "path '$p' holds a character the wrapper cannot carry" }
}
if (-not [IO.Path]::IsPathRooted($Exe)) { throw 'Exe must be a full path' }
if ($Register -and -not (Test-Path -LiteralPath $Exe -PathType Leaf)) { throw "Exe not found: $Exe" }

$wrapperPath = Join-Path $LogDir 'synapsed-run.cmd'
$logPath = Join-Path $LogDir 'synapsed.log'
$wrapper = @"
@echo off
rem Written by agentlife scripts\synapsed-task.ps1. Edit the script, not this file.
set "SYNAPSE_HOME=$SynapseHome"
set "SYNAPSE_ADDR=$Addr"
set "SYNAPSE_LAUNCHER_ROLES=$LauncherRoles"
"$Exe" 2>>"$logPath"
set "RC=%ERRORLEVEL%"
if "%RC%"=="2" (
  echo %DATE% %TIME% synapsed exited 2: refused to start, not restarting ^(see lines above^)>>"$logPath"
  exit /b 0
)
exit /b %RC%
"@
$wrapper = $wrapper -replace "`r?`n", "`r`n"

$user = "$env:USERDOMAIN\$env:USERNAME"
function Esc([string]$s) { [Security.SecurityElement]::Escape($s) }
$xml = @"
<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Start the Synapse daemon at logon with a fixed environment (agentlife scripts/synapsed-task.ps1)</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>$(Esc $user)</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>$(Esc $user)</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>3</Count>
    </RestartOnFailure>
    <Enabled>true</Enabled>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>$(Esc $wrapperPath)</Command>
    </Exec>
  </Actions>
</Task>
"@

if (-not $Register) {
  Write-Host "DRY RUN (nothing written, no task created). Pass -Register to apply.`n"
  Write-Host "--- $wrapperPath ---`n$wrapper`n"
  Write-Host "--- task '$TaskName' XML ---`n$xml"
  return
}

New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
[IO.File]::WriteAllText($wrapperPath, $wrapper, [Text.Encoding]::ASCII)
$xmlPath = Join-Path $LogDir 'synapsed-task.xml'
[IO.File]::WriteAllText($xmlPath, $xml, [Text.Encoding]::Unicode)
schtasks /Create /TN $TaskName /XML $xmlPath /F
if ($LASTEXITCODE -ne 0) { throw "schtasks /Create exited $LASTEXITCODE" }
# Read the task back; an exit code is not a state read.
$back = schtasks /Query /TN $TaskName /XML | Out-String
foreach ($needle in @('<LogonTrigger>', '<RestartOnFailure>', 'synapsed-run.cmd')) {
  if ($back -notlike "*$needle*") { throw "task read back without $needle" }
}
Write-Host "registered task $TaskName (read back: logon trigger, restart-on-failure, wrapper $wrapperPath)"
Write-Host "start it now with: schtasks /Run /TN $TaskName ; then check: synapsectl list (same SYNAPSE_HOME=$SynapseHome)"
