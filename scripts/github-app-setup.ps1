# SPDX-License-Identifier: MIT OR Apache-2.0
#Requires -Version 7.0
<#
.SYNOPSIS
  Creates the runner GitHub App through GitHub's manifest flow and stores its private key (roadmap C1;
  CireSnave 2026-10-22: "lets lean toward GitHub apps anywhere tokens are required").

.DESCRIPTION
  DRY RUN BY DEFAULT: with no switch it prints the manifest it would send and changes nothing.

    -Apply   Starts a loopback listener, opens the browser on it, and waits. THE HUMAN CLICKS TWICE:
               click 1: GitHub's "Create GitHub App for <org>" page (it shows the permissions to approve);
               click 2: "Install" on the organization (printed at the end; this script cannot do it).
             On GitHub's redirect the script checks the one-time `state`, exchanges the code
             (POST /app-manifests/<code>/conversions, unauthenticated, the code is single use and lasts an hour),
             and stores the private key PROTECTED FOR THE CURRENT WINDOWS ACCOUNT (DPAPI user scope) in
             -KeyDir, with an owner-only ACL. Run it AS THE WATCHER ACCOUNT, outside any sandbox: only that
             account can decrypt the key, and the runner account (`gh-runner`) never sees it.
    -Verify  Proves the stored key and the install end to end without printing a token: signs a short-lived JWT,
             asks GitHub for the app and its installations, mints one installation token per installation,
             checks the granted permissions, REVOKES the token, and records the installation id (not a secret).

  The private key, the client secret and the webhook secret are never printed, logged or put in an argument or
  an environment variable. The client and webhook secrets are discarded (this app has no webhook and no OAuth);
  the key exists only as the DPAPI blob. If writing the blob fails after GitHub returned the key, it is lost:
  generate a new key on the app's settings page and rerun with -Replace.

  Permissions requested (the page shows them; read them before click 1):
    organization_self_hosted_runners: write   register and remove runners for the whole organization
    metadata: read                            always present
    -RepoAdminFallback adds `administration: write` (a repo-level fallback for ciresnave-owned repos that
    have not moved to the organization yet; skip it once they have, roadmap C4).
  The app is private ("only on this account"), has no webhook and subscribes to no events.

  The honest limit: DPAPI user scope stops a copied disk and other accounts, not a process running as the
  watcher account (the same limit as the vault, docs/DESIGN.md). The key can mint tokens only for this app's
  installation, and installation tokens last one hour.

.PARAMETER Org           Organization that owns the app. Default ThinkersJournal.
.PARAMETER AppName       The app's name (unique across GitHub, at most 34 characters). Default "AntSpawn Runner".
.PARAMETER KeyDir        Where the DPAPI blob and the non-secret <slug>.json go. Default %LOCALAPPDATA%\antspawn\github-app.
.PARAMETER RepoAdminFallback  Also request repository Administration: write.
.PARAMETER GitHubWeb     Default https://github.com. Only that, or a loopback http address (tests).
.PARAMETER GitHubApi     Default https://api.github.com. Only that, or a loopback http address (tests).
.PARAMETER TimeoutSeconds  How long -Apply waits for the redirect. Default 600.
.PARAMETER NoBrowser     Print START_URL=<url> instead of opening the browser.
.PARAMETER Replace       Allow -Apply when a key already exists in -KeyDir (it is overwritten).
.PARAMETER Slug          With -Verify: which app's key (needed only if -KeyDir holds more than one).
#>
[CmdletBinding()]
param(
  [string]$Org = 'ThinkersJournal',
  [string]$AppName = 'AntSpawn Runner',
  [string]$KeyDir = (Join-Path $env:LOCALAPPDATA 'antspawn\github-app'),
  [switch]$RepoAdminFallback,
  [string]$GitHubWeb = 'https://github.com',
  [string]$GitHubApi = 'https://api.github.com',
  [int]$TimeoutSeconds = 600,
  [switch]$NoBrowser,
  [switch]$Apply,
  [switch]$Replace,
  [switch]$Verify,
  [string]$Slug
)
$ErrorActionPreference = 'Stop'

if ($Apply -and $Verify) { throw '-Apply and -Verify are exclusive' }
if ($Org -notmatch '^[A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?$') { throw "-Org '$Org' is not a GitHub organization name" }
if ($AppName -notmatch '^[A-Za-z0-9][A-Za-z0-9 ._-]{0,33}$') { throw "-AppName must be 1-34 characters of letters, digits, space, '.', '_' or '-'" }
if ($Slug -and $Slug -notmatch '^[a-z0-9][a-z0-9-]{0,63}$') { throw "-Slug '$Slug' is not an app slug" }
if ($TimeoutSeconds -lt 5 -or $TimeoutSeconds -gt 3600) { throw '-TimeoutSeconds must be 5..3600' }

# Only the real GitHub, or a loopback http address (so a test can stand in for it): the key exchange and the
# JWT must never be sent anywhere else, whatever a caller passes.
function Assert-Endpoint([string]$name, [string]$value, [string]$official) {
  $v = $value.TrimEnd('/')
  if ($v -ne $official -and $v -notmatch '^http://(127\.0\.0\.1|localhost):\d{2,5}$') {
    throw "$name '$value' is neither $official nor a loopback http address"
  }
  return $v
}
$GitHubWeb = Assert-Endpoint '-GitHubWeb' $GitHubWeb 'https://github.com'
$GitHubApi = Assert-Endpoint '-GitHubApi' $GitHubApi 'https://api.github.com'

$me = "$env:USERDOMAIN\$env:USERNAME"
$UserAgent = 'antspawn-github-app-setup'

function New-Manifest([string]$redirect) {
  $perms = [ordered]@{ organization_self_hosted_runners = 'write'; metadata = 'read' }
  if ($RepoAdminFallback) { $perms['administration'] = 'write' }
  return [ordered]@{
    name                     = $AppName
    url                      = "https://github.com/$Org"
    description              = 'Mints short-lived installation tokens so a watcher can register one-job (JIT) self-hosted runners. Created by scripts/github-app-setup.ps1 (agentlife).'
    redirect_url             = $redirect
    public                   = $false
    default_events           = @()
    default_permissions      = $perms
    request_oauth_on_install = $false
    setup_on_update          = $false
  }
}

function Get-KeyFiles { if (Test-Path -LiteralPath $KeyDir) { Get-ChildItem -LiteralPath $KeyDir -Filter '*.key.dpapi' -File } else { @() } }

# ---------------------------------------------------------------------------------------------- -Verify
if ($Verify) {
  $keys = @(Get-KeyFiles)
  if ($Slug) { $keys = @($keys | Where-Object { $_.Name -eq "$Slug.key.dpapi" }) }
  if ($keys.Count -eq 0) { throw "no key in $KeyDir$(if ($Slug) { " for slug $Slug" }): run with -Apply first" }
  if ($keys.Count -gt 1) { throw "$($keys.Count) keys in ${KeyDir}: pass -Slug <slug>" }
  $slug = $keys[0].Name -replace '\.key\.dpapi$', ''
  $metaPath = Join-Path $KeyDir "$slug.json"
  $meta = Get-Content -LiteralPath $metaPath -Raw | ConvertFrom-Json
  $appId = [string]$meta.app_id

  function ConvertTo-B64Url([byte[]]$bytes) { [Convert]::ToBase64String($bytes).TrimEnd('=').Replace('+', '-').Replace('/', '_') }
  # The PEM lives only in this scope; it is turned into an RSA object and dropped at once.
  $secure = ConvertTo-SecureString -String ((Get-Content -LiteralPath $keys[0].FullName -Raw).Trim())
  $rsa = [System.Security.Cryptography.RSA]::Create()
  $rsa.ImportFromPem([System.Net.NetworkCredential]::new('', $secure).Password)
  $now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
  $head = ConvertTo-B64Url ([Text.Encoding]::UTF8.GetBytes('{"alg":"RS256","typ":"JWT"}'))
  $body = ConvertTo-B64Url ([Text.Encoding]::UTF8.GetBytes((@{ iat = $now - 60; exp = $now + 540; iss = $appId } | ConvertTo-Json -Compress)))
  $sig = ConvertTo-B64Url ($rsa.SignData([Text.Encoding]::UTF8.GetBytes("$head.$body"), [System.Security.Cryptography.HashAlgorithmName]::SHA256, [System.Security.Cryptography.RSASignaturePadding]::Pkcs1))
  $rsa.Dispose()
  $jwt = "$head.$body.$sig"
  $hdr = @{ Accept = 'application/vnd.github+json'; 'User-Agent' = $UserAgent; 'X-GitHub-Api-Version' = '2022-11-28' }

  $app = Invoke-RestMethod -Method Get -Uri "$GitHubApi/app" -Headers ($hdr + @{ Authorization = "Bearer $jwt" })
  Write-Host "app: $($app.slug) (id $($app.id))"
  # An empty JSON array can arrive as one empty object: flatten, and keep only real installations.
  $installs = @(Invoke-RestMethod -Method Get -Uri "$GitHubApi/app/installations" -Headers ($hdr + @{ Authorization = "Bearer $jwt" }) |
      ForEach-Object { $_ } | Where-Object { $_ -and $_.id })
  if ($installs.Count -eq 0) { throw "app $($app.slug) is not installed anywhere: click 2 (Install on the organization) is still to do: $GitHubWeb/apps/$($app.slug)/installations/new" }
  $required = [ordered]@{ organization_self_hosted_runners = 'write'; metadata = 'read' }
  $problems = New-Object System.Collections.Generic.List[string]
  $orgInstall = $null
  foreach ($i in $installs) {
    $login = $i.account.login
    $tok = Invoke-RestMethod -Method Post -Uri "$GitHubApi/app/installations/$($i.id)/access_tokens" -Headers ($hdr + @{ Authorization = "Bearer $jwt" })
    $granted = $tok.permissions
    $perm = ($granted.PSObject.Properties | ForEach-Object { "$($_.Name)=$($_.Value)" }) -join ', '
    Write-Host "installation $($i.id) on $login ($($i.target_type)): $perm; token expires $($tok.expires_at)"
    # Revoke at once: a verify must not leave a live token behind. The token itself is never printed.
    Invoke-RestMethod -Method Delete -Uri "$GitHubApi/installation/token" -Headers ($hdr + @{ Authorization = "Bearer $($tok.token)" }) | Out-Null
    $tok = $null
    if ($login -eq $Org) {
      $orgInstall = $i.id
      foreach ($k in $required.Keys) {
        $have = $granted.$k
        $okLevel = ($have -eq $required[$k]) -or ($required[$k] -eq 'read' -and $have -eq 'write')
        if (-not $okLevel) { $problems.Add("$k should be $($required[$k]) on $login, is '$have'") }
      }
    }
  }
  $jwt = $null
  if (-not $orgInstall) { $problems.Add("no installation on $Org") }
  if ($problems.Count -gt 0) { throw ("verify FAILED: " + ($problems -join '; ')) }
  $meta | Add-Member -NotePropertyName installation_id -NotePropertyValue $orgInstall -Force
  $meta | ConvertTo-Json | Set-Content -LiteralPath $metaPath -Encoding utf8
  Write-Host "verify ok: app $($app.slug), installation $orgInstall on $Org has the permissions asked for; token revoked; installation_id recorded in $metaPath"
  return
}

# ---------------------------------------------------------------------------------------------- dry run
if (-not $Apply) {
  Write-Host 'DRY RUN (nothing is created or written). Pass -Apply to start the flow. The manifest would be:'
  (New-Manifest 'http://127.0.0.1:<port>/callback') | ConvertTo-Json -Depth 5
  Write-Host "Posted to: $GitHubWeb/organizations/$Org/settings/apps/new?state=<one-time>"
  Write-Host "Key would be stored (DPAPI, account $me) in: $KeyDir"
  return
}

# ---------------------------------------------------------------------------------------------- -Apply
if (-not $Replace -and @(Get-KeyFiles).Count -gt 0) {
  throw "a key already exists in $KeyDir ($((Get-KeyFiles | Select-Object -First 1).Name)): creating another app would orphan it. Use -Replace to overwrite."
}
New-Item -ItemType Directory -Force -Path $KeyDir | Out-Null
icacls $KeyDir /inheritance:r /grant:r "${me}:(OI)(CI)F" | Out-Null
if ($LASTEXITCODE -ne 0) { throw "icacls on $KeyDir exited $LASTEXITCODE" }

$probe = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$probe.Start(); $port = $probe.LocalEndpoint.Port; $probe.Stop()
$state = -join ((1..32) | ForEach-Object { '{0:x2}' -f (Get-Random -Maximum 256) })
$manifestJson = (New-Manifest "http://127.0.0.1:$port/callback") | ConvertTo-Json -Depth 5 -Compress
$action = "$GitHubWeb/organizations/$Org/settings/apps/new?state=$state"
$page = @"
<!doctype html><html><head><meta charset="utf-8"><title>Create the GitHub App</title></head><body>
<p>Sending the app manifest to GitHub. If nothing happens, press the button.</p>
<form id="f" method="post" action="$([System.Net.WebUtility]::HtmlEncode($action))">
<input type="hidden" name="manifest" value="$([System.Net.WebUtility]::HtmlEncode($manifestJson))">
<button type="submit">Continue to GitHub</button></form>
<script>document.getElementById('f').submit();</script></body></html>
"@

$listener = [System.Net.HttpListener]::new()
$listener.Prefixes.Add("http://127.0.0.1:$port/")
$listener.Start()
$startUrl = "http://127.0.0.1:$port/start"
Write-Host "START_URL=$startUrl"
if (-not $NoBrowser) { Start-Process $startUrl }
Write-Host "Waiting up to $TimeoutSeconds s. Click 1 is on GitHub's page (read the permissions it lists first)."

function Send-Reply($ctx, [int]$status, [string]$text, [string]$type = 'text/plain; charset=utf-8') {
  $bytes = [Text.Encoding]::UTF8.GetBytes($text)
  $ctx.Response.StatusCode = $status
  $ctx.Response.ContentType = $type
  $ctx.Response.OutputStream.Write($bytes, 0, $bytes.Length)
  $ctx.Response.Close()
}

$code = $null
$pending = $null
$deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
try {
  while (-not $code) {
    $left = ($deadline - [DateTime]::UtcNow).TotalMilliseconds
    if ($left -le 0) { throw "timed out after $TimeoutSeconds s waiting for GitHub's redirect: nothing was created here" }
    # ONE outstanding accept at a time, kept across polls: a task abandoned after a 2 s timeout still collects the next
    # request and drops it (the browser's redirect from GitHub arrives long after 2 s, and would never be seen).
    if ($null -eq $pending) { $pending = $listener.GetContextAsync() }
    if (-not $pending.Wait([int][Math]::Min($left, 2000))) { continue }
    $ctx = $pending.Result
    $pending = $null
    $req = $ctx.Request
    # Loopback only, by the Host header too (a DNS-rebinding page must not be able to talk to this).
    if ($req.Headers['Host'] -ne "127.0.0.1:$port" -or $req.HttpMethod -ne 'GET') { Send-Reply $ctx 400 'bad request'; continue }
    switch ($req.Url.AbsolutePath) {
      '/start' { Send-Reply $ctx 200 $page 'text/html; charset=utf-8' }
      '/callback' {
        $q = [System.Web.HttpUtility]::ParseQueryString($req.Url.Query)
        $given = [Text.Encoding]::UTF8.GetBytes([string]$q['state'])
        $want = [Text.Encoding]::UTF8.GetBytes($state)
        $stateOk = ($given.Length -eq $want.Length) -and [System.Security.Cryptography.CryptographicOperations]::FixedTimeEquals($given, $want)
        if (-not $stateOk) { Send-Reply $ctx 400 'state mismatch: ignored'; continue }
        if ([string]$q['code'] -notmatch '^[A-Za-z0-9]{8,64}$') { Send-Reply $ctx 400 'no usable code: ignored'; continue }
        $code = [string]$q['code']
        Send-Reply $ctx 200 'Done. You can close this tab and go back to the terminal. Next: click 2, install the app on the organization.'
      }
      default { Send-Reply $ctx 404 'not found' }
    }
  }
}
finally {
  # Let the browser finish reading the last reply before the listener goes away.
  Start-Sleep -Milliseconds 300
  $listener.Stop(); $listener.Close()
}

Write-Host 'Redirect received with the right state; exchanging the code.'
$hdr = @{ Accept = 'application/vnd.github+json'; 'User-Agent' = $UserAgent; 'X-GitHub-Api-Version' = '2022-11-28' }
try { $made = Invoke-RestMethod -Method Post -Uri "$GitHubApi/app-manifests/$code/conversions" -Headers $hdr }
catch { throw "GitHub refused the code exchange: $($_.Exception.Message)" }
if (-not $made.pem -or -not $made.slug -or -not $made.id) { throw 'GitHub answered the exchange without an id, slug and key' }
$slug = [string]$made.slug
if ($slug -notmatch '^[a-z0-9][a-z0-9-]{0,63}$') { throw 'GitHub returned an unusable slug' }

# The key goes straight into the DPAPI blob; nothing else holds it, and nothing prints it.
$blob = ConvertFrom-SecureString (ConvertTo-SecureString -String ([string]$made.pem) -AsPlainText -Force)
$keyPath = Join-Path $KeyDir "$slug.key.dpapi"
$tmp = "$keyPath.tmp"
Set-Content -LiteralPath $tmp -Value $blob -Encoding ascii -NoNewline
Move-Item -LiteralPath $tmp -Destination $keyPath -Force
$info = [ordered]@{
  app_id       = $made.id
  slug         = $slug
  name         = $made.name
  org          = $Org
  html_url     = $made.html_url
  created_utc  = [DateTime]::UtcNow.ToString('o')
  permissions  = (New-Manifest 'x').default_permissions
  key_file     = $keyPath
  protected_by = "DPAPI, user scope, account $me"
}
$info | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $KeyDir "$slug.json") -Encoding utf8
$made = $null; $blob = $null

Write-Host "created app '$slug' (id $($info.app_id)); private key stored protected for $me at $keyPath (not printed)."
Write-Host "Click 2: install it on the organization: $GitHubWeb/apps/$slug/installations/new"
Write-Host "Then prove it: pwsh -File scripts\github-app-setup.ps1 -Verify -Org $Org -KeyDir '$KeyDir'"
