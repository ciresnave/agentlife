# SPDX-License-Identifier: MIT OR Apache-2.0
#Requires -Version 7.0
<#
.SYNOPSIS
  Tests scripts\launcher-task-wrapper.ps1: the generated launcher-task.cmd text (defaults, -LaneReporting, refused values),
  that a dry run writes nothing and -Write writes exactly the printed text. Needs no Synapse and no elevation; writes only to
  a temp directory. Exit 0 = all passed. Run by tests\launcher_task_wrapper.rs on the Windows CI leg, or by hand:
    pwsh -NoProfile -File scripts\tests\launcher-task-wrapper.tests.ps1
#>
$ErrorActionPreference = 'Stop'
$script = Join-Path (Split-Path -Parent $PSScriptRoot) 'launcher-task-wrapper.ps1'
if (-not (Test-Path -LiteralPath $script)) { throw "script not found: $script" }
$pwsh = (Get-Process -Id $PID).Path
$work = Join-Path ([IO.Path]::GetTempPath()) ("lauwrap-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $work | Out-Null
$failures = New-Object System.Collections.Generic.List[string]
function Check([string]$name, [bool]$ok, [string]$detail = '') {
  if ($ok) { Write-Host "ok   $name" } else { Write-Host "FAIL $name $detail"; $failures.Add($name) }
}
function Run([string[]]$extra) {
  $o = & $pwsh -NoProfile -NonInteractive -File $script @extra 2>&1 | Out-String
  [pscustomobject]@{ Text = $o; Code = $LASTEXITCODE }
}
$out = Join-Path $work 'launcher-task.cmd'

# 1. default: the portfolio table, absolute, is set in the wrapper; the exe line is unchanged in shape.
$r = Run @('-Out', $out)
Check 'dry run exits 0' ($r.Code -eq 0) $r.Text
Check 'default sets AGENTLIFE_LANE_REPORTING to the portfolio table' ($r.Text.Contains('set "AGENTLIFE_LANE_REPORTING=C:\Projects\LANE-REPORTING.md"')) $r.Text
Check 'it still sets SYNAPSE_ADDR and runs the exe with the log' (($r.Text.Contains('set "SYNAPSE_ADDR=127.0.0.1:7920"')) -and ($r.Text.Contains('agentlife.exe" %* 2>>'))) $r.Text
Check 'a dry run writes nothing' (-not (Test-Path -LiteralPath $out))

# 2. -LaneReporting overrides; -Write writes exactly the text (CRLF) and nothing else.
$table = Join-Path $work 'table.md'
$r = Run @('-Out', $out, '-LaneReporting', $table, '-Write')
Check '-Write exits 0' ($r.Code -eq 0) $r.Text
$written = [IO.File]::ReadAllText($out)
Check 'the file holds the override' ($written.Contains("set `"AGENTLIFE_LANE_REPORTING=$table`""))
Check 'the file is CRLF' ($written.Contains("`r`n") -and -not ($written -match "(?<!`r)`n"))
Check 'only one AGENTLIFE_LANE_REPORTING line' ((([regex]::Matches($written, 'AGENTLIFE_LANE_REPORTING')).Count) -eq 1)
Check 'an existing wrapper is backed up, not lost' ((Get-ChildItem -LiteralPath $work -Filter 'launcher-task.cmd.bak-*').Count -eq 0) 'first write: no backup expected'
$r = Run @('-Out', $out, '-Write')
Check 'a second write keeps a dated backup of the first' ((Get-ChildItem -LiteralPath $work -Filter 'launcher-task.cmd.bak-*').Count -eq 1) $r.Text

# 3. refused values: a relative path, a path the cmd wrapper cannot carry, non-ASCII.
foreach ($bad in @('LANE-REPORTING.md', '.\x.md', 'C:\a"b.md', 'C:\a%b.md', 'C:\a&b.md', ("C:\" + [char]0x00e9 + ".md"))) {
  $r = Run @('-Out', (Join-Path $work 'bad.cmd'), '-LaneReporting', $bad, '-Write')
  Check "refuses -LaneReporting '$bad'" (($r.Code -ne 0) -and -not (Test-Path -LiteralPath (Join-Path $work 'bad.cmd'))) $r.Text
}

Remove-Item -LiteralPath $work -Recurse -Force
if ($failures.Count -gt 0) { Write-Host "$($failures.Count) failed: $($failures -join '; ')"; exit 1 }
Write-Host 'all passed'
