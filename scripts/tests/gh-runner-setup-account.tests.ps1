# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Tests the one-account-per-runner changes of scripts\gh-runner-setup.ps1 (docs/RUNNER-ACCOUNTS.md, tests T1, T2, T4, T5):
  the account name derived from the repo, the refusal to rotate an account that services already use, the listed-never-defaulted
  group flag, and a dry run that changes nothing. Fake service objects; no elevation, no account, no service touched.
  Exit 0 = all passed. Run by tests\runner_setup_account.rs on the Windows CI leg, or by hand:
  powershell -NoProfile -File <this file>
#>
$ErrorActionPreference = 'Stop'
$setup = Join-Path (Split-Path -Parent $PSScriptRoot) 'gh-runner-setup.ps1'
if (-not (Test-Path -LiteralPath $setup)) { throw "setup script not found: $setup" }
$ast = [Management.Automation.Language.Parser]::ParseFile($setup, [ref]$null, [ref]$null)
foreach ($name in 'Get-RunnerAccountName', 'Assert-AccountReusable', 'Test-GroupRequest') {
  $fn = $ast.FindAll({ param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq $name }, $true)
  if ($fn.Count -ne 1) { throw "expected exactly one $name in $setup, found $($fn.Count)" }
  . ([scriptblock]::Create($fn[0].Extent.Text))
}

$failures = New-Object System.Collections.Generic.List[string]
function Check([string]$name, [bool]$ok, [string]$detail = '') {
  if ($ok) { Write-Host "ok   $name" } else { Write-Host "FAIL $name $detail"; $failures.Add($name) }
}
function UserExists([string]$n) { & cmd.exe /c "net user $n >nul 2>nul"; return ($LASTEXITCODE -eq 0) }
function Throws([scriptblock]$b) { try { & $b | Out-Null; return $null } catch { return $_.Exception.Message } }

# T1: name derivation.
Check 'T1 ThinkersJournal/cpufish is ghr-cpufish' ((Get-RunnerAccountName 'ThinkersJournal/cpufish') -eq 'ghr-cpufish')
$long1 = Get-RunnerAccountName 'ThinkersJournal/ThinkersFoundation-site'
$long2 = Get-RunnerAccountName 'ThinkersJournal/ThinkersFoundation-sandbox'
Check 'T1 a long name is at most 20 characters' ($long1.Length -le 20 -and $long2.Length -le 20) "$long1 / $long2"
Check 'T1 a cut name is exactly 20 characters (15 + dash + 4 hex)' ($long1 -match '^ghr-[A-Za-z0-9_-]{11}-[0-9a-f]{4}$') $long1
Check 'T1 two long repos sharing a prefix get different names' ($long1 -ne $long2) "$long1 = $long2"
Check 'T1 deterministic' ($long1 -eq (Get-RunnerAccountName 'ThinkersJournal/ThinkersFoundation-site'))
Check 'T1 a dotted repo name is a plain local account name' ((Get-RunnerAccountName 'ciresnave/a.b.c') -match '^[A-Za-z0-9_-]{1,20}$')
Check 'T1 not owner/name is refused' ($null -ne (Throws { Get-RunnerAccountName 'justaname' }))
# Control: the check above has teeth. A 21-character name must read as too long.
Check 'T1 control: a 21-character name fails the length check' (-not (('ghr-' + ('x' * 17)).Length -le 20))

# T2: an existing account with dependents is refused unless -RotateShared; nothing is called to change it.
function FakeSvc([string]$n) { [pscustomobject]@{ Name = $n; StartName = '.\ghr-x' } }
$two = @((FakeSvc 'svc-a'), (FakeSvc 's-b'))
$msg = Throws { Assert-AccountReusable 'ghr-x' $two $false }
Check 'T2 an account with 2 services is refused' ($msg -like '*2 service(s)*svc-a*REFUSED*') $msg
Check 'T2 the refusal names the way out' ($msg -like '*-RotateShared*')
Check 'T2 -RotateShared lets it through' ($null -eq (Throws { Assert-AccountReusable 'ghr-x' $two $true }))
Check 'T2 an account with no services is not refused' ($null -eq (Throws { Assert-AccountReusable 'ghr-x' @() $false }))
Check 'T2 $null dependents is not refused' ($null -eq (Throws { Assert-AccountReusable 'ghr-x' $null $false }))
# The guard must run BEFORE the password is touched (nothing here can run Set-LocalUser, so read the order from the source).
function GuardFirst([string]$src) {
  $g = $src.IndexOf('Assert-AccountReusable $Account $dependents')
  $s = $src.IndexOf('Set-LocalUser -Name $Account -Password')
  return ($g -ge 0 -and $s -ge 0 -and $g -lt $s)
}
$text = Get-Content -LiteralPath $setup -Raw
Check 'T2 in the script the guard comes before Set-LocalUser -Password' (GuardFirst $text)
$noGuard = $text.Replace('Assert-AccountReusable $Account $dependents ([bool]$RotateShared)', '')
Check 'T2 mutation: removing the guard call is caught' (-not (GuardFirst $noGuard))
Check 'T2 control: both anchors exist exactly once' (([regex]::Matches($text, [regex]::Escape('Set-LocalUser -Name $Account -Password')).Count -eq 1) -and ([regex]::Matches($text, [regex]::Escape('Assert-AccountReusable $Account $dependents')).Count -eq 1))

# T4: groups are listed, never defaulted.
Check 'T4 no group requested gives none' ($null -eq (Test-GroupRequest '' ''))
$ok = Test-GroupRequest 'Hyper-V Administrators' '.github/workflows/machine-durability.yml'
Check 'T4 a named group with a reason is accepted' ($ok -and $ok.Group -eq 'Hyper-V Administrators' -and $ok.Because -like '*machine-durability.yml')
Check 'T4 the description fits the 48-character limit' ($ok.Description.Length -le 48) "len $($ok.Description.Length)"
Check 'T4 a group without -Because is refused' ($null -ne (Throws { Test-GroupRequest 'Hyper-V Administrators' '' }))
Check 'T4 -Because without a group is refused' ($null -ne (Throws { Test-GroupRequest '' 'because' }))
foreach ($bad in 'Administrators', 'administrators', 'Remote Desktop Users', 'Users', 'Everyone') {
  Check "T4 -AddToGroup $bad is refused" ($null -ne (Throws { Test-GroupRequest $bad 'x' }))
}
Check 'T4 a group name with a shell character is refused' ($null -ne (Throws { Test-GroupRequest 'a;b' 'x' }))

# T5: the dry run changes nothing, and says so. (The control is the account and root it names, which must not exist afterwards.)
$tmpRoot = Join-Path ([IO.Path]::GetTempPath()) ('ghr-t5-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
$acct = 'ghr-zzt5probe'
Check 'T5 control: the probe account does not exist before' (-not (UserExists $acct))
Check 'T5 control: the same query finds an account that does exist' (UserExists $env:USERNAME)
$dry = & { try { & $setup -Repo ciresnave/zzt5probe -ProtectedPaths @() -Root $tmpRoot -AddToGroup 'Hyper-V Administrators' -Because 'a workflow' } catch { 'THREW: ' + $_.Exception.Message } } *>&1 | Out-String
Check 'T5 the dry run is a dry run' ($dry -like '*DRY RUN*nothing changed*') $dry
Check 'T5 the plan names the derived account' ($dry -like "*$acct*" -or $dry -like '*ghr-zzt5probe*') $dry
Check 'T5 the plan says group memberships and why' ($dry -like '*group memberships: Hyper-V Administrators because a workflow*') $dry
$dry2 = & { try { & $setup -Repo ciresnave/zzt5probe -ProtectedPaths @() -Root $tmpRoot } catch { 'THREW: ' + $_.Exception.Message } } *>&1 | Out-String
Check 'T5 by default the plan says group memberships: none' ($dry2 -like '*group memberships: none*') $dry2
Check 'T5 the plan says an account with services is refused' ($dry2 -like '*REFUSED unless -RotateShared*') $dry2
Check 'T5 the dry run created no root directory' (-not (Test-Path -LiteralPath $tmpRoot))
Check 'T5 the dry run created no account' (-not (UserExists $acct))
$refused = & { try { & $setup -Repo ciresnave/zzt5probe -ProtectedPaths @() -Root $tmpRoot -AddToGroup Administrators -Because x } catch { 'THREW: ' + $_.Exception.Message } } *>&1 | Out-String
Check 'T5 -AddToGroup Administrators is refused even in a dry run' ($refused -like 'THREW:*refused*') $refused
$multi = & { try { & $setup -Repo ciresnave/a, ciresnave/b -ProtectedPaths @() -Root $tmpRoot } catch { 'THREW: ' + $_.Exception.Message } } *>&1 | Out-String
Check 'T5 two repos without -Account are refused (one account per runner)' ($multi -like 'THREW:*-Account is required*') $multi
$multiOk = & { try { & $setup -Repo ciresnave/a, ciresnave/b -Account ghr-shared -ProtectedPaths @() -Root $tmpRoot } catch { 'THREW: ' + $_.Exception.Message } } *>&1 | Out-String
Check 'T5 control: two repos WITH an explicit -Account plan fine' ($multiOk -like '*DRY RUN*' -and $multiOk -notlike '*THREW*') $multiOk

if ($failures.Count -gt 0) { Write-Host "`n$($failures.Count) FAILED: $($failures -join '; ')"; exit 1 }
Write-Host "`nall passed"
exit 0
