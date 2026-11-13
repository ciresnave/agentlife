# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Set up self-hosted GitHub Actions runners for our own repos (owned by ciresnave or ThinkersJournal), under a restricted local account (docs/RUNNER-SETUP.md).

.DESCRIPTION
  DRY RUN BY DEFAULT: with no -Apply it prints the plan and the exact commands and changes nothing (no network,
  no account, no files). -Apply needs an ELEVATED PowerShell; the PM and CireSnave run it after review, a lane never does.

  What -Apply does, in order, and refuses at the first failure:
    1. Refuses any -Repo whose owner is not in -Owners (default ciresnave, ThinkersJournal): every repo of ours may
       get a runner (CireSnave 2026-10-16), none of anyone else's. Each repo is named explicitly, each time. A runner
       runs whatever its repo's workflows say, so the policy in docs/RUNNER-SETUP.md decides what those may be.
       A repo is treated as PUBLIC unless you name it in -Private, and -Apply asks GitHub (gh api) and refuses a repo
       declared private that is public: public means ephemeral only.
    2. Creates the local account (default gh-runner): standard user, never in Administrators, password a random
       32 characters (letters, digits, - _ @ only: cmd.exe mangles % ^ !) generated in memory, passed to New-LocalUser and to the service configuration, never written to a
       file, never printed, never logged. An existing account is kept and its password is ROTATED the same way, so
       nobody ever knows it; every service that already logs on as the account is given the new password in the same run, so none breaks at its next start. (The password is visible in the process command line of config.cmd for a moment, to
       administrators only; the runner stores it in the service control manager.)
    3. Creates -Root (default C:\gh-runner) with an ACL of SYSTEM, Administrators and the account only.
    4. Downloads the pinned official runner release and VERIFIES its SHA256 against the pinned value before
       extracting a byte of it. A mismatch deletes the download and stops.
    5. Per repo: extracts to <Root>\<owner>-<repo>, asks for that repo's registration token (Read-Host -AsSecureString:
       the PM fetches it at run time, it expires in an hour, it is used once and never stored), and runs config.cmd
       with labels self-hosted,windows,x64 (automatic) plus -Labels, as a Windows service under the account. Before
       config.cmd it writes <dir>\.env with PSExecutionPolicyPreference=Bypass (this runner's jobs only; see RUNNER-SETUP 5a).
    6. Reads the service back (name, StartName, state): an exit code is not a state read.
  Triggers are the workflow's business, not the runner's: see docs/RUNNER-SETUP.md section 6. This script cannot
  stop a repo from adding a pull_request job that runs on the runner; the lint and the repo setting do.

  Service mode only. Whether CUDA and Vulkan work from a service (session 0) is a measurement the spike workflow
  makes (docs/RUNNER-SETUP.md section 5); until it passes the runner is not trusted with GPU work.

.PARAMETER Repo          One or more owner/name repos to register the runner for. Required.
.PARAMETER Owners        Account names whose repos may get a runner. Default ciresnave, ThinkersJournal.
.PARAMETER Private       The -Repo entries that are PRIVATE (only these may use -Ephemeral:$false). -Apply verifies each with `gh api`.
.PARAMETER Account       Local account name. Default ghr-<repo> (cut to 20 characters with a stable hash suffix), with exactly one -Repo; required with more.
.PARAMETER RotateShared  Allow rotating the password of an EXISTING account that services already log on as (it is refused by default; never run on a live service).
.PARAMETER AddToGroup    Add the account to ONE named local group (never Administrators or Remote Desktop Users); needs -Because.
.PARAMETER Because       Why the group is needed (a workflow path); printed in the plan and cut into the account description.
.PARAMETER Root          Install root, ASCII path. Default C:\gh-runner-<account>: one root per account, so no runner account can write another runner's files. An existing root that already grants another account is REFUSED at -Apply (the shared C:\gh-runner of the three older runners must be passed explicitly for -Remove or -CheckAclsOnly).
.PARAMETER Labels        Extra runner labels, added to the automatic self-hosted, Windows, X64. Default NONE: add gpu only for a repo whose jobs need the GPU, so a plain-Windows job cannot queue behind (or be mistaken for) a GPU one.
.PARAMETER RunnerVersion Runner release to install. Pinned with its hash; change both together.
.PARAMETER RunnerSha256  SHA256 of actions-runner-win-x64-<RunnerVersion>.zip, from the release notes.
.PARAMETER Ephemeral    Register with --ephemeral: the runner takes ONE job, then unregisters and its service stops, so a job cannot leave anything for the next. REQUIRED for the public repos (docs/RUNNER-SETUP.md section 2a); -Ephemeral:$false is refused for them. Default (board 223, CireSnave 2026-10-16): PERSISTENT when every -Repo is named in -Private, EPHEMERAL otherwise (any public or undeclared repo in the call makes the whole call ephemeral). Pass the switch to override either way.
.PARAMETER ProtectedPaths  Paths the runner account must NOT be able to write, directly or through a broad group: the lane tools that run as the owner WITH vault access. Default C:\Projects, C:\Projects\.claude-hooks, with-secret.exe, lane-restart.exe, C:\Projects\.lane-state. -Apply refuses to create the account while any is writable by Everyone, Authenticated Users, Users, Interactive or the account itself (board 224).
.PARAMETER AlsoGrant     Groups or accounts to keep READ-AND-EXECUTE (never write) on the protected paths in the printed icacls fix, e.g. -AlsoGrant CodexSandboxUsers. Only changes the text printed; Everyone, Authenticated Users, Users and Interactive are refused (they are the hole).
.PARAMETER CheckAclsOnly Read-only: run the ACL check, print the exact elevated fix commands for what it finds, and throw if anything is exposed. Nothing else happens.
.PARAMETER PrintMigration Read-only: with -Root naming a SHARED root (e.g. C:\gh-runner), read which service runs from each runner directory under it and as which account, and PRINT the elevated icacls commands that give each account Modify on its own directory only and remove every runner account from the root. Nothing is run; CireSnave decides (it changes live services' access). -Repo is still required (any value).
.PARAMETER Apply         Actually do it. Without it, nothing is changed.
.PARAMETER Remove        With -Apply: unregister and remove the runners for -Repo (asks for a removal token) and leave the account (unless -RemoveAccount).
.PARAMETER Rearm         With -Apply: re-arm an EPHEMERAL runner that has taken its job (one job, then it unregisters itself and its service stops). One elevated run, ONE registration token, same account, same labels: for each -Repo it stops and deletes the runner's service, deletes the runner directory, then does the normal -Apply (rotate the account password, extract, register, read back). It does NOT use `config.cmd remove` (that needs a different, removal token and has not been measured against a runner that already unregistered itself); `--replace` covers a registration that is still on GitHub. Refuses when the directory is outside -Root, holds no runner, or its service logs on as another account. Needs the account to exist already with no other service (the normal -Apply refuses otherwise).
.PARAMETER GitRoot       Git for Windows root. Its bin and usr\bin go FIRST on the runner's PATH (in <runner dir>\.env), ahead of System32: on this box `bash` otherwise resolves to the WSL launcher (C:\Windows\System32\bash.exe) and every `shell: bash` step fails with "Windows Subsystem for Linux has no installed distribution" (mlmf first job, 2026-11-12). Default C:\Program Files\Git.
.PARAMETER RustRoot      The machine-wide Rust install (default C:\rust: .cargo\bin holds the rustup shims, .rustup the toolchains). The runner's .env gets PATH (shims), RUSTUP_HOME (shared toolchains) and CARGO_HOME = <Root>\cargo-home (the account's OWN registry cache, so a job cannot write into the shared C:\.cargo). Pass '' to leave Rust out.
.PARAMETER RemoveAccount With -Remove -Apply: after the runners are gone, also delete the local account, but only when no other service still logs on as it (re-read from the service manager) and it is not a built-in account or the person running the script. Otherwise the account is kept and the reason printed.
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory)][string[]]$Repo,
  [string[]]$Owners = @('ciresnave', 'ThinkersJournal'),
  [string[]]$Private = @(),
  [string]$Account,
  [string]$Root,
  [string[]]$Labels = @(),
  [string]$RunnerVersion = '2.338.0',
  [string]$RunnerSha256 = 'f48e0750a21812bca5f82de5f7f5aeae71abee647fab5a582f1742d07eba455f',
  [Nullable[bool]]$Ephemeral = $null,
  [string[]]$ProtectedPaths = @('C:\Projects', 'C:\Projects\.claude-hooks', 'C:\Projects\.claude-hooks\with-secret.exe', 'C:\Projects\.claude-hooks\lane-restart.exe', 'C:\Projects\.lane-state'),
  [string[]]$AlsoGrant = @(),
  [switch]$CheckAclsOnly,
  [switch]$Apply,
  [switch]$Remove,
  [switch]$Rearm,
  [string]$GitRoot = 'C:\Program Files\Git',
  [string]$RustRoot = 'C:\rust',
  [switch]$PrintMigration,
  [switch]$RotateShared,
  [switch]$RemoveAccount,
  [string]$AddToGroup,
  [string]$Because
)
$ErrorActionPreference = 'Stop'

# One account per runner (docs/RUNNER-ACCOUNTS.md, PM ruling 2026-10-30). The three functions below are read out of this file
# by scripts\tests\gh-runner-setup-account.tests.ps1, so they take everything as arguments and touch nothing.
function Get-RunnerAccountName([string]$repo) {
  if ($repo -notmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$') { throw "Repo '$repo' is not owner/name" }
  # ghr-<repo>, cut to the 20-character SAM limit. A cut name keeps 15 characters, a dash and a stable 4-hex suffix of the
  # whole slug, so two long repo names that share a prefix cannot collide. '.' is not allowed in the account name here.
  $base = 'ghr-' + (($repo.Split('/')[1]) -replace '[^A-Za-z0-9_-]', '-')
  if ($base.Length -le 20) { return $base }
  $sha = [Security.Cryptography.SHA256]::Create()
  try { $h = $sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($repo.ToLowerInvariant())) } finally { $sha.Dispose() }
  $hex = ([BitConverter]::ToString($h[0..1]) -replace '-', '').ToLowerInvariant()
  return $base.Substring(0, 15) + '-' + $hex
}
# Add-only by default: an existing account that a service already logs on as is REFUSED, because rotating its password means
# pushing the new one into each service through Win32_Service.Change, which has never run on a live service. -RotateShared is
# the explicit way back to that path. An existing account with no services has no dependent to break.
function Assert-AccountReusable([string]$account, $dependents, [bool]$rotateShared) {
  $deps = @($dependents | Where-Object { $null -ne $_ })   # @($null) has one element in PowerShell
  $n = $deps.Count
  if ($n -gt 0 -and -not $rotateShared) {
    throw "account $account already has $n service(s) logging on as it ($(($deps | ForEach-Object Name) -join ', ')): REFUSED, nothing changed. Use a new -Account, or -RotateShared to rotate its password and push it into each service (never run on a live service)."
  }
}
# -RemoveAccount (slice 4): after the service is gone, remove the account only when no other service logs on as it. Services are
# passed in (re-read AFTER the removal), so the test feeds fake objects. Built-in accounts and the person running are never removed.
function Get-AccountRemovalPlan([string]$account, $services, [string]$computer, [string]$currentUser) {
  if ($account -imatch '^(Administrator|Guest|DefaultAccount|WDAGUtilityAccount|SYSTEM)$') { throw "-RemoveAccount: $account is a built-in account; REFUSED" }
  if ($currentUser -and $account -ieq $currentUser) { throw "-RemoveAccount: $account is the account running this script; REFUSED" }
  $deps = @($services | Where-Object { $null -ne $_ -and ($_.StartName -ieq ".\$account" -or $_.StartName -ieq "$computer\$account") })
  $names = @($deps | ForEach-Object Name)
  [pscustomobject]@{
    Remove     = ($deps.Count -eq 0)
    Dependents = $names
    Reason     = $(if ($deps.Count -eq 0) { 'no service logs on as it' } else { "kept: $($deps.Count) service(s) still log on as it ($($names -join ', '))" })
  }
}
# Groups are listed, never defaulted: one named local group, with the reason. Administrators and Remote Desktop Users stay refused.
# The root ACL (slice 2, docs/RUNNER-ACCOUNTS.md T3). Both functions take the principal as an argument and are read out of this file
# by scripts\tests\gh-runner-setup-account.tests.ps1, which runs them on real ACLs in a temp directory.
# Principals with ANY allow entry on the root other than SYSTEM, Administrators, the person running the script and the one account: a shared root re-creates the hole
# where each runner account can rewrite the binaries another service starts as a different account.
function Get-RootForeignPrincipals([string]$root, [string]$principal) {
  if (-not (Test-Path -LiteralPath $root)) { return @() }
  $keep = New-Object System.Collections.Generic.HashSet[string]
  foreach ($x in 'S-1-5-18', 'S-1-5-32-544') { [void]$keep.Add($x) }
  # The person running this (an administrator by the time -Apply runs) is not a runner account; a CI runner's own user shows up on a fresh directory.
  [void]$keep.Add([Security.Principal.WindowsIdentity]::GetCurrent().User.Value)
  try { [void]$keep.Add(([Security.Principal.NTAccount]$principal).Translate([Security.Principal.SecurityIdentifier]).Value) } catch { }
  if ($PSVersionTable.PSEdition -eq 'Core') { $sec = [System.IO.FileSystemAclExtensions]::GetAccessControl([System.IO.DirectoryInfo]::new($root)) }
  else { $sec = [System.IO.Directory]::GetAccessControl($root) }
  $rules = $sec.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])
  if ($null -eq $rules) { throw "could not read the ACL of $root (refusing rather than assuming it is clean)" }
  foreach ($ace in $rules) {
    if ($ace.AccessControlType -ne 'Allow' -or $keep.Contains($ace.IdentityReference.Value)) { continue }
    try { $ace.IdentityReference.Translate([Security.Principal.NTAccount]).Value } catch { $ace.IdentityReference.Value }
  }
}
function Set-RunnerRootAcl([string]$root, [string]$principal) {
  icacls $root /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' "${principal}:(OI)(CI)M" | Out-Null
  if ($LASTEXITCODE -ne 0) { throw "icacls on $root exited $LASTEXITCODE" }
}
# Slice 3 (docs/RUNNER-ACCOUNTS.md): the printed migration of a SHARED root. Both functions are pure (they read what they are handed and run
# nothing) and are read out of this file by scripts\tests\gh-runner-setup-account.tests.ps1.
# Which runner directory under $root is run by which local account. $services: objects with Name, PathName, StartName (null reads Win32_Service).
function Get-SharedRootRunners([string]$root, $services = $null) {
  if ($null -eq $services) { $services = Get-CimInstance Win32_Service -Filter "PathName LIKE '%RunnerService.exe%'" }
  $prefix = ([IO.Path]::GetFullPath($root)).TrimEnd('\') + '\'
  $byDir = [ordered]@{}
  foreach ($s in @($services)) {
    $exe = ($s.PathName -replace '^\s*"([^"]*)".*$', '$1').Trim()
    if (-not $exe.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase) -or $exe -notlike '*RunnerService.exe') { continue }
    $dir = $prefix + $exe.Substring($prefix.Length).Split('\')[0]
    $m = [regex]::Match([string]$s.StartName, '^(?:\.|[^\\]+)\\([A-Za-z0-9_ -]{1,20})$')
    if (-not $m.Success) { throw "service $($s.Name) under $dir logs on as '$($s.StartName)', not a local account: not migrating it by script" }
    if ($byDir.Contains($dir) -and $byDir[$dir].Account -ine $m.Groups[1].Value) { throw "$dir is run by two different accounts ($($byDir[$dir].Account), $($m.Groups[1].Value))" }
    $byDir[$dir] = [pscustomobject]@{ Dir = $dir; Account = $m.Groups[1].Value; Service = $s.Name }
  }
  @($byDir.Values)
}
# The commands, in the order that is safe: each directory is detached from the root and granted to its own account first, then the runner
# accounts are removed from the root, then the root is left to SYSTEM, Administrators and the person running it. Each is Path + Args for icacls.
function New-RootMigrationPlan([string]$root, [string]$computer, $runners, [string]$keepUser) {
  $base = @('/inheritance:r', '/grant:r', '*S-1-5-18:(OI)(CI)F', '*S-1-5-32-544:(OI)(CI)F')
  foreach ($r in @($runners)) { [pscustomobject]@{ Path = $r.Dir; Args = $base + @("${computer}\$($r.Account):(OI)(CI)M") } }
  foreach ($a in @($runners | ForEach-Object Account | Sort-Object -Unique)) { [pscustomobject]@{ Path = $root; Args = @('/remove', "${computer}\$a") } }
  [pscustomobject]@{ Path = $root; Args = $base + @("${keepUser}:(OI)(CI)F") }
}
function Test-GroupRequest([string]$group, [string]$because) {
  if (-not $group -and -not $because) { return $null }
  if (-not $group) { throw '-Because needs -AddToGroup' }
  if (-not $because) { throw "-AddToGroup '$group' needs -Because <the workflow path that needs it>" }
  if ($group -notmatch '^[A-Za-z0-9_. -]{1,60}$') { throw "-AddToGroup '$group' is not a plain local group name" }
  if ($group -imatch '^(Administrators|Remote Desktop Users|Users|Everyone|Authenticated Users|Guests)$') { throw "-AddToGroup '$group' is refused: the runner account stays a standard user" }
  if ($because -notmatch '^[A-Za-z0-9_./ -]{1,200}$') { throw '-Because must be plain text (a workflow path or a short reason)' }
  # New-LocalUser/Set-LocalUser reject a description over 48 characters at run time only (the 2026-10-17 live run), so cut here.
  $d = "runner; $group because $because"
  [pscustomobject]@{ Group = $group; Because = $because; Description = $d.Substring(0, [Math]::Min(48, $d.Length)) }
}

$accountDefaulted = -not $PSBoundParameters.ContainsKey('Account')
if ($accountDefaulted) {
  if (@($Repo).Count -ne 1) { throw '-Account is required with more than one -Repo: one account per runner (docs/RUNNER-ACCOUNTS.md). Name the account for each, or run once per repo.' }
  $Account = Get-RunnerAccountName $Repo[0]
}
$groupPlan = Test-GroupRequest $AddToGroup $Because
if ($RemoveAccount -and -not $Remove) { throw '-RemoveAccount needs -Remove' }
if ($Rearm -and ($Remove -or $RemoveAccount -or $PrintMigration -or $CheckAclsOnly)) { throw '-Rearm is its own mode: not with -Remove, -RemoveAccount, -PrintMigration or -CheckAclsOnly' }
if ($RemoveAccount) { Get-AccountRemovalPlan $Account @() $env:COMPUTERNAME $env:USERNAME | Out-Null }   # refuse a built-in or current account BEFORE any runner is removed
$rootDefaulted = -not $PSBoundParameters.ContainsKey('Root')
if ($rootDefaulted) { $Root = "C:\gh-runner-$Account" }

foreach ($g in $AlsoGrant) {
  if ($g -notmatch '^[A-Za-z0-9_. \\-]{1,60}$') { throw "-AlsoGrant '$g' is not a plain group or account name" }
  $bare = ($g -split '\\')[-1]
  if ($bare -imatch '^(Everyone|Authenticated Users|Users|Interactive|INTERACTIVE|Guests|Anonymous Logon)$') { throw "-AlsoGrant '$g' is one of the broad principals the preflight exists to keep out" }
  if ($bare -ieq $Account) { throw "-AlsoGrant '$g' is the runner account itself" }
}
$nameRe = '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$'
foreach ($r in $Repo) {
  if ($r -notmatch $nameRe) { throw "Repo '$r' is not owner/name" }
  if ($Owners -notcontains $r.Split('/')[0]) { throw "Repo '$r' is not owned by one of ($($Owners -join ', ')): a runner runs that repo's workflows as $Account" }
}
if ($Account -notmatch '^[A-Za-z0-9_-]{1,20}$') { throw "Account '$Account' is not a plain local account name" }
if ($Account -ieq $env:USERNAME) { throw 'Account must not be the account you run this as (it is not yours)' }
if ($Root -notmatch '^[A-Za-z]:\\' -or $Root -match '[^\x20-\x7E]' -or $Root -match '[%"&|<>^]') { throw "Root '$Root' must be an absolute ASCII path on a drive" }
foreach ($l in $Labels) { if ($l -notmatch '^[A-Za-z0-9_.-]+$') { throw "Label '$l' has a character the config line cannot carry" } }
if ($RunnerSha256 -notmatch '^[0-9a-fA-F]{64}$') { throw 'RunnerSha256 is not 64 hex characters' }
if ($RunnerVersion -notmatch '^\d+\.\d+\.\d+$') { throw 'RunnerVersion is not major.minor.patch' }

$zipName = "actions-runner-win-x64-$RunnerVersion.zip"
$zipUrl = "https://github.com/actions/runner/releases/download/v$RunnerVersion/$zipName"
$zipPath = Join-Path $Root $zipName
$allLabels = ($Labels -join ',')
$labelFlag = if ($Labels) { " --labels $allLabels" } else { '' }
$labelArgs = if ($Labels) { @('--labels', $allLabels) } else { @() }
foreach ($pr in $Private) { if ($Repo -notcontains $pr) { throw "-Private names '$pr', which is not in -Repo" } }
# Board 223 (CireSnave, 2026-10-16: "Go with your recommendations."): private repos get a persistent runner, public repos an
# ephemeral one. Unset -Ephemeral therefore follows -Private: persistent only if EVERY -Repo is declared private.
$ephemeralDefaulted = ($null -eq $Ephemeral)
if ($ephemeralDefaulted) {
  $undeclared = @($Repo | Where-Object { $Private -notcontains $_ })
  $Ephemeral = ($undeclared.Count -gt 0)
}
$Ephemeral = [bool]$Ephemeral
# A repo is PUBLIC unless declared private: anyone can open a PR on a public repo, so a persistent runner is refused.
if (-not $Ephemeral) {
  $pub = @($Repo | Where-Object { $Private -notcontains $_ })
  if ($pub) { throw "-Ephemeral:`$false is refused for public repo(s): $($pub -join ', ')" }
}
$ephFlag = if ($Ephemeral) { ' --ephemeral' } else { '' }
# The lines of <runner dir>\.env (pure: reads only its arguments; read out of this file by scripts\tests\gh-runner-setup-env.tests.ps1).
# PATH: Git's bin and usr\bin, then the Rust shims, then the machine PATH minus any repeat; RUSTUP_HOME shared, CARGO_HOME the account's own.
function Get-RunnerEnvLines([string]$gitRoot, [string]$rustRoot, [string]$cargoHome, [string]$machinePath) {
  foreach ($v in $gitRoot, $rustRoot, $cargoHome) {
    if ($v -and ($v -notmatch '^[A-Za-z]:\\[\x20-\x7E]*$' -or $v -match '[;%"=]')) { throw "'$v' must be an absolute ASCII path with no ; % `" or =" }
  }
  $lines = New-Object System.Collections.Generic.List[string]
  $lines.Add('PSExecutionPolicyPreference=Bypass')
  $front = New-Object System.Collections.Generic.List[string]
  if ($gitRoot) { $front.Add("$gitRoot\bin"); $front.Add("$gitRoot\usr\bin") }
  if ($rustRoot) { $front.Add("$rustRoot\.cargo\bin") }
  if ($front.Count -gt 0) {
    $seen = New-Object System.Collections.Generic.HashSet[string] ([StringComparer]::OrdinalIgnoreCase)
    $parts = New-Object System.Collections.Generic.List[string]
    foreach ($d in @($front) + @($machinePath -split ';')) { $d = $d.Trim().TrimEnd('\'); if ($d -and $seen.Add($d)) { $parts.Add($d) } }
    $lines.Add('PATH=' + ($parts -join ';'))
  }
  if ($rustRoot) { $lines.Add("RUSTUP_HOME=$rustRoot\.rustup"); $lines.Add("CARGO_HOME=$cargoHome") }
  @($lines)
}
# What the probe printed, judged (pure). The probe runs AS the runner account with the .env's variables and prints one line per fact.
# Returns the failures (empty = good): bash must be Git's, must run, and cargo and rustc must run.
function Test-ProbeOutput([string]$text, [string]$gitRoot, [bool]$rust) {
  $f = New-Object System.Collections.Generic.List[string]
  $lines = @($text -split "`r?`n" | ForEach-Object { $_.Trim() } | Where-Object { $_ })
  $get = { param($k) @($lines | Where-Object { $_ -like "$k=*" } | ForEach-Object { $_.Substring($k.Length + 1) }) | Select-Object -First 1 }
  $bashPath = & $get 'bash-path'
  if (-not $bashPath) { $f.Add('bash was not found on the account''s PATH') }
  elseif ($gitRoot -and -not $bashPath.StartsWith($gitRoot + '\', [StringComparison]::OrdinalIgnoreCase)) { $f.Add("bash resolves to $bashPath, not Git's ($gitRoot): the WSL launcher is still first") }
  if (([string](& $get 'bash-run')) -ne 'ok') { $f.Add("bash -c ""echo ok"" printed '$(& $get 'bash-run')', not ok") }
  if ($rust) {
    if (([string](& $get 'cargo')) -notmatch '^cargo \d+\.\d+\.\d+') { $f.Add("cargo --version printed '$(& $get 'cargo')'") }
    if (([string](& $get 'rustc')) -notmatch '^rustc \d+\.\d+\.\d+') { $f.Add("rustc --version printed '$(& $get 'rustc')'") }
  }
  @($f)
}
# Written to <runner dir>\.env, which the runner loads for every job. Windows PowerShell's execution policy is Restricted on this
# box, and the runner dot-sources a temp .ps1 for each `shell: powershell` step (spike run 1, 2026-10-17). The env var is process
# scope: it touches this runner's jobs only, no machine or registry-hive setting. Not a security boundary (a job runs any command).
$cargoHome = if ($RustRoot) { Join-Path $Root 'cargo-home' } else { '' }
$runnerEnv = @(Get-RunnerEnvLines $GitRoot $RustRoot $cargoHome ([Environment]::GetEnvironmentVariable('Path', 'Machine')))
function Slug([string]$r) { $r -replace '/', '-' }
# The runner TRUNCATES long service names ('...TUF-LAPTOP-ThinkersJ-9595'), so a name built from the repo cannot be trusted
# (live run 2026-10-22: read-back threw 'not found' for a service that was running). The service is found by the runner
# directory its executable lives in. `$services` is for the test (objects with Name and PathName); null reads Win32_Service.
function Find-RunnerService([string]$dir, $services = $null) {
  if ($null -eq $services) { $services = Get-CimInstance Win32_Service -Filter "PathName LIKE '%RunnerService.exe%'" }
  $sep = [string][IO.Path]::DirectorySeparatorChar
  $prefix = ([IO.Path]::GetFullPath($dir)).TrimEnd($sep[0]) + $sep
  $hit = @($services | Where-Object {
    $exe = ($_.PathName -replace '^\s*"([^"]*)".*$', '$1').Trim()
    $exe.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase) -and $exe -like '*RunnerService.exe'
  })
  if ($hit.Count -gt 1) { throw "more than one runner service lives under ${dir}: $(($hit | ForEach-Object Name) -join ', ')" }
  if ($hit.Count -eq 1) { $hit[0] } else { $null }
}

# -Rearm (PM 2026-11-12, CireSnave board 425 mode A). Pure: reads what it is handed, runs nothing; read out of this file by
# scripts\tests\gh-runner-setup-rearm.tests.ps1. $services: objects with Name, PathName, StartName (Find-RunnerService reads CIM when null).
# Returns what the teardown would delete, or throws: the directory must be a direct child of $root, hold config.cmd (a runner), and any
# service running from it must log on as $account (never delete another account's service).
function Get-RearmPlan([string]$root, [string]$dir, [string]$account, [string]$computer, $services = $null) {
  $rootFull = [IO.Path]::GetFullPath($root).TrimEnd('\') + '\'
  $dirFull = [IO.Path]::GetFullPath($dir).TrimEnd('\')
  if (-not $dirFull.StartsWith($rootFull, [StringComparison]::OrdinalIgnoreCase) -or $dirFull.Substring($rootFull.Length).Contains('\') -or $dirFull.Length -le $rootFull.Length) {
    throw "-Rearm: $dir is not a direct child of the root $root; refusing to delete it"
  }
  if (-not (Test-Path -LiteralPath "$dirFull\config.cmd")) { throw "-Rearm: no runner at $dirFull (no config.cmd): nothing to re-arm; use a plain -Apply" }
  $svc = Find-RunnerService $dirFull $services
  if ($svc -and $svc.StartName -ine ".\$account" -and $svc.StartName -ine "$computer\$account") {
    throw "-Rearm: the service $($svc.Name) under $dirFull logs on as '$($svc.StartName)', not $account; refusing to delete it"
  }
  [pscustomobject]@{ Dir = $dirFull; Service = $(if ($svc) { $svc.Name } else { $null }) }
}

# ACL preflight (board 224). A local account that can replace with-secret.exe or lane-restart.exe gets code run as
# the owner WITH vault access, so none of the protected paths may be writable by any principal the runner account
# is in. Read-only. "Authenticated Users: Modify" inherited from C:\ is the exact hole this exists for.
# The ACL read goes straight to .NET, not through Get-Acl: Get-Acl lives in Microsoft.PowerShell.Security, which Windows
# PowerShell 5.1 cannot load when it inherits a PowerShell 7 PSModulePath (seen under a test harness launched from pwsh).
function Read-Acl([string]$Path) {
  $isDir = Test-Path -LiteralPath $Path -PathType Container
  if ($PSVersionTable.PSEdition -eq 'Core') {
    if ($isDir) { return [System.IO.FileSystemAclExtensions]::GetAccessControl([System.IO.DirectoryInfo]::new($Path)) }
    return [System.IO.FileSystemAclExtensions]::GetAccessControl([System.IO.FileInfo]::new($Path))
  }
  if ($isDir) { return [System.IO.Directory]::GetAccessControl($Path) }
  return [System.IO.File]::GetAccessControl($Path)
}
function Get-WriteExposure {
  param([string[]]$Paths, [string]$AccountName)
  # Everyone, Authenticated Users, Users, Interactive, plus the runner account itself if it exists.
  $sids = New-Object System.Collections.Generic.HashSet[string]
  foreach ($x in 'S-1-1-0', 'S-1-5-11', 'S-1-5-32-545', 'S-1-5-4') { [void]$sids.Add($x) }
  try {
    $acct = New-Object System.Security.Principal.NTAccount("$env:COMPUTERNAME\$AccountName")
    [void]$sids.Add($acct.Translate([System.Security.Principal.SecurityIdentifier]).Value)
  } catch { }
  # WriteData, AppendData, WriteExtendedAttributes, WriteAttributes, DeleteSubdirectoriesAndFiles, Delete,
  # ChangePermissions, TakeOwnership, plus the generic GENERIC_ALL and GENERIC_WRITE bits some inherited ACEs carry.
  $mask = 0x2 -bor 0x4 -bor 0x10 -bor 0x100 -bor 0x40 -bor 0x10000 -bor 0x40000 -bor 0x80000 -bor 0x10000000 -bor 0x40000000
  foreach ($path in $Paths) {
    if (-not (Test-Path -LiteralPath $path)) { continue }
    # GetAccessRules, never the `.Access` property: that property exists only on what Get-Acl returns, so on a raw
    # security object it is $null and the loop would see no entries, i.e. a silent "clean". Rules come back as SIDs.
    $rules = (Read-Acl $path).GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier])
    if ($null -eq $rules) { throw "could not read the ACL of $path (refusing rather than assuming it is clean)" }
    foreach ($ace in $rules) {
      if ($ace.AccessControlType -ne 'Allow') { continue }
      $sid = $ace.IdentityReference.Value
      if (-not $sids.Contains($sid)) { continue }
      if (($ace.FileSystemRights.value__ -band $mask) -eq 0) { continue }
      [pscustomobject]@{ Path = $path; Who = $(try { $ace.IdentityReference.Translate([System.Security.Principal.NTAccount]).Value } catch { $ace.IdentityReference.Value }); Rights = $(if ("$($ace.FileSystemRights)" -match '^-?\d+$') { 'generic bits 0x{0:X8}' -f $ace.FileSystemRights.value__ } else { [string]$ace.FileSystemRights }); Inherited = $ace.IsInherited }
    }
  }
}
function Show-AclFix {
  param($Findings)
  $me = "$env:USERDOMAIN\$env:USERNAME"
  Write-Host "The runner account (or a group every account is in) can WRITE these, and so could replace a tool that runs as $me with vault access:"
  foreach ($f in $Findings) { Write-Host ("  {0}  <-  {1} : {2}{3}" -f $f.Path, $f.Who, $f.Rights, $(if ($f.Inherited) { ' (inherited)' } else { '' })) }
  Write-Host "Fix, from an ELEVATED PowerShell by an administrator (this script never runs it). It cuts off inheritance on the top-level path,"
  Write-Host "so everything under it inherits SYSTEM, Administrators and you only$(if ($AlsoGrant.Count -gt 0) { ", plus read-and-execute for $($AlsoGrant -join ', ')" }), and EVERY other local account loses access to it (pass -AlsoGrant <group> to keep read for one):"
  # Only the outermost exposed paths: everything below inherits the fix (a child with its own entry shows up on the re-check).
  $all = @($Findings | ForEach-Object { $_.Path } | Sort-Object -Unique)
  $tops = @($all | Where-Object { $p = $_; -not ($all | Where-Object { $_ -ne $p -and $p.StartsWith($_.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase) }) })
  $extra = ($AlsoGrant | ForEach-Object { " `"{0}:(OI)(CI)RX`"" -f $_ }) -join ''
  foreach ($t in $tops) { Write-Host ("  icacls `"{0}`" /inheritance:r /grant:r `"*S-1-5-18:(OI)(CI)F`" `"*S-1-5-32-544:(OI)(CI)F`" `"{1}:(OI)(CI)F`"{2}" -f $t, $me, $extra) }
  if ($AlsoGrant.Count -gt 0) { Write-Host ("  (keeping read-and-execute, no write, for: {0})" -f ($AlsoGrant -join ', ')) }
  Write-Host "Then read it back: re-run this script with -CheckAclsOnly; it must pass. A child with its OWN (non-inherited) entry needs the same icacls."
}
if ($PrintMigration) {
  if (-not $PSBoundParameters.ContainsKey('Root')) { throw '-PrintMigration needs -Root <the shared root>, e.g. C:\gh-runner' }
  $found = @(Get-SharedRootRunners $Root)
  if ($found.Count -eq 0) { throw "no runner service lives under ${Root}: nothing to migrate" }
  Write-Host "MIGRATION PLAN for the shared root $Root (read-only: nothing was run). Run these from an ELEVATED shell, in this order, then read it back."
  Write-Host "Today these accounts can all write each other's runner files; afterwards each can write only its own directory and cannot list the root (a service still reaches its directory by full path)."
  foreach ($r in $found) { Write-Host "  $($r.Dir): service $($r.Service) as $($r.Account)" }
  Write-Host "Stop the affected services first (Stop-Service), run the commands, then Start-Service each: a running service holds files open. Explicit entries such as the runner's own GITHUB_ActionsRunner_* group are kept."
  foreach ($c in @(New-RootMigrationPlan $Root $env:COMPUTERNAME $found "$env:USERDOMAIN\$env:USERNAME")) { Write-Host ("  icacls `"{0}`" {1}" -f $c.Path, (($c.Args | ForEach-Object { '"' + $_ + '"' }) -join ' ')) }
  Write-Host "Read it back: icacls on each directory (only its own account has Modify), then -CheckAclsOnly, then confirm each service is Running as the same account."
  return
}
$aclFindings = @()
if (-not $Remove) { $aclFindings = @(Get-WriteExposure -Paths $ProtectedPaths -AccountName $Account) }
if ($CheckAclsOnly) {
  if ($aclFindings.Count -gt 0) { Show-AclFix $aclFindings; throw "ACL preflight FAILED: $($aclFindings.Count) exposed entr(ies) on the protected paths" }
  Write-Host "ACL preflight passed: none of ($($ProtectedPaths -join ', ')) is writable by Everyone, Authenticated Users, Users, Interactive or $Account."
  return
}

if (-not $Apply) {
  Write-Host "DRY RUN (nothing changed, no network). Pass -Apply from an elevated shell to do this.`n"
  Write-Host "Owners        : $($Owners -join ', ')   private (persistent allowed): $(if ($Private) { $Private -join ', ' } else { 'none: every -Repo is treated as public, so ephemeral' })"
  Write-Host "Runner mode   : $(if ($Ephemeral) { 'ephemeral (one job per registration)' } else { 'persistent (one registration, many jobs)' })$(if ($ephemeralDefaulted) { ' [default: ' + $(if ($Ephemeral) { 'a -Repo is not declared in -Private' } else { 'every -Repo is declared in -Private' }) + ']' } else { ' [set by -Ephemeral]' })"
  Write-Host "Account       : $env:COMPUTERNAME\$Account (standard user; random 32-char password held in memory only)$(if ($accountDefaulted) { ' [default: ghr-<repo>, one account per runner]' })"
  Write-Host "  account plan: create | exists with 0 services: password rotated | exists with services: REFUSED unless -RotateShared"
  Write-Host "  group memberships: $(if ($groupPlan) { $groupPlan.Group + ' because ' + $groupPlan.Because } else { 'none' })"
  Write-Host "Root          : $Root  (ACL: SYSTEM, Administrators, ${Account}: its own tree only)$(if ($rootDefaulted) { ' [default: C:\gh-runner-<account>, one root per account]' })"
  $foreign = @(Get-RootForeignPrincipals $Root "$env:COMPUTERNAME\$Account")
  Write-Host "  root plan: $(if ($foreign.Count -gt 0) { 'REFUSED at -Apply, the root already grants ' + ($foreign -join ', ') } else { 'create or reuse; no other account has an entry' })"
  Write-Host "Runner        : v$RunnerVersion  SHA256 $RunnerSha256`n  $zipUrl"
  if ($aclFindings.Count -gt 0) { Write-Host "`nACL PREFLIGHT WOULD REFUSE -Apply:"; Show-AclFix $aclFindings; Write-Host "" }
  else { Write-Host "ACL preflight: clean (-Apply would proceed past it)" }
  foreach ($r in $Repo) {
    $dir = Join-Path $Root (Slug $r)
    Write-Host "`n[$r]"
    Write-Host "  extract  -> $dir (only after the hash matches)"
    Write-Host "  token    -> prompted (Read-Host -AsSecureString): PM fetches it now; not stored"
    if ($Rearm) {
      $rp = $null
      try { $rp = Get-RearmPlan $Root $dir $Account $env:COMPUTERNAME } catch { Write-Host "  -Rearm WOULD REFUSE: $($_.Exception.Message)" }
      if ($rp) { Write-Host "  re-arm   -> $(if ($rp.Service) { 'stop and delete service ' + $rp.Service } else { 'no service under the directory' }), delete $($rp.Dir), then the registration below (ONE registration token; no removal token, no config.cmd remove)" }
    }
    if ($Remove) { Write-Host "  $dir\config.cmd remove --token <removal token>"; if ($RemoveAccount) { Write-Host "  then account $env:COMPUTERNAME\$Account removed ONLY if no other service logs on as it (re-read after the remove)" } }
    else {
      Write-Host "  $dir\config.cmd --unattended --url https://github.com/$r --token <token> --name $($env:COMPUTERNAME)-$(Slug $r) ``"
      Write-Host "     $labelFlag --work _work --replace$ephFlag --runasservice --windowslogonaccount .\$Account --windowslogonpassword <generated>"
      Write-Host "  .env     -> $dir\.env (before config.cmd, so the service starts with it):"
      foreach ($l in $runnerEnv) { Write-Host "     $(if ($l.Length -gt 150) { $l.Substring(0, 150) + ' ...' } else { $l })" }
      Write-Host "  probe    -> after the service read-back, AS $Account (Start-Process -Credential, Secondary Logon): bash path, bash -c `"echo ok`"$(if ($RustRoot) { ', cargo --version, rustc --version' }) with those .env values; any failure stops the run"
      Write-Host "  read back: service ... StartName = .\$Account, State = Running; every .env line present"
    }
  }
  return
}

$id = [Security.Principal.WindowsIdentity]::GetCurrent()
if (-not ([Security.Principal.WindowsPrincipal]$id).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
  throw '-Apply needs an elevated PowerShell'
}
if ($aclFindings.Count -gt 0) {
  Show-AclFix $aclFindings
  throw "refusing to create or change the account: $($aclFindings.Count) exposed entr(ies) on the protected paths (board 224). Fix them, then run again."
}

# Services that log on as the account (any root): a changed password must reach every one of them, or each fails at its next start.
function Select-AccountServices($services, [string]$account, [string]$computer) {
  @($services | Where-Object { $_.StartName -ieq ".\$account" -or $_.StartName -ieq "$computer\$account" })
}

function New-RandomPassword([int]$len = 32) {
  # Letters, digits and -_@ ONLY: config.cmd is a .cmd file, so cmd.exe re-reads the argument and mangles % ^ ! (live run
  # 2026-10-22: 'Invalid windows credentials entered', config.cmd exited 1, a runner registered with no service). The
  # test pushes generated passwords through a real cmd.exe round trip.
  $chars = 'ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789-_@'
  $rng = [Security.Cryptography.RandomNumberGenerator]::Create()
  $limit = 256 - (256 % $chars.Length)   # rejection sampling: no modulo bias
  $b = New-Object byte[] 1
  $sb = New-Object Text.StringBuilder
  while ($sb.Length -lt $len) {
    $rng.GetBytes($b)
    if ($b[0] -lt $limit) { [void]$sb.Append($chars[$b[0] % $chars.Length]) }
  }
  $rng.Dispose()
  $sb.ToString()
}

if ($Remove) {
  foreach ($r in $Repo) {
    $dir = Join-Path $Root (Slug $r)
    if (-not (Test-Path -LiteralPath "$dir\config.cmd")) { Write-Host "[$r] no runner at $dir, skipping"; continue }
    $before = Find-RunnerService $dir
    $tok = [Net.NetworkCredential]::new('', (Read-Host "Removal token for $r" -AsSecureString)).Password
    & "$dir\config.cmd" remove --token $tok
    if ($LASTEXITCODE -ne 0) { throw "config.cmd remove exited $LASTEXITCODE for $r" }
    $tok = $null
    if (Find-RunnerService $dir) { throw "a runner service under $dir still exists after remove" }
    if ($before -and (Get-Service -Name $before.Name -ErrorAction SilentlyContinue)) { throw "service $($before.Name) still exists after remove" }
    Write-Host "[$r] removed (read back: service absent). Directory $dir kept for you to delete."
  }
  if ($RemoveAccount) {
    $plan = Get-AccountRemovalPlan $Account (Get-CimInstance Win32_Service) $env:COMPUTERNAME $env:USERNAME
    if (-not (Get-LocalUser -Name $Account -ErrorAction SilentlyContinue)) { Write-Host "account ${Account}: already absent" }
    elseif (-not $plan.Remove) { Write-Host "account ${Account}: $($plan.Reason)" }
    else {
      Remove-LocalUser -Name $Account
      if (Get-LocalUser -Name $Account -ErrorAction SilentlyContinue) { throw "account $Account still exists after Remove-LocalUser" }
      Write-Host "account ${Account}: removed (read back: absent); $($plan.Reason)"
    }
  }
  return
}

# 1b. Visibility is read from GitHub, never taken on trust: a repo declared private that is public is refused.
foreach ($r in $Repo) {
  $isPrivate = (gh api "repos/$r" --jq .private 2>&1 | Out-String).Trim()
  if ($isPrivate -ne 'true' -and $isPrivate -ne 'false') { throw "cannot read visibility of $r from gh api: $isPrivate" }
  if (($Private -contains $r) -and $isPrivate -ne 'true') { throw "$r is declared -Private but GitHub says it is public" }
}

# 1c. -Rearm: tear down each runner that has taken its job, with no removal token. Every plan is made (and so refused) BEFORE any
# service or directory is touched; each deletion is read back. Then the normal flow below runs unchanged.
if ($Rearm) {
  $plans = @(foreach ($r in $Repo) { Get-RearmPlan $Root (Join-Path $Root (Slug $r)) $Account $env:COMPUTERNAME })
  foreach ($pl in $plans) {
    if ($pl.Service) {
      Stop-Service -Name $pl.Service -Force -ErrorAction SilentlyContinue
      sc.exe delete $pl.Service | Out-Null
      if ($LASTEXITCODE -ne 0) { throw "sc.exe delete $($pl.Service) exited $LASTEXITCODE" }
      $gone = $false
      foreach ($i in 1..20) { if (-not (Get-Service -Name $pl.Service -ErrorAction SilentlyContinue)) { $gone = $true; break }; Start-Sleep -Milliseconds 500 }
      if (-not $gone) { throw "service $($pl.Service) is still registered 10 s after sc.exe delete (marked for deletion with a handle open?): close what holds it and run again" }
      Write-Host "re-arm: service $($pl.Service) deleted (read back: absent)"
    }
    Remove-Item -LiteralPath $pl.Dir -Recurse -Force
    if (Test-Path -LiteralPath $pl.Dir) { throw "$($pl.Dir) still exists after Remove-Item" }
    Write-Host "re-arm: $($pl.Dir) deleted (read back: absent)"
  }
}

# 2. The account.
$password = New-RandomPassword
$secure = ConvertTo-SecureString $password -AsPlainText -Force
$existing = Get-LocalUser -Name $Account -ErrorAction SilentlyContinue
if ($existing) {
  # A new password nobody knows would silently break every runner service already configured with the old one (live run
  # 2026-10-22: the ThinkersFoundation-site service would have failed at its next start). So the same run gives each of
  # them the new password through the service manager (CIM Change: the password is not on a command line).
  $dependents = Select-AccountServices (Get-CimInstance Win32_Service) $Account $env:COMPUTERNAME
  Assert-AccountReusable $Account $dependents ([bool]$RotateShared)
  Set-LocalUser -Name $Account -Password $secure
  foreach ($d in $dependents) {
    $res = Invoke-CimMethod -InputObject $d -MethodName Change -Arguments @{ StartName = ".\$Account"; StartPassword = $password }
    if ($res.ReturnValue -ne 0) { throw "the password of $Account was rotated but service $($d.Name) could not be updated (Win32_Service.Change returned $($res.ReturnValue)): fix that service before it next starts" }
  }
  Write-Host "account $Account exists: password rotated (nobody knows it); $($dependents.Count) service(s) logging on as it were given the new one: $(($dependents | ForEach-Object Name) -join ', ')"
} else {
  New-LocalUser -Name $Account -Password $secure -PasswordNeverExpires -UserMayNotChangePassword `
    -AccountNeverExpires -Description 'GitHub Actions runner (agentlife setup script)' | Out-Null
  Write-Host "account $Account created"
}
Add-LocalGroupMember -Group 'Users' -Member $Account -ErrorAction SilentlyContinue
if ($groupPlan) {
  Add-LocalGroupMember -Group $groupPlan.Group -Member $Account -ErrorAction Stop
  Set-LocalUser -Name $Account -Description $groupPlan.Description
  Write-Host "group: $Account added to '$($groupPlan.Group)' because $($groupPlan.Because)"
}
$admins = @(Get-LocalGroupMember -Group 'Administrators' | ForEach-Object { $_.Name })
if ($admins | Where-Object { $_ -ieq "$env:COMPUTERNAME\$Account" }) { throw "$Account is in Administrators; refusing. Remove it and run again." }
$rdp = @(Get-LocalGroupMember -Group 'Remote Desktop Users' -ErrorAction SilentlyContinue | ForEach-Object { $_.Name })
if ($rdp | Where-Object { $_ -ieq "$env:COMPUTERNAME\$Account" }) { throw "$Account may log on by Remote Desktop; refusing. Remove it and run again." }

# 3. The root.
$foreign = @(Get-RootForeignPrincipals $Root "${env:COMPUTERNAME}\${Account}")
if ($foreign.Count -gt 0) { throw "root $Root already grants $($foreign -join ', '): REFUSED, nothing changed. One root per account (docs/RUNNER-ACCOUNTS.md); pass a different -Root." }
New-Item -ItemType Directory -Force -Path $Root | Out-Null
Set-RunnerRootAcl $Root "${env:COMPUTERNAME}\${Account}"

# 4. The download, verified before any extraction.
if (-not (Test-Path -LiteralPath $zipPath)) {
  [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
  Invoke-WebRequest -Uri $zipUrl -OutFile $zipPath -UseBasicParsing
}
$got = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash
if ($got -ine $RunnerSha256) {
  Remove-Item -LiteralPath $zipPath -Force
  throw "SHA256 of $zipName is $got, expected $RunnerSha256; download deleted, nothing extracted"
}
Write-Host "SHA256 verified: $got"

# 5. One runner per repo.
Add-Type -AssemblyName System.IO.Compression.FileSystem
foreach ($r in $Repo) {
  $dir = Join-Path $Root (Slug $r)
  if (Test-Path -LiteralPath "$dir\config.cmd") { throw "$dir already holds a runner; run with -Remove first" }
  [IO.Compression.ZipFile]::ExtractToDirectory($zipPath, $dir)
  Set-Content -LiteralPath "$dir\.env" -Value $runnerEnv -Encoding Ascii
  $tok = [Net.NetworkCredential]::new('', (Read-Host "Registration token for $r (not stored)" -AsSecureString)).Password
  & "$dir\config.cmd" --unattended --url "https://github.com/$r" --token $tok --name "$($env:COMPUTERNAME)-$(Slug $r)" `
   @labelArgs --work _work --replace$ephFlag --runasservice --windowslogonaccount ".\$Account" --windowslogonpassword $password
  $rc = $LASTEXITCODE
  $tok = $null
  if ($rc -ne 0) { throw "config.cmd exited $rc for $r" }
  # 6. Read it back.
  $svc = Find-RunnerService $dir
  if (-not $svc) { throw "no runner service found under $dir after configure" }
  if ($svc.StartName -ine ".\$Account") { throw "service runs as '$($svc.StartName)', expected .\$Account" }
  $envBack = @(Get-Content -LiteralPath "$dir\.env")
  foreach ($l in $runnerEnv) { if ($envBack -notcontains $l) { throw "$dir\.env does not hold the line: $l" } }
  Write-Host "[$r] service $($svc.Name) as $($svc.StartName), state $($svc.State); .env holds $($runnerEnv.Count) line(s) ($(($runnerEnv | ForEach-Object { ($_ -split '=', 2)[0] }) -join ', '))"
  # 7. The probe: what the first job would hit, run now AS the account with the .env's values, so a bad PATH or an unreadable toolchain
  # fails here, loudly, instead of in a queued job (mlmf 2026-11-12). Output goes to a file the account can write (its root).
  if ($GitRoot) { foreach ($x in "$GitRoot\bin\bash.exe") { if (-not (Test-Path -LiteralPath $x)) { throw "probe: $x does not exist" } } }
  if ($RustRoot) { New-Item -ItemType Directory -Force -Path $cargoHome | Out-Null }
  $probeOut = Join-Path $Root 'probe-out.txt'; $probeErr = Join-Path $Root 'probe-err.txt'; $probePs1 = Join-Path $Root 'probe.ps1'
  $probeLines = @('$ErrorActionPreference = ''Continue''')
  foreach ($l in $runnerEnv) { $kv = $l -split '=', 2; $probeLines += "`$env:$($kv[0]) = '$($kv[1] -replace "'", "''")'" }
  $probeLines += @(
    '$w = (where.exe bash 2>$null | Select-Object -First 1); "bash-path=$w"',
    '"bash-run=" + (& bash -c "echo ok" 2>&1 | Select-Object -First 1)')
  if ($RustRoot) { $probeLines += @('"cargo=" + (& cargo --version 2>&1 | Select-Object -First 1)', '"rustc=" + (& rustc --version 2>&1 | Select-Object -First 1)') }
  Set-Content -LiteralPath $probePs1 -Value $probeLines -Encoding Ascii
  Remove-Item -LiteralPath $probeOut, $probeErr -ErrorAction SilentlyContinue
  Set-Service -Name seclogon -StartupType Manual -ErrorAction SilentlyContinue; Start-Service -Name seclogon -ErrorAction SilentlyContinue
  $cred = New-Object System.Management.Automation.PSCredential(".\$Account", $secure)
  $pr = Start-Process -FilePath "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" -Credential $cred -LoadUserProfile -WorkingDirectory $Root -Wait -PassThru -WindowStyle Hidden `
    -ArgumentList @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', "`"$probePs1`"") -RedirectStandardOutput $probeOut -RedirectStandardError $probeErr
  $probeText = if (Test-Path -LiteralPath $probeOut) { Get-Content -LiteralPath $probeOut -Raw } else { '' }
  $probeFail = @(Test-ProbeOutput $probeText $GitRoot ([bool]$RustRoot))
  Remove-Item -LiteralPath $probePs1 -ErrorAction SilentlyContinue
  if ($probeFail.Count -gt 0) {
    $err = if (Test-Path -LiteralPath $probeErr) { (Get-Content -LiteralPath $probeErr -Raw) } else { '' }
    throw "PROBE FAILED for ${r} as ${Account} (probe exit $($pr.ExitCode)): $($probeFail -join '; '). Probe output: $probeText $err The runner is registered but a job on it would fail the same way."
  }
  Write-Host "[$r] probe as ${Account} OK: $(($probeText -split "`r?`n" | Where-Object { $_.Trim() }) -join ' | ')"
}
$password = $null; $secure = $null
Write-Host "`nNext: gh api repos/<owner>/<repo>/actions/runners  (labels must read self-hosted, Windows, X64, gpu), then run the spike (docs/RUNNER-SETUP.md section 5)."
