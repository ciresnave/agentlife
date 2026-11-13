# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Tests Get-ConfigArgs of scripts\gh-runner-setup.ps1: the config.cmd argument list is one element per argument, and a real
  native call receives --replace and --ephemeral as two arguments. Nothing is registered. Exit 0 = all passed.
  Run by tests\runner_setup_ephemeral.rs on the Windows CI leg, or by hand: powershell -NoProfile -File <this file>
#>
$ErrorActionPreference = 'Stop'
$setup = Join-Path (Split-Path -Parent $PSScriptRoot) 'gh-runner-setup.ps1'
if (-not (Test-Path -LiteralPath $setup)) { throw "setup script not found: $setup" }
$ast = [Management.Automation.Language.Parser]::ParseFile($setup, [ref]$null, [ref]$null)
$fn = $ast.FindAll({ param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq 'Get-ConfigArgs' }, $true)
if ($fn.Count -ne 1) { throw "expected exactly one Get-ConfigArgs in $setup, found $($fn.Count)" }
. ([scriptblock]::Create($fn[0].Extent.Text))

$failures = New-Object System.Collections.Generic.List[string]
function Check([string]$name, [bool]$ok, [string]$detail = '') {
  if ($ok) { Write-Host "ok   $name" } else { Write-Host "FAIL $name $detail"; $failures.Add($name) }
}

$eph = @(Get-ConfigArgs 'ciresnave/x' 'TOK' 'HOST-ciresnave-x' @('--labels', 'a,b') $true 'ghr-x' 'pw')
Check 'ephemeral: --replace and --ephemeral are separate elements' (($eph -contains '--replace') -and ($eph -contains '--ephemeral')) ($eph -join '|')
Check 'ephemeral: no element holds a space' (@($eph | Where-Object { $_ -match '\s' }).Count -eq 0) ($eph -join '|')
Check 'ephemeral: --ephemeral follows --replace' ($eph.IndexOf('--ephemeral') -eq $eph.IndexOf('--replace') + 1)
Check 'labels, account and password are their own elements' (($eph -contains '--labels') -and ($eph -contains 'a,b') -and ($eph -contains '.\ghr-x') -and ($eph -contains 'pw'))
$per = @(Get-ConfigArgs 'ciresnave/x' 'TOK' 'HOST-ciresnave-x' @() $false 'ghr-x' 'pw')
Check 'persistent: no --ephemeral, --replace still present' (($per -notcontains '--ephemeral') -and ($per -contains '--replace')) ($per -join '|')
Check 'persistent: --replace is followed by --runasservice' ($per[$per.IndexOf('--replace') + 1] -eq '--runasservice')

# A real native call: the stub prints one line per argument it receives.
$tmp = Join-Path ([IO.Path]::GetTempPath()) ('ghephem-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $tmp -Force | Out-Null
try {
  $stub = Join-Path $tmp 'config.cmd'
  Set-Content -LiteralPath $stub -Encoding Ascii -Value @('@echo off', ':loop', 'if "%~1"=="" exit /b 0', 'echo ARG[%~1]', 'shift', 'goto loop')
  $a = Get-ConfigArgs 'ciresnave/x' 'TOK' 'HOST-ciresnave-x' @() $true 'ghr-x' 'pw'
  $got = @(& $stub @a)
  Check 'a native call receives --replace and --ephemeral as two arguments' (($got -contains 'ARG[--replace]') -and ($got -contains 'ARG[--ephemeral]') -and ($got -notcontains 'ARG[--replace --ephemeral]')) ($got -join '|')
} finally { Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue }

if ($failures.Count -gt 0) { Write-Host "`n$($failures.Count) FAILED: $($failures -join ', ')"; exit 1 }
Write-Host "`nall passed"
exit 0
