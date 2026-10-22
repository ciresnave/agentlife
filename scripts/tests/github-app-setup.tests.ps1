#Requires -Version 7.0
<#
.SYNOPSIS
  Tests scripts\github-app-setup.ps1 (roadmap C1) against a local stand-in for GitHub: the manifest page, the
  state check, the code exchange, DPAPI storage, the key never being printed, and -Verify (including its
  known-bad controls). Needs no elevation, no network and no GitHub account; writes only to a temp directory.
  Exit 0 = all passed. Run by tests\github_app_setup.rs on the Windows CI leg, or by hand:
    pwsh -NoProfile -File scripts\tests\github-app-setup.tests.ps1
#>
$ErrorActionPreference = 'Stop'
$setup = Join-Path (Split-Path -Parent $PSScriptRoot) 'github-app-setup.ps1'
if (-not (Test-Path -LiteralPath $setup)) { throw "setup script not found: $setup" }
$pwsh = (Get-Process -Id $PID).Path
$work = Join-Path ([IO.Path]::GetTempPath()) ("ghapp-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $work | Out-Null
$failures = New-Object System.Collections.Generic.List[string]
$procs = @()

function Check([string]$name, [bool]$ok, [string]$detail = '') {
  if ($ok) { Write-Host "ok   $name" } else { Write-Host "FAIL $name $detail"; $failures.Add($name) }
}

function Q([string[]]$a) { $a | ForEach-Object { if ($_ -match '\s') { '"' + $_ + '"' } else { $_ } } }

# --- the stand-in for GitHub -----------------------------------------------------------------------
# Serves /app-manifests/<code>/conversions, /app, /app/installations, /app/installations/<id>/access_tokens and
# DELETE /installation/token, verifying the app JWT's RS256 signature (so -Verify's signing is really checked).
# mode.txt (read per request): ok | noperm | wrongkey | noinstall.
$fake = @'
param([string]$Dir)
$ErrorActionPreference = 'Stop'
$rsa = [Security.Cryptography.RSA]::Create(2048)
$other = [Security.Cryptography.RSA]::Create(2048)
$pem = $rsa.ExportRSAPrivateKeyPem()
$probe = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0); $probe.Start(); $port = $probe.LocalEndpoint.Port; $probe.Stop()
$l = [Net.HttpListener]::new(); $l.Prefixes.Add("http://127.0.0.1:$port/"); $l.Start()
[Console]::Out.WriteLine("PORT=$port"); [Console]::Out.Flush()
function B64([string]$s) { $s = $s.Replace('-', '+').Replace('_', '/'); switch ($s.Length % 4) { 2 { $s += '==' } 3 { $s += '=' } }; [Convert]::FromBase64String($s) }
function Reply($c, [int]$status, $obj) {
  $c.Response.StatusCode = $status
  if ($null -ne $obj) {
    $bytes = [Text.Encoding]::UTF8.GetBytes((ConvertTo-Json -InputObject $obj -Depth 6 -Compress))
    $c.Response.ContentType = 'application/json'
    $c.Response.OutputStream.Write($bytes, 0, $bytes.Length)
  }
  $c.Response.Close()
}
while ($true) {
  $c = $l.GetContext()
  $mode = (Get-Content -LiteralPath (Join-Path $Dir 'mode.txt') -Raw).Trim()
  $m = $c.Request.HttpMethod; $p = $c.Request.Url.AbsolutePath
  Add-Content -LiteralPath (Join-Path $Dir 'requests.log') -Value "$m $p"
  $auth = [string]$c.Request.Headers['Authorization']
  $jwtOk = $false
  if ($auth -like 'Bearer *.*.*') {
    $t = $auth.Substring(7).Split('.')
    $key = if ($mode -eq 'wrongkey') { $other } else { $rsa }
    $jwtOk = $key.VerifyData([Text.Encoding]::UTF8.GetBytes("$($t[0]).$($t[1])"), (B64 $t[2]), [Security.Cryptography.HashAlgorithmName]::SHA256, [Security.Cryptography.RSASignaturePadding]::Pkcs1)
  }
  if ($m -eq 'POST' -and $p -match '^/app-manifests/([A-Za-z0-9]+)/conversions$') {
    if ($Matches[1] -eq 'GOODCODE12345') {
      Reply $c 201 @{ id = 4242; slug = 'fake-app'; name = 'Fake App'; html_url = "http://127.0.0.1:$port/apps/fake-app"; pem = $pem; client_secret = 'CLIENT-SECRET-MARKER'; webhook_secret = 'WEBHOOK-SECRET-MARKER' }
    } else { Reply $c 404 @{ message = 'Not Found' } }
  }
  elseif ($m -eq 'GET' -and $p -eq '/app') { if ($jwtOk) { Reply $c 200 @{ id = 4242; slug = 'fake-app' } } else { Reply $c 401 @{ message = 'bad jwt' } } }
  elseif ($m -eq 'GET' -and $p -eq '/app/installations') {
    if (-not $jwtOk) { Reply $c 401 @{ message = 'bad jwt' } }
    elseif ($mode -eq 'noinstall') { Reply $c 200 @() }
    else { Reply $c 200 @(, @{ id = 77; target_type = 'Organization'; account = @{ login = 'ThinkersJournal' } }) }
  }
  elseif ($m -eq 'POST' -and $p -eq '/app/installations/77/access_tokens') {
    if (-not $jwtOk) { Reply $c 401 @{ message = 'bad jwt' } }
    else {
      $perms = if ($mode -eq 'noperm') { @{ metadata = 'read' } } else { @{ organization_self_hosted_runners = 'write'; metadata = 'read' } }
      Reply $c 201 @{ token = 'INSTALLATION-TOKEN-MARKER'; expires_at = '2026-10-22T20:00:00Z'; permissions = $perms }
    }
  }
  elseif ($m -eq 'DELETE' -and $p -eq '/installation/token') {
    if ($auth -eq 'Bearer INSTALLATION-TOKEN-MARKER') { Reply $c 204 $null } else { Reply $c 401 @{ message = 'bad token' } }
  }
  else { Reply $c 404 @{ message = 'Not Found' } }
}
'@
$fakePath = Join-Path $work 'fake-github.ps1'
Set-Content -LiteralPath $fakePath -Value $fake -Encoding utf8
$fakeDir = Join-Path $work 'fake'; New-Item -ItemType Directory -Path $fakeDir | Out-Null
Set-Content -LiteralPath (Join-Path $fakeDir 'mode.txt') -Value 'ok'
$fakeOut = Join-Path $work 'fake.out'
$fakeProc = Start-Process -FilePath $pwsh -ArgumentList (Q @('-NoProfile', '-File', $fakePath, $fakeDir)) -RedirectStandardOutput $fakeOut -PassThru -WindowStyle Hidden
$procs += $fakeProc

function Wait-Line([string]$path, [string]$pattern, [int]$seconds = 30) {
  $end = [DateTime]::UtcNow.AddSeconds($seconds)
  while ([DateTime]::UtcNow -lt $end) {
    if (Test-Path -LiteralPath $path) {
      $m = Select-String -LiteralPath $path -Pattern $pattern -ErrorAction SilentlyContinue | Select-Object -First 1
      if ($m) { return $m.Matches[0].Groups[1].Value }
    }
    Start-Sleep -Milliseconds 100
  }
  return $null
}

# Runs the setup script to completion (not -Apply unless asked): returns exit code and merged output.
function Run-Setup([string[]]$extra) {
  $out = Join-Path $work ("run-" + [Guid]::NewGuid().ToString('N').Substring(0, 6) + '.out')
  $p = Start-Process -FilePath $pwsh -ArgumentList (Q (@('-NoProfile', '-File', $setup) + $extra)) -RedirectStandardOutput $out -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden -Wait
  $text = (Get-Content -LiteralPath $out -Raw -ErrorAction SilentlyContinue) + (Get-Content -LiteralPath "$out.err" -Raw -ErrorAction SilentlyContinue)
  return @{ Code = $p.ExitCode; Text = [string]$text }
}

try {
  $port = Wait-Line $fakeOut '^PORT=(\d+)$'
  if (-not $port) { throw "the GitHub stand-in did not start: $(Get-Content -LiteralPath $fakeOut -Raw)" }
  $fakeUrl = "http://127.0.0.1:$port"
  $keyDir = Join-Path $work 'keys'
  $common = @('-GitHubWeb', $fakeUrl, '-GitHubApi', $fakeUrl, '-KeyDir', $keyDir)
  $secretMarkers = @('CLIENT-SECRET-MARKER', 'WEBHOOK-SECRET-MARKER', 'INSTALLATION-TOKEN-MARKER', 'BEGIN RSA PRIVATE KEY', 'BEGIN PRIVATE KEY')

  # --- dry run and argument refusals ----------------------------------------------------------------
  $r = Run-Setup $common
  Check 'dry run exits 0, prints the manifest and creates nothing' ($r.Code -eq 0 -and $r.Text -like '*DRY RUN*' -and $r.Text -like '*organization_self_hosted_runners*' -and -not (Test-Path -LiteralPath $keyDir)) $r.Text
  Check 'dry run asks for runner write, metadata read and no administration by default' ($r.Text -like '*"metadata": "read"*' -and $r.Text -notlike '*administration*') $r.Text
  $r = Run-Setup ($common + '-RepoAdminFallback')
  Check '-RepoAdminFallback adds administration: write' ($r.Text -like '*"administration": "write"*') $r.Text
  $r = Run-Setup @('-GitHubApi', 'https://evil.example', '-KeyDir', $keyDir)
  Check 'an API endpoint other than GitHub or loopback is refused' ($r.Code -ne 0 -and $r.Text -like '*neither https://api.github.com nor a loopback*') $r.Text
  $r = Run-Setup @('-GitHubWeb', 'http://evil.example:80', '-KeyDir', $keyDir)
  Check 'a web endpoint other than GitHub or loopback is refused' ($r.Code -ne 0) $r.Text
  $r = Run-Setup ($common + @('-Org', 'bad org!'))
  Check 'a bad organization name is refused' ($r.Code -ne 0 -and $r.Text -like '*not a GitHub organization name*') $r.Text
  $r = Run-Setup ($common + @('-Apply', '-Verify'))
  Check '-Apply with -Verify is refused' ($r.Code -ne 0) $r.Text

  # --- the flow --------------------------------------------------------------------------------------
  $applyOut = Join-Path $work 'apply.out'
  $apply = Start-Process -FilePath $pwsh -ArgumentList (Q (@('-NoProfile', '-File', $setup) + $common + @('-Apply', '-NoBrowser', '-TimeoutSeconds', '120'))) -RedirectStandardOutput $applyOut -RedirectStandardError "$applyOut.err" -PassThru -WindowStyle Hidden
  $procs += $apply
  $startUrl = Wait-Line $applyOut '^START_URL=(http://127\.0\.0\.1:\d+/start)$'
  Check 'apply prints the loopback START_URL' ([bool]$startUrl) (Get-Content -LiteralPath $applyOut -Raw)
  $base = ([Uri]$startUrl).GetLeftPart([UriPartial]::Authority)

  # The first request arrives LATE, after the script's 2 s accept poll has timed out more than once: it must still be
  # answered (an accept task abandoned at a poll timeout used to swallow it: CI saw "connection forcibly closed").
  Start-Sleep -Seconds 5
  $page = Invoke-WebRequest -Uri $startUrl -SkipHttpErrorCheck -TimeoutSec 20
  $html = [System.Net.WebUtility]::HtmlDecode($page.Content)
  Check 'the start page is a form that POSTs the manifest to the organization page with a state' ($page.StatusCode -eq 200 -and $html.Contains("method=`"post`" action=`"$fakeUrl/organizations/ThinkersJournal/settings/apps/new?state=") -and $html -like '*name="manifest"*') $html
  Check 'the manifest has the permissions and a loopback redirect, no webhook, no events' ($html.Contains('"organization_self_hosted_runners":"write"') -and $html.Contains('"metadata":"read"') -and $html.Contains("`"redirect_url`":`"$base/callback`"") -and $html.Contains('"public":false') -and $html.Contains('"default_events":[]') -and -not $html.Contains('hook_attributes')) $html
  $state = [regex]::Match($html, 'state=([0-9a-f]{64})').Groups[1].Value
  Check 'the state is 32 random bytes' ($state.Length -eq 64)

  $bad = Invoke-WebRequest -Uri "$base/callback?code=GOODCODE12345&state=$('0' * 64)" -SkipHttpErrorCheck
  Check 'a callback with the wrong state is refused and creates nothing' ($bad.StatusCode -eq 400 -and -not (Test-Path -LiteralPath (Join-Path $keyDir 'fake-app.key.dpapi'))) "$($bad.StatusCode)"
  $nostate = Invoke-WebRequest -Uri "$base/callback?code=GOODCODE12345" -SkipHttpErrorCheck
  Check 'a callback with no state is refused' ($nostate.StatusCode -eq 400)
  $post = Invoke-WebRequest -Uri "$base/callback?code=GOODCODE12345&state=$state" -Method Post -SkipHttpErrorCheck
  Check 'a non-GET request is refused' ($post.StatusCode -eq 400)
  $other = Invoke-WebRequest -Uri "$base/elsewhere" -SkipHttpErrorCheck
  Check 'other paths are 404' ($other.StatusCode -eq 404)
  Check 'the script is still waiting after all that' (-not $apply.HasExited)
  Check 'the stand-in saw no code exchange yet' (-not (Select-String -LiteralPath (Join-Path $fakeDir 'requests.log') -Pattern 'conversions' -Quiet -ErrorAction SilentlyContinue))

  $good = Invoke-WebRequest -Uri "$base/callback?code=GOODCODE12345&state=$state" -SkipHttpErrorCheck
  Check 'the right state and code is accepted' ($good.StatusCode -eq 200)
  $apply.WaitForExit(60000) | Out-Null
  $applyText = (Get-Content -LiteralPath $applyOut -Raw) + (Get-Content -LiteralPath "$applyOut.err" -Raw)
  Check 'apply exits 0 and tells the human click 2 with the install URL' ($apply.HasExited -and $apply.ExitCode -eq 0 -and $applyText -like "*$fakeUrl/apps/fake-app/installations/new*") $applyText
  $blobHead = (Get-Content -LiteralPath (Join-Path $keyDir 'fake-app.key.dpapi') -Raw).Substring(0, 40)
  Check 'apply output holds no part of the key blob either' (-not $applyText.Contains($blobHead))
  Check 'apply output holds no key, no client secret, no webhook secret' (-not ($secretMarkers | Where-Object { $applyText -like "*$_*" })) $applyText

  $keyFile = Join-Path $keyDir 'fake-app.key.dpapi'
  $metaFile = Join-Path $keyDir 'fake-app.json'
  Check 'the key file exists' (Test-Path -LiteralPath $keyFile)
  $blob = Get-Content -LiteralPath $keyFile -Raw
  Check 'the key file is a DPAPI blob, not the PEM' ($blob.Length -gt 100 -and $blob -notlike '*BEGIN*' -and $blob -match '^[0-9a-f]+$')
  $meta = Get-Content -LiteralPath $metaFile -Raw
  Check 'the metadata file has the app id and slug and no secret' ($meta -like '*"app_id": 4242*' -and $meta -like '*fake-app*' -and -not ($secretMarkers | Where-Object { $meta -like "*$_*" }))
  $everyDisk = Get-ChildItem -LiteralPath $keyDir -Recurse -File | ForEach-Object { Get-Content -LiteralPath $_.FullName -Raw }
  Check 'no file under the key directory contains a PEM or a secret marker' (-not ($secretMarkers | Where-Object { ($everyDisk -join "`n") -like "*$_*" }))
  $acl = (Get-Acl -LiteralPath $keyFile).Access | ForEach-Object { $_.IdentityReference.Value }
  Check 'the key directory is owner-only (no Everyone, Users, Authenticated Users)' (-not ($acl | Where-Object { $_ -match 'Everyone|BUILTIN\\Users|Authenticated Users' })) ($acl -join ', ')

  # --- a second -Apply must not orphan the key ----------------------------------------------------------
  $r = Run-Setup ($common + @('-Apply', '-NoBrowser', '-TimeoutSeconds', '5'))
  Check 'a second -Apply is refused while a key exists' ($r.Code -ne 0 -and $r.Text -like '*already exists*' -and $r.Text -notlike '*START_URL*') $r.Text

  # --- the timeout leaves nothing behind -----------------------------------------------------------------
  $emptyDir = Join-Path $work 'keys-timeout'
  $r = Run-Setup @('-GitHubWeb', $fakeUrl, '-GitHubApi', $fakeUrl, '-KeyDir', $emptyDir, '-Apply', '-NoBrowser', '-TimeoutSeconds', '5')
  Check 'with no redirect the script times out non-zero and stores no key' ($r.Code -ne 0 -and $r.Text -like '*timed out*' -and @(Get-ChildItem -LiteralPath $emptyDir -Filter '*.key.dpapi' -ErrorAction SilentlyContinue).Count -eq 0) $r.Text

  # --- a refused code exchange stores nothing -------------------------------------------------------------
  $badDir = Join-Path $work 'keys-badcode'
  $badOut = Join-Path $work 'badcode.out'
  $bad = Start-Process -FilePath $pwsh -ArgumentList (Q @('-NoProfile', '-File', $setup, '-GitHubWeb', $fakeUrl, '-GitHubApi', $fakeUrl, '-KeyDir', $badDir, '-Apply', '-NoBrowser', '-TimeoutSeconds', '60')) -RedirectStandardOutput $badOut -RedirectStandardError "$badOut.err" -PassThru -WindowStyle Hidden
  $procs += $bad
  $u = Wait-Line $badOut '^START_URL=(http://127\.0\.0\.1:\d+/start)$'
  $b2 = ([Uri]$u).GetLeftPart([UriPartial]::Authority)
  $st = [regex]::Match([System.Net.WebUtility]::HtmlDecode((Invoke-WebRequest -Uri $u).Content), 'state=([0-9a-f]{64})').Groups[1].Value
  try { Invoke-WebRequest -Uri "$b2/callback?code=WRONGCODE9999&state=$st" -SkipHttpErrorCheck | Out-Null } catch { }
  $bad.WaitForExit(60000) | Out-Null
  Check 'a code GitHub rejects ends non-zero and stores nothing' ($bad.ExitCode -ne 0 -and @(Get-ChildItem -LiteralPath $badDir -Filter '*.key.dpapi' -ErrorAction SilentlyContinue).Count -eq 0) (Get-Content -LiteralPath "$badOut.err" -Raw)

  # --- -Verify ----------------------------------------------------------------------------------------------
  $r = Run-Setup ($common + '-Verify')
  Check '-Verify passes against a correct install, signs a JWT GitHub accepts, and prints no token' ($r.Code -eq 0 -and $r.Text -like '*verify ok*' -and $r.Text -like '*organization_self_hosted_runners=write*' -and -not ($secretMarkers | Where-Object { $r.Text -like "*$_*" })) $r.Text
  $log = Get-Content -LiteralPath (Join-Path $fakeDir 'requests.log') -Raw
  Check '-Verify revoked the installation token' ($log -like '*DELETE /installation/token*') $log
  Check '-Verify recorded the installation id' ((Get-Content -LiteralPath $metaFile -Raw) -like '*"installation_id": 77*')

  Set-Content -LiteralPath (Join-Path $fakeDir 'mode.txt') -Value 'noperm'
  $r = Run-Setup ($common + '-Verify')
  Check '-Verify fails when the runner permission is missing' ($r.Code -ne 0 -and $r.Text -like '*organization_self_hosted_runners should be write*') $r.Text

  Set-Content -LiteralPath (Join-Path $fakeDir 'mode.txt') -Value 'wrongkey'
  $r = Run-Setup ($common + '-Verify')
  Check '-Verify fails when GitHub does not accept the signature (known-bad control)' ($r.Code -ne 0) $r.Text

  Set-Content -LiteralPath (Join-Path $fakeDir 'mode.txt') -Value 'noinstall'
  $r = Run-Setup ($common + '-Verify')
  Check '-Verify fails, and says click 2, when the app is not installed' ($r.Code -ne 0 -and $r.Text -like '*not installed*') $r.Text

  Set-Content -LiteralPath (Join-Path $fakeDir 'mode.txt') -Value 'ok'
  $r = Run-Setup @('-GitHubApi', $fakeUrl, '-KeyDir', (Join-Path $work 'nokeys'), '-Verify')
  Check '-Verify with no key says to run -Apply first' ($r.Code -ne 0 -and $r.Text -like '*no key*') $r.Text
}
finally {
  foreach ($p in $procs) { if ($p -and -not $p.HasExited) { try { $p.Kill($true) } catch { } } }
  Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}

if ($failures.Count -gt 0) { Write-Host "FAILED: $($failures.Count): $($failures -join '; ')"; exit 1 }
Write-Host 'all passed'
