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
       32 characters generated in memory, passed to New-LocalUser and to the service configuration, never written to a
       file, never printed, never logged. An existing account is kept and its password is ROTATED the same way, so
       nobody ever knows it. (The password is visible in the process command line of config.cmd for a moment, to
       administrators only; the runner stores it in the service control manager.)
    3. Creates -Root (default C:\gh-runner) with an ACL of SYSTEM, Administrators and the account only.
    4. Downloads the pinned official runner release and VERIFIES its SHA256 against the pinned value before
       extracting a byte of it. A mismatch deletes the download and stops.
    5. Per repo: extracts to <Root>\<owner>-<repo>, asks for that repo's registration token (Read-Host -AsSecureString:
       the PM fetches it at run time, it expires in an hour, it is used once and never stored), and runs config.cmd
       with labels self-hosted,windows,x64 (automatic) plus -Labels, as a Windows service under the account.
    6. Reads the service back (name, StartName, state): an exit code is not a state read.
  Triggers are the workflow's business, not the runner's: see docs/RUNNER-SETUP.md section 6. This script cannot
  stop a repo from adding a pull_request job that runs on the runner; the lint and the repo setting do.

  Service mode only. Whether CUDA and Vulkan work from a service (session 0) is a measurement the spike workflow
  makes (docs/RUNNER-SETUP.md section 5); until it passes the runner is not trusted with GPU work.

.PARAMETER Repo          One or more owner/name repos to register the runner for. Required.
.PARAMETER Owners        Account names whose repos may get a runner. Default ciresnave, ThinkersJournal.
.PARAMETER Private       The -Repo entries that are PRIVATE (only these may use -Ephemeral:$false). -Apply verifies each with `gh api`.
.PARAMETER Account       Local account name. Default gh-runner.
.PARAMETER Root          Install root, ASCII path. Default C:\gh-runner.
.PARAMETER Labels        Extra runner labels, added to the automatic self-hosted, Windows, X64. Default NONE: add gpu only for a repo whose jobs need the GPU, so a plain-Windows job cannot queue behind (or be mistaken for) a GPU one.
.PARAMETER RunnerVersion Runner release to install. Pinned with its hash; change both together.
.PARAMETER RunnerSha256  SHA256 of actions-runner-win-x64-<RunnerVersion>.zip, from the release notes.
.PARAMETER Ephemeral    Register with --ephemeral: the runner takes ONE job, then unregisters and its service stops, so a job cannot leave anything for the next. REQUIRED for the public repos (docs/RUNNER-SETUP.md section 2a); -Ephemeral:$false is refused for them. Default (board 223, CireSnave 2026-10-16): PERSISTENT when every -Repo is named in -Private, EPHEMERAL otherwise (any public or undeclared repo in the call makes the whole call ephemeral). Pass the switch to override either way.
.PARAMETER ProtectedPaths  Paths the runner account must NOT be able to write, directly or through a broad group: the lane tools that run as the owner WITH vault access. Default C:\Projects, C:\Projects\.claude-hooks, with-secret.exe, lane-restart.exe, C:\Projects\.lane-state. -Apply refuses to create the account while any is writable by Everyone, Authenticated Users, Users, Interactive or the account itself (board 224).
.PARAMETER AlsoGrant     Groups or accounts to keep READ-AND-EXECUTE (never write) on the protected paths in the printed icacls fix, e.g. -AlsoGrant CodexSandboxUsers. Only changes the text printed; Everyone, Authenticated Users, Users and Interactive are refused (they are the hole).
.PARAMETER CheckAclsOnly Read-only: run the ACL check, print the exact elevated fix commands for what it finds, and throw if anything is exposed. Nothing else happens.
.PARAMETER Apply         Actually do it. Without it, nothing is changed.
.PARAMETER Remove        With -Apply: unregister and remove the runners for -Repo (asks for a removal token) and leave the account.
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory)][string[]]$Repo,
  [string[]]$Owners = @('ciresnave', 'ThinkersJournal'),
  [string[]]$Private = @(),
  [string]$Account = 'gh-runner',
  [string]$Root = 'C:\gh-runner',
  [string[]]$Labels = @(),
  [string]$RunnerVersion = '2.338.0',
  [string]$RunnerSha256 = 'f48e0750a21812bca5f82de5f7f5aeae71abee647fab5a582f1742d07eba455f',
  [Nullable[bool]]$Ephemeral = $null,
  [string[]]$ProtectedPaths = @('C:\Projects', 'C:\Projects\.claude-hooks', 'C:\Projects\.claude-hooks\with-secret.exe', 'C:\Projects\.claude-hooks\lane-restart.exe', 'C:\Projects\.lane-state'),
  [string[]]$AlsoGrant = @(),
  [switch]$CheckAclsOnly,
  [switch]$Apply,
  [switch]$Remove
)
$ErrorActionPreference = 'Stop'

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
function Slug([string]$r) { $r -replace '/', '-' }
function Svc([string]$r) { 'actions.runner.' + (Slug $r) + '.' + $env:COMPUTERNAME + '-' + (Slug $r) }

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
  Write-Host "Account       : $env:COMPUTERNAME\$Account (standard user; random 32-char password held in memory only)"
  Write-Host "Root          : $Root  (ACL: SYSTEM, Administrators, $Account)"
  Write-Host "Runner        : v$RunnerVersion  SHA256 $RunnerSha256`n  $zipUrl"
  if ($aclFindings.Count -gt 0) { Write-Host "`nACL PREFLIGHT WOULD REFUSE -Apply:"; Show-AclFix $aclFindings; Write-Host "" }
  else { Write-Host "ACL preflight: clean (-Apply would proceed past it)" }
  foreach ($r in $Repo) {
    $dir = Join-Path $Root (Slug $r)
    Write-Host "`n[$r]"
    Write-Host "  extract  -> $dir (only after the hash matches)"
    Write-Host "  token    -> prompted (Read-Host -AsSecureString): PM fetches it now; not stored"
    if ($Remove) { Write-Host "  $dir\config.cmd remove --token <removal token>" }
    else {
      Write-Host "  $dir\config.cmd --unattended --url https://github.com/$r --token <token> --name $($env:COMPUTERNAME)-$(Slug $r) ``"
      Write-Host "     $labelFlag --work _work --replace$ephFlag --runasservice --windowslogonaccount .\$Account --windowslogonpassword <generated>"
      Write-Host "  read back: service ... StartName = .\$Account, State = Running"
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

function New-RandomPassword([int]$len = 32) {
  $chars = 'ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789!#%+-=?@^_'
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
    $tok = [Net.NetworkCredential]::new('', (Read-Host "Removal token for $r" -AsSecureString)).Password
    & "$dir\config.cmd" remove --token $tok
    if ($LASTEXITCODE -ne 0) { throw "config.cmd remove exited $LASTEXITCODE for $r" }
    $tok = $null
    if (Get-Service -Name (Svc $r) -ErrorAction SilentlyContinue) { throw "service $(Svc $r) still exists after remove" }
    Write-Host "[$r] removed (read back: service absent). Directory $dir kept for you to delete."
  }
  return
}

# 1b. Visibility is read from GitHub, never taken on trust: a repo declared private that is public is refused.
foreach ($r in $Repo) {
  $isPrivate = (gh api "repos/$r" --jq .private 2>&1 | Out-String).Trim()
  if ($isPrivate -ne 'true' -and $isPrivate -ne 'false') { throw "cannot read visibility of $r from gh api: $isPrivate" }
  if (($Private -contains $r) -and $isPrivate -ne 'true') { throw "$r is declared -Private but GitHub says it is public" }
}

# 2. The account.
$password = New-RandomPassword
$secure = ConvertTo-SecureString $password -AsPlainText -Force
$existing = Get-LocalUser -Name $Account -ErrorAction SilentlyContinue
if ($existing) {
  Set-LocalUser -Name $Account -Password $secure
  Write-Host "account $Account exists: password rotated (nobody knows it)"
} else {
  New-LocalUser -Name $Account -Password $secure -PasswordNeverExpires -UserMayNotChangePassword `
    -AccountNeverExpires -Description 'GitHub Actions runner (agentlife scripts/gh-runner-setup.ps1)' | Out-Null
  Write-Host "account $Account created"
}
Add-LocalGroupMember -Group 'Users' -Member $Account -ErrorAction SilentlyContinue
$admins = @(Get-LocalGroupMember -Group 'Administrators' | ForEach-Object { $_.Name })
if ($admins | Where-Object { $_ -ieq "$env:COMPUTERNAME\$Account" }) { throw "$Account is in Administrators; refusing. Remove it and run again." }
$rdp = @(Get-LocalGroupMember -Group 'Remote Desktop Users' -ErrorAction SilentlyContinue | ForEach-Object { $_.Name })
if ($rdp | Where-Object { $_ -ieq "$env:COMPUTERNAME\$Account" }) { throw "$Account may log on by Remote Desktop; refusing. Remove it and run again." }

# 3. The root.
New-Item -ItemType Directory -Force -Path $Root | Out-Null
icacls $Root /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' "${env:COMPUTERNAME}\${Account}:(OI)(CI)M" | Out-Null
if ($LASTEXITCODE -ne 0) { throw "icacls on $Root exited $LASTEXITCODE" }

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
  $tok = [Net.NetworkCredential]::new('', (Read-Host "Registration token for $r (not stored)" -AsSecureString)).Password
  & "$dir\config.cmd" --unattended --url "https://github.com/$r" --token $tok --name "$($env:COMPUTERNAME)-$(Slug $r)" `
   @labelArgs --work _work --replace$ephFlag --runasservice --windowslogonaccount ".\$Account" --windowslogonpassword $password
  $rc = $LASTEXITCODE
  $tok = $null
  if ($rc -ne 0) { throw "config.cmd exited $rc for $r" }
  # 6. Read it back.
  $svc = Get-CimInstance Win32_Service -Filter "Name='$(Svc $r)'"
  if (-not $svc) { throw "service $(Svc $r) not found after configure" }
  if ($svc.StartName -ine ".\$Account") { throw "service runs as '$($svc.StartName)', expected .\$Account" }
  Write-Host "[$r] service $($svc.Name) as $($svc.StartName), state $($svc.State)"
}
$password = $null; $secure = $null
Write-Host "`nNext: gh api repos/<owner>/<repo>/actions/runners  (labels must read self-hosted, Windows, X64, gpu), then run the spike (docs/RUNNER-SETUP.md section 5)."
