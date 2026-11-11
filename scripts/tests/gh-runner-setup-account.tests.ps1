# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Tests the one-account-per-runner changes of scripts\gh-runner-setup.ps1 (docs/RUNNER-ACCOUNTS.md, tests T1 to T5):
  the account name derived from the repo, the refusal to rotate an account that services already use, the listed-never-defaulted
  group flag, and a dry run that changes nothing. Fake service objects; no elevation, no account, no service touched.
  Exit 0 = all passed. Run by tests\runner_setup_account.rs on the Windows CI leg, or by hand:
  powershell -NoProfile -File <this file>
#>
$ErrorActionPreference = 'Stop'
$setup = Join-Path (Split-Path -Parent $PSScriptRoot) 'gh-runner-setup.ps1'
if (-not (Test-Path -LiteralPath $setup)) { throw "setup script not found: $setup" }
$ast = [Management.Automation.Language.Parser]::ParseFile($setup, [ref]$null, [ref]$null)
foreach ($name in 'Get-RunnerAccountName', 'Assert-AccountReusable', 'Test-GroupRequest', 'Get-RootForeignPrincipals', 'Set-RunnerRootAcl', 'Get-SharedRootRunners', 'New-RootMigrationPlan', 'Get-AccountRemovalPlan') {
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

# T3: a root per account, on real ACLs in a temp directory. Two built-in groups stand in for two runner accounts (an account cannot be
# made without elevation); the functions take the principal as text, so the stand-ins exercise the same code.
$me = "$env:USERDOMAIN\$env:USERNAME"
$pa = 'BUILTIN\Backup Operators'; $pb = 'BUILTIN\Replicator'
$base = Join-Path ([IO.Path]::GetTempPath()) ('ghr-t3-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
$rootA = Join-Path $base 'gh-runner-a'; $rootB = Join-Path $base 'gh-runner-b'
New-Item -ItemType Directory -Force -Path $rootA, $rootB | Out-Null
try {
  Set-RunnerRootAcl $rootA $pa
  Set-RunnerRootAcl $rootB $pb
  $fa = @(Get-RootForeignPrincipals $rootA $pa); $fb = @(Get-RootForeignPrincipals $rootB $pb)
  Check 'T3 root A grants no principal besides SYSTEM, Administrators and A' ($fa.Count -eq 0) ($fa -join ',')
  Check 'T3 root B grants no principal besides SYSTEM, Administrators and B' ($fb.Count -eq 0) ($fb -join ',')
  $crossAB = @(Get-RootForeignPrincipals $rootA $pb); $crossBA = @(Get-RootForeignPrincipals $rootB $pa)
  Check 'T3 seen from B, root A holds a stranger (A)' ($crossAB.Count -eq 1 -and $crossAB[0] -like '*Backup Operators') ($crossAB -join ',')
  Check 'T3 seen from A, root B holds a stranger (B)' ($crossBA.Count -eq 1 -and $crossBA[0] -like '*Replicator') ($crossBA -join ',')
  $out = (icacls $rootA) -join "`n"
  Check 'T3 control: icacls shows A on root A' ($out -like '*Backup Operators*') $out
  Check 'T3 neither root lists the other runner' ($out -notlike '*Replicator*' -and ((icacls $rootB) -join "`n") -notlike '*Backup Operators*')
  # Mutation: the shared-root grant the old script made (a second account added to the same root) is caught.
  Set-RunnerRootAcl $rootA $pa
  icacls $rootA /grant "${pb}:(OI)(CI)M" | Out-Null
  $shared = @(Get-RootForeignPrincipals $rootA $pa)
  Check 'T3 mutation: a second account granted on the same root is reported' ($shared.Count -eq 1 -and $shared[0] -like '*Replicator') ($shared -join ',')
  # The person running the script is not a runner account: an entry for them (as CI's runneradmin gets on a fresh directory) is not foreign.
  icacls $rootA /grant "${me}:(OI)(CI)F" | Out-Null
  Check 'T3 an entry for the invoking user is not reported' (@(Get-RootForeignPrincipals $rootA $pa | Where-Object { $_ -like "*$env:USERNAME" }).Count -eq 0)
  Check 'T3 a root that does not exist has no foreign principal' (@(Get-RootForeignPrincipals (Join-Path $base 'nope') $pa).Count -eq 0)
} finally {
  foreach ($d in $rootA, $rootB) { icacls $d /grant "${me}:(OI)(CI)F" 2>&1 | Out-Null }
  Remove-Item -LiteralPath $base -Recurse -Force -ErrorAction SilentlyContinue
}

# T6 (slice 3): the printed migration of a shared root. Fake services decide who runs where; the plan is then EXECUTED on real ACLs in a
# temp directory built the way the old script built it (every account Modify on the root, inherited into every runner directory).
$svc = {
  param($n, $dir, $start) [pscustomobject]@{ Name = $n; PathName = "`"$dir\bin\RunnerService.exe`""; StartName = $start }
}
$sharedRoot = Join-Path ([IO.Path]::GetTempPath()) ('ghr-t6-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
$dA = Join-Path $sharedRoot 'owner-a'; $dB = Join-Path $sharedRoot 'owner-b'; $dOther = Join-Path ([IO.Path]::GetTempPath()) 'elsewhere'
$fake = @((& $svc 'svc-a' $dA '.\Backup Operators'), (& $svc 'svc-b' $dB 'BUILTIN\Replicator'), (& $svc 'svc-x' $dOther '.\someone'))
$bad = @($fake) + (& $svc 'svc-sys' (Join-Path $sharedRoot 'owner-c') 'LocalSystem')
$runners = @(Get-SharedRootRunners $sharedRoot $fake)
Check 'T6 two runner directories are found under the root' ($runners.Count -eq 2) ($runners | Out-String)
Check 'T6 a service outside the root is not listed' (@($runners | Where-Object Service -eq 'svc-x').Count -eq 0)
Check 'T6 each directory maps to its own account' (($runners | Where-Object Dir -eq $dA).Account -eq 'Backup Operators' -and ($runners | Where-Object Dir -eq $dB).Account -eq 'Replicator')
Check 'T6 a service logging on as LocalSystem is refused, not guessed' ($null -ne (Throws { Get-SharedRootRunners $sharedRoot $bad }))
$plan = @(New-RootMigrationPlan $sharedRoot 'BUILTIN' $runners $me)
$dirCmds = @($plan | Where-Object { $_.Path -ne $sharedRoot })
Check 'T6 no directory command grants an account on another account''s directory' (-not (@($dirCmds | Where-Object { ($_.Path -eq $dA -and ($_.Args -join ' ') -like '*Replicator*') -or ($_.Path -eq $dB -and ($_.Args -join ' ') -like '*Backup Operators*') }).Count))
Check 'T6 directories come before the root commands' ($plan[0].Path -ne $sharedRoot -and $plan[1].Path -ne $sharedRoot -and $plan[$plan.Count - 1].Path -eq $sharedRoot)
Check 'T6 the plan removes each runner account from the root' ((@($plan | Where-Object { $_.Path -eq $sharedRoot -and $_.Args[0] -eq '/remove' }).Count) -eq 2)
Check 'T6 the plan itself runs nothing: the shared root does not exist yet' (-not (Test-Path -LiteralPath $sharedRoot))
try {
  New-Item -ItemType Directory -Force -Path $dA, $dB | Out-Null
  icacls $sharedRoot /inheritance:r /grant:r "${me}:(OI)(CI)F" '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' | Out-Null
  foreach ($p in 'BUILTIN\Backup Operators', 'BUILTIN\Replicator') { icacls $sharedRoot /grant "${p}:(OI)(CI)M" | Out-Null }
  icacls $dA /reset /T | Out-Null; icacls $dB /reset /T | Out-Null
  $before = ((icacls $dA) -join "`n")
  Check 'T6 control: before, directory A lists BOTH accounts (the old shared-root hole)' ($before -like '*Backup Operators*' -and $before -like '*Replicator*') $before
  foreach ($c in $plan) { & icacls $c.Path @($c.Args) | Out-Null; if ($LASTEXITCODE -ne 0) { throw "icacls $($c.Path) $($c.Args -join ' ') exited $LASTEXITCODE" } }
  $a = (icacls $dA) -join "`n"; $b = (icacls $dB) -join "`n"; $r = (icacls $sharedRoot) -join "`n"
  Check 'T6 after, A lists only A' ($a -like '*Backup Operators*' -and $a -notlike '*Replicator*') $a
  Check 'T6 after, B lists only B' ($b -like '*Replicator*' -and $b -notlike '*Backup Operators*') $b
  Check 'T6 after, the root lists no runner account' ($r -notlike '*Backup Operators*' -and $r -notlike '*Replicator*') $r
  Check 'T6 after, no foreign principal is left on either directory' (@(Get-RootForeignPrincipals $dA 'BUILTIN\Backup Operators').Count -eq 0 -and @(Get-RootForeignPrincipals $dB 'BUILTIN\Replicator').Count -eq 0)
} finally {
  foreach ($d in $dA, $dB, $sharedRoot) { icacls $d /grant "${me}:(OI)(CI)F" 2>&1 | Out-Null }
  Remove-Item -LiteralPath $sharedRoot -Recurse -Force -ErrorAction SilentlyContinue
}
$mig = & { try { & $setup -Repo ciresnave/x -ProtectedPaths @() -PrintMigration } catch { 'THREW: ' + $_.Exception.Message } } *>&1 | Out-String
Check 'T6 -PrintMigration without -Root is refused' ($mig -like 'THREW:*needs -Root*') $mig

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
Check 'T5 the default root is C:\gh-runner-<account>, not the shared C:\gh-runner' ((& { try { & $setup -Repo ciresnave/zzt5probe -ProtectedPaths @() } catch { 'THREW: ' + $_.Exception.Message } } *>&1 | Out-String) -like '*C:\gh-runner-ghr-zzt5probe*own tree only*')
Check 'T5 the plan says no other account has an entry on a fresh root' ($dry2 -like '*root plan: create or reuse*')
Check 'T5 the dry run created no root directory' (-not (Test-Path -LiteralPath $tmpRoot))
Check 'T5 the dry run created no account' (-not (UserExists $acct))
$refused = & { try { & $setup -Repo ciresnave/zzt5probe -ProtectedPaths @() -Root $tmpRoot -AddToGroup Administrators -Because x } catch { 'THREW: ' + $_.Exception.Message } } *>&1 | Out-String
Check 'T5 -AddToGroup Administrators is refused even in a dry run' ($refused -like 'THREW:*refused*') $refused
$multi = & { try { & $setup -Repo ciresnave/a, ciresnave/b -ProtectedPaths @() -Root $tmpRoot } catch { 'THREW: ' + $_.Exception.Message } } *>&1 | Out-String
Check 'T5 two repos without -Account are refused (one account per runner)' ($multi -like 'THREW:*-Account is required*') $multi
$multiOk = & { try { & $setup -Repo ciresnave/a, ciresnave/b -Account ghr-shared -ProtectedPaths @() -Root $tmpRoot } catch { 'THREW: ' + $_.Exception.Message } } *>&1 | Out-String
Check 'T5 control: two repos WITH an explicit -Account plan fine' ($multiOk -like '*DRY RUN*' -and $multiOk -notlike '*THREW*') $multiOk

# T7: -RemoveAccount removes the account only when no other service logs on as it (fake service objects; nothing is removed here).
function SvcAs([string]$n, [string]$who) { [pscustomobject]@{ Name = $n; StartName = $who } }
$pc = 'BOX'
$none = Get-AccountRemovalPlan 'ghr-x' @((SvcAs 'other' '.\ghr-y'), (SvcAs 'sys' 'LocalSystem')) $pc 'cires'
Check 'T7 no service logs on as it: the account is removed' ($none.Remove -and $none.Dependents.Count -eq 0) ($none | Out-String)
$one = Get-AccountRemovalPlan 'ghr-x' @((SvcAs 'svc-a' '.\ghr-x')) $pc 'cires'
Check 'T7 control: one service logs on as it (.\ form): the account is kept and the service named' (-not $one.Remove -and $one.Reason -like '*svc-a*')
$two = Get-AccountRemovalPlan 'ghr-x' @((SvcAs 'svc-a' 'BOX\ghr-x'), (SvcAs 'svc-b' '.\GHR-X')) $pc 'cires'
Check 'T7 the BOXccount form and a different case both count' (-not $two.Remove -and $two.Dependents.Count -eq 2)
Check 'T7 $null and an empty service list remove it' ((Get-AccountRemovalPlan 'ghr-x' $null $pc 'cires').Remove -and (Get-AccountRemovalPlan 'ghr-x' @() $pc 'cires').Remove)
Check 'T7 a prefix of another account name is not a dependent' ((Get-AccountRemovalPlan 'ghr-x' @((SvcAs 'svc' '.\ghr-xy')) $pc 'cires').Remove)
foreach ($bad in 'Administrator', 'Guest', 'DefaultAccount', 'WDAGUtilityAccount') {
  Check "T7 built-in $bad is refused" ($null -ne (Throws { Get-AccountRemovalPlan $bad @() $pc 'cires' }))
}
Check 'T7 the account running the script is refused' ($null -ne (Throws { Get-AccountRemovalPlan 'cires' @() $pc 'cires' }))
Check 'T7 -RemoveAccount without -Remove is refused' ((& { try { & $setup -Repo ciresnave/zzt7probe -RemoveAccount -ProtectedPaths @() } catch { 'THREW: ' + $_.Exception.Message } } *>&1 | Out-String) -like 'THREW:*-RemoveAccount needs -Remove*')
Check 'T7 -Remove -RemoveAccount of a built-in is refused in the dry run, before any runner is touched' ((& { try { & $setup -Repo ciresnave/zzt7probe -Account Guest -Remove -RemoveAccount -ProtectedPaths @() } catch { 'THREW: ' + $_.Exception.Message } } *>&1 | Out-String) -like 'THREW:*built-in*')
$dry7 = & { try { & $setup -Repo ciresnave/zzt7probe -Remove -RemoveAccount -ProtectedPaths @() } catch { 'THREW: ' + $_.Exception.Message } } *>&1 | Out-String
Check 'T7 the -Remove -RemoveAccount dry run states the condition' ($dry7 -like '*DRY RUN*' -and $dry7 -like '*ONLY if no other service logs on as it*') $dry7
Check 'T7 control: -Remove alone does not mention removing the account' ((& { & $setup -Repo ciresnave/zzt7probe -Remove -ProtectedPaths @() } *>&1 | Out-String) -notlike '*ONLY if no other service*')
# Mutation: a plan that ignores the services would remove an account in use.
$mut = (Get-Content -LiteralPath $setup -Raw).Contains('Remove     = ($deps.Count -eq 0)')
Check 'T7 control: the removal condition exists exactly as the test assumes' $mut

if ($failures.Count -gt 0) { Write-Host "`n$($failures.Count) FAILED: $($failures -join '; ')"; exit 1 }
Write-Host "`nall passed"
exit 0
