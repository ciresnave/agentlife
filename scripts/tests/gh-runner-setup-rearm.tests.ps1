# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Tests Get-RearmPlan of scripts\gh-runner-setup.ps1 (-Rearm: tear down a runner that has taken its one job, with no removal token).
  Real temp directories, fake service objects; nothing is stopped or deleted. Exit 0 = all passed.
  Run by tests\runner_setup_rearm.rs on the Windows CI leg, or by hand: powershell -NoProfile -File <this file>
#>
$ErrorActionPreference = 'Stop'
$setup = Join-Path (Split-Path -Parent $PSScriptRoot) 'gh-runner-setup.ps1'
if (-not (Test-Path -LiteralPath $setup)) { throw "setup script not found: $setup" }
$ast = [Management.Automation.Language.Parser]::ParseFile($setup, [ref]$null, [ref]$null)
foreach ($name in 'Find-RunnerService', 'Get-RearmPlan') {
  $fn = $ast.FindAll({ param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq $name }, $true)
  if ($fn.Count -ne 1) { throw "expected exactly one $name in $setup, found $($fn.Count)" }
  . ([scriptblock]::Create($fn[0].Extent.Text))
}

$failures = New-Object System.Collections.Generic.List[string]
function Check([string]$name, [bool]$ok, [string]$detail = '') {
  if ($ok) { Write-Host "ok   $name" } else { Write-Host "FAIL $name $detail"; $failures.Add($name) }
}
function Throws([scriptblock]$sb) { try { & $sb | Out-Null; $null } catch { $_.Exception.Message } }

$tmp = Join-Path ([IO.Path]::GetTempPath()) ('ghrearm-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
$root = Join-Path $tmp 'gh-runner-ghr-mlmf-corpus'
$dir = Join-Path $root 'ciresnave-mlmf'
New-Item -ItemType Directory -Path $dir -Force | Out-Null
try {
  Set-Content -LiteralPath "$dir\config.cmd" -Value '@echo off' -Encoding Ascii
  $svcName = 'actions.runner.ciresnave-mlmf.TUF-LAPTOP-ciresnave-mlmf'
  $mine = [pscustomobject]@{ Name = $svcName; PathName = "`"$dir\bin\RunnerService.exe`""; StartName = '.\ghr-mlmf-corpus' }

  $p = Get-RearmPlan $root $dir 'ghr-mlmf-corpus' 'TUF-LAPTOP' @($mine)
  Check 'plans the service found by directory and the directory' ($p.Service -eq $svcName -and $p.Dir -eq $dir) "$($p.Service) $($p.Dir)"
  $p = Get-RearmPlan $root "$dir\" 'ghr-mlmf-corpus' 'TUF-LAPTOP' @()
  Check 'a runner whose service is already gone still plans (directory only)' ($null -eq $p.Service -and $p.Dir -eq $dir)
  $m2 = [pscustomobject]@{ Name = $svcName; PathName = $mine.PathName; StartName = 'TUF-LAPTOP\ghr-mlmf-corpus' }
  Check 'the computer\account spelling of the logon is accepted' ((Get-RearmPlan $root $dir 'ghr-mlmf-corpus' 'TUF-LAPTOP' @($m2)).Service -eq $svcName)

  $other = [pscustomobject]@{ Name = $svcName; PathName = $mine.PathName; StartName = '.\ghr-other' }
  $e = Throws { Get-RearmPlan $root $dir 'ghr-mlmf-corpus' 'TUF-LAPTOP' @($other) }
  Check 'a service logging on as another account is refused, not deleted' ($e -match 'not ghr-mlmf-corpus') $e

  $e = Throws { Get-RearmPlan $root $tmp 'ghr-mlmf-corpus' 'TUF-LAPTOP' @() }
  Check 'the root itself is never planned for deletion' ($e -match 'not a direct child') $e
  $e = Throws { Get-RearmPlan $root (Join-Path $tmp 'elsewhere') 'ghr-mlmf-corpus' 'TUF-LAPTOP' @() }
  Check 'a directory outside the root is refused' ($e -match 'not a direct child') $e
  $e = Throws { Get-RearmPlan $root "$dir\..\..\elsewhere" 'ghr-mlmf-corpus' 'TUF-LAPTOP' @() }
  Check 'a .. path that leaves the root is refused' ($e -match 'not a direct child') $e
  $e = Throws { Get-RearmPlan $root "$dir\bin" 'ghr-mlmf-corpus' 'TUF-LAPTOP' @() }
  Check 'a grandchild of the root is refused' ($e -match 'not a direct child') $e
  $sib = Join-Path $tmp 'gh-runner-ghr-mlmf-corpus-2\x'
  New-Item -ItemType Directory -Path $sib -Force | Out-Null
  $e = Throws { Get-RearmPlan $root $sib 'ghr-mlmf-corpus' 'TUF-LAPTOP' @() }
  Check 'a sibling root sharing the name prefix is refused' ($e -match 'not a direct child') $e

  Remove-Item -LiteralPath "$dir\config.cmd"
  $e = Throws { Get-RearmPlan $root $dir 'ghr-mlmf-corpus' 'TUF-LAPTOP' @($mine) }
  Check 'a directory with no config.cmd is not a runner: refused' ($e -match 'no config.cmd') $e
  $e = Throws { Get-RearmPlan $root (Join-Path $root 'absent') 'ghr-mlmf-corpus' 'TUF-LAPTOP' @() }
  Check 'an absent directory is refused' ($e -match 'no config.cmd') $e
} finally { Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue }

if ($failures.Count -gt 0) { Write-Host "`n$($failures.Count) FAILED: $($failures -join ', ')"; exit 1 }
Write-Host "`nall passed"
exit 0
