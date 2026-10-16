<#
.SYNOPSIS
  Set up self-hosted GitHub Actions runners for trusted repos, under a restricted local account (docs/RUNNER-SETUP.md).

.DESCRIPTION
  DRY RUN BY DEFAULT: with no -Apply it prints the plan and the exact commands and changes nothing (no network,
  no account, no files). -Apply needs an ELEVATED PowerShell; the PM and CireSnave run it after review, a lane never does.

  What -Apply does, in order, and refuses at the first failure:
    1. Refuses any -Repo not on the trusted list (-TrustedRepos). A runner runs whatever its repo's workflows say, so
       the list is the whole security boundary: trusted repos only, never one that takes untrusted PRs.
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
.PARAMETER TrustedRepos  The repos a runner may be registered for. Default: ciresnave/fuel, baracuda, lightbulb.
.PARAMETER Account       Local account name. Default gh-runner.
.PARAMETER Root          Install root, ASCII path. Default C:\gh-runner.
.PARAMETER Labels        Extra runner labels. Default gpu.
.PARAMETER RunnerVersion Runner release to install. Pinned with its hash; change both together.
.PARAMETER RunnerSha256  SHA256 of actions-runner-win-x64-<RunnerVersion>.zip, from the release notes.
.PARAMETER Ephemeral    Register with --ephemeral (default ON): the runner takes ONE job, then unregisters and its service stops, so a job cannot leave anything for the next. REQUIRED for the public repos (docs/RUNNER-SETUP.md section 2a); -Ephemeral:$false is refused for them.
.PARAMETER Apply         Actually do it. Without it, nothing is changed.
.PARAMETER Remove        With -Apply: unregister and remove the runners for -Repo (asks for a removal token) and leave the account.
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory)][string[]]$Repo,
  [string[]]$TrustedRepos = @('ciresnave/fuel', 'ciresnave/baracuda', 'ciresnave/lightbulb'),
  [string]$Account = 'gh-runner',
  [string]$Root = 'C:\gh-runner',
  [string[]]$Labels = @('gpu'),
  [string]$RunnerVersion = '2.338.0',
  [string]$RunnerSha256 = 'f48e0750a21812bca5f82de5f7f5aeae71abee647fab5a582f1742d07eba455f',
  [bool]$Ephemeral = $true,
  [switch]$Apply,
  [switch]$Remove
)
$ErrorActionPreference = 'Stop'

$nameRe = '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$'
foreach ($r in $Repo) {
  if ($r -notmatch $nameRe) { throw "Repo '$r' is not owner/name" }
  if ($TrustedRepos -notcontains $r) { throw "Repo '$r' is not on the trusted list ($($TrustedRepos -join ', ')): a runner runs that repo's workflows as $Account" }
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
# Every default trusted repo is PUBLIC (PM 2026-10-16): anyone can open a PR there, so a persistent runner is refused.
$PublicRepos = @('ciresnave/fuel', 'ciresnave/baracuda', 'ciresnave/lightbulb')
if (-not $Ephemeral) {
  $pub = @($Repo | Where-Object { $PublicRepos -contains $_ })
  if ($pub) { throw "-Ephemeral:`$false is refused for public repo(s): $($pub -join ', ')" }
}
$ephFlag = if ($Ephemeral) { ' --ephemeral' } else { '' }
function Slug([string]$r) { $r -replace '/', '-' }
function Svc([string]$r) { 'actions.runner.' + (Slug $r) + '.' + $env:COMPUTERNAME + '-' + (Slug $r) }

if (-not $Apply) {
  Write-Host "DRY RUN (nothing changed, no network). Pass -Apply from an elevated shell to do this.`n"
  Write-Host "Trusted repos : $($TrustedRepos -join ', ')"
  Write-Host "Account       : $env:COMPUTERNAME\$Account (standard user; random 32-char password held in memory only)"
  Write-Host "Root          : $Root  (ACL: SYSTEM, Administrators, $Account)"
  Write-Host "Runner        : v$RunnerVersion  SHA256 $RunnerSha256`n  $zipUrl"
  foreach ($r in $Repo) {
    $dir = Join-Path $Root (Slug $r)
    Write-Host "`n[$r]"
    Write-Host "  extract  -> $dir (only after the hash matches)"
    Write-Host "  token    -> prompted (Read-Host -AsSecureString): PM fetches it now; not stored"
    if ($Remove) { Write-Host "  $dir\config.cmd remove --token <removal token>" }
    else {
      Write-Host "  $dir\config.cmd --unattended --url https://github.com/$r --token <token> --name $($env:COMPUTERNAME)-$(Slug $r) ``"
      Write-Host "      --labels $allLabels --work _work --replace$ephFlag --runasservice --windowslogonaccount .\$Account --windowslogonpassword <generated>"
      Write-Host "  read back: service ... StartName = .\$Account, State = Running"
    }
  }
  return
}

$id = [Security.Principal.WindowsIdentity]::GetCurrent()
if (-not ([Security.Principal.WindowsPrincipal]$id).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
  throw '-Apply needs an elevated PowerShell'
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
    --labels $allLabels --work _work --replace$ephFlag --runasservice --windowslogonaccount ".\$Account" --windowslogonpassword $password
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
