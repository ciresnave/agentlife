# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Writes the launcher task's wrapper, %USERPROFILE%\.agentlife-launcher\launcher-task.cmd (PM FIX toupv5-jds-lk, 2026-11-26:
  the wrapper was hand-written on 2026-11-13 and a hand edit is lost on regeneration, so the script is the source).

.DESCRIPTION
  DRY RUN BY DEFAULT: with no switch it prints the wrapper and changes nothing. -Write writes it (CRLF, ASCII), first keeping the
  existing file as launcher-task.cmd.bak-<UTC stamp>. It does not register or restart the task: the person who does that
  (`schtasks /End` does not stop the exe behind a wrapper, docs/LAUNCHER-SERVICE.md) reads the file back first.

  What the wrapper sets, because a scheduled task has no environment:
    - SYNAPSE_HOME, SYNAPSE_ADDR: the pilot daemon's home and address (as the hand-written wrapper did).
    - AGENTLIFE_LANE_REPORTING: the portfolio's lane-to-cluster-PM table (LANE-REPORTING.md). The launcher reads it to send the
      once-a-day daily-cap mail to a role's cluster PM (docs/LAUNCHER-CAP-DESIGN.md B); unset or unreadable, the mail goes to
      pm-platform with a note saying so. An ABSOLUTE path: the launcher has no working directory anyone can vouch for.

.PARAMETER Exe            The agentlife exe. Default %USERPROFILE%\.agentlife-launcher\bin\agentlife.exe.
.PARAMETER SynapseHome    SYNAPSE_HOME. Default %LOCALAPPDATA%\synapse-pilot (resolved when the wrapper RUNS, so it is written as the variable).
.PARAMETER Addr           SYNAPSE_ADDR. Default 127.0.0.1:7920.
.PARAMETER LaneReporting  AGENTLIFE_LANE_REPORTING. Default C:\Projects\LANE-REPORTING.md. Must be absolute and ASCII, with no % " & | < > ^.
.PARAMETER Out            Where the wrapper goes. Default %USERPROFILE%\.agentlife-launcher\launcher-task.cmd.
.PARAMETER Write          Write the file (otherwise print only).
#>
[CmdletBinding()]
param(
  [string]$Exe = '%USERPROFILE%\.agentlife-launcher\bin\agentlife.exe',
  [string]$SynapseHome = '%LOCALAPPDATA%\synapse-pilot',
  [string]$Addr = '127.0.0.1:7920',
  [string]$LaneReporting = 'C:\Projects\LANE-REPORTING.md',
  [string]$Out = (Join-Path $env:USERPROFILE '.agentlife-launcher\launcher-task.cmd'),
  [switch]$Write
)
$ErrorActionPreference = 'Stop'

if ($LaneReporting -notmatch '^([A-Za-z]:\\|\\\\)') { throw "LaneReporting '$LaneReporting' must be an absolute path (drive or UNC)" }
if ($LaneReporting -match '[%"&|<>^\r\n]') { throw 'LaneReporting holds a character the cmd wrapper cannot carry' }
if ($LaneReporting -match '[^\x20-\x7E]') { throw 'LaneReporting holds a non-ASCII character; the cmd wrapper is ASCII' }
if ($Addr -notmatch '^(127\.0\.0\.1|\[::1\]):(\d{1,5})$') { throw "Addr '$Addr' is not loopback host:port" }
foreach ($v in @($Exe, $SynapseHome)) {
  if ($v -match '["&|<>^\r\n]') { throw "'$v' holds a character the cmd wrapper cannot carry" }
}

$wrapper = @"
@echo off
rem Written by agentlife scripts\launcher-task-wrapper.ps1. Edit the script, not this file.
rem The scheduled task `agentlife-launcher` runs with no environment: this wrapper gives it the pilot daemon's home and
rem address, and the portfolio's lane-to-cluster-PM table (the once-a-day daily-cap mail goes to a role's cluster PM).
set "SYNAPSE_HOME=$SynapseHome"
set "SYNAPSE_ADDR=$Addr"
set "AGENTLIFE_LANE_REPORTING=$LaneReporting"
"$Exe" %* 2>>"%USERPROFILE%\.agentlife-launcher\launcher.log"
"@
$wrapper = ($wrapper -replace "`r?`n", "`r`n") + "`r`n"

if (-not $Write) {
  Write-Host "DRY RUN (nothing written). Pass -Write to write $Out`n"
  Write-Host $wrapper
  return
}
$dir = Split-Path -Parent $Out
if ($dir) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
if (Test-Path -LiteralPath $Out) {
  Copy-Item -LiteralPath $Out -Destination ($Out + '.bak-' + [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssfff')) -ErrorAction Stop
}
[IO.File]::WriteAllText($Out, $wrapper, [Text.Encoding]::ASCII)
$back = [IO.File]::ReadAllText($Out)
if ($back -ne $wrapper) { throw "read-back of $Out differs from what was written" }
Write-Host "wrote $Out (read back: identical). Restart the launcher task to pick it up; the previous file is kept as .bak-*"
