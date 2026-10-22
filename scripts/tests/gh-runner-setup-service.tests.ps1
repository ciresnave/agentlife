<#
.SYNOPSIS
  Tests three functions of scripts\gh-runner-setup.ps1 that a live run (2026-10-22) showed wrong:
  Find-RunnerService (the runner truncates long service names, so the service is found by the runner directory its executable
  lives in), New-RandomPassword (the password goes through config.cmd, so it must survive a real cmd.exe round trip) and
  Select-AccountServices (a rotated password must reach every service that logs on as the account). Fake service objects,
  a temp directory, no service read or changed. Exit 0 = all passed.
  Run by tests\runner_setup_service.rs on the Windows CI leg, or by hand: powershell -NoProfile -File <this file>
#>
$ErrorActionPreference = 'Stop'
$setup = Join-Path (Split-Path -Parent $PSScriptRoot) 'gh-runner-setup.ps1'
if (-not (Test-Path -LiteralPath $setup)) { throw "setup script not found: $setup" }
$ast = [Management.Automation.Language.Parser]::ParseFile($setup, [ref]$null, [ref]$null)
foreach ($name in 'Find-RunnerService', 'Select-AccountServices', 'New-RandomPassword') {
  $fn = $ast.FindAll({ param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq $name }, $true)
  if ($fn.Count -ne 1) { throw "expected exactly one $name in $setup, found $($fn.Count)" }
  . ([scriptblock]::Create($fn[0].Extent.Text))
}

$failures = New-Object System.Collections.Generic.List[string]
function Check([string]$name, [bool]$ok, [string]$detail = '') {
  if ($ok) { Write-Host "ok   $name" } else { Write-Host "FAIL $name $detail"; $failures.Add($name) }
}
function Fake([string]$name, [string]$path) { [pscustomobject]@{ Name = $name; PathName = $path } }

$root = 'C:\actions-runners'
$repo = 'ThinkersJournal/ThinkersFoundation-site'
$slug = $repo -replace '/', '-'
$dir = "$root\$slug"
# What the old code computed, and what the runner really registered (truncated; from the PM's live run).
$computed = 'actions.runner.' + $slug + '.TUF-LAPTOP-' + $slug
$real = 'actions.runner.ThinkersJournal-ThinkersFoundation-site.TUF-LAPTOP-ThinkersJ-9595'
Check 'control: the computed name is over 80 characters' ($computed.Length -gt 80) "len $($computed.Length)"
Check 'control: the computed name is not the real one' ($computed -ne $real)

$mine = Fake $real "`"$dir\bin\RunnerService.exe`""
$other = Fake 'actions.runner.ciresnave-lightbulb.TUF-LAPTOP-ciresnave-lightbulb' "`"$root\ciresnave-lightbulb\bin\RunnerService.exe`""
$unrelated = Fake 'Spooler' 'C:\Windows\System32\spoolsv.exe'

$hit = Find-RunnerService $dir @($other, $unrelated, $mine)
Check 'finds the service by directory though its name is truncated' ($hit -and $hit.Name -eq $real) "got $($hit.Name)"
Check 'a service under another runner directory is not picked' ((Find-RunnerService "$root\ciresnave-lightbulb" @($other, $mine)).Name -eq $other.Name)
Check 'none under the directory gives null' ($null -eq (Find-RunnerService "$root\absent" @($other, $mine, $unrelated)))
Check 'no services at all gives null' ($null -eq (Find-RunnerService $dir @()))

# A sibling whose name only STARTS with this directory's name (ThinkersFoundation-site vs ThinkersFoundation-site-2).
$sibling = Fake 'actions.runner.sibling' "`"$dir-2\bin\RunnerService.exe`""
$hit = Find-RunnerService $dir @($sibling)
Check 'a sibling directory sharing the prefix is not matched' ($null -eq $hit) "got $($hit.Name)"

Check 'matching ignores case' ((Find-RunnerService $dir.ToUpper() @($mine)).Name -eq $real)
$noQuotes = Fake $real "$dir\bin\RunnerService.exe"
Check 'an unquoted path is matched' ((Find-RunnerService $dir @($noQuotes)).Name -eq $real)
$withArgs = Fake $real "`"$dir\bin\RunnerService.exe`" --some-arg"
Check 'a path followed by arguments is matched' ((Find-RunnerService $dir @($withArgs)).Name -eq $real)
$notRunner = Fake 'x' "`"$dir\other.exe`""
Check 'an executable other than RunnerService.exe is not matched' ($null -eq (Find-RunnerService $dir @($notRunner)))

$threw = $false
try { Find-RunnerService $dir @($mine, (Fake 'second' "`"$dir\bin\RunnerService.exe`"")) | Out-Null } catch { $threw = $true }
Check 'two services under one directory are refused, not guessed' $threw


# --- New-RandomPassword: the alphabet and a real cmd.exe round trip -------------------------------------------------------
$pws = 1..200 | ForEach-Object { New-RandomPassword }
Check 'passwords are 32 characters of letters, digits and - _ @ only' (@($pws | Where-Object { $_ -notmatch '^[A-Za-z0-9@_-]{32}$' }).Count -eq 0)
Check 'no generated password holds a cmd.exe metacharacter (% ^ ! ? + =)' (@($pws | Where-Object { $_ -match '[%^!?+=]' }).Count -eq 0)
Check 'passwords differ' (($pws | Sort-Object -Unique).Count -eq $pws.Count)

$tmp = Join-Path ([IO.Path]::GetTempPath()) ('ghrunner-pw-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
  # Stands in for config.cmd: a .cmd file that records its arguments as cmd.exe sees them.
  Set-Content -LiteralPath "$tmp\echo-args.cmd" -Value "@echo off`r`n>`"%~dp0out.txt`" echo %*" -Encoding Ascii
  function RoundTrip([string]$pw) {
    Remove-Item -LiteralPath "$tmp\out.txt" -ErrorAction SilentlyContinue
    & "$tmp\echo-args.cmd" --unattended --windowslogonpassword $pw | Out-Null
    (Get-Content -LiteralPath "$tmp\out.txt" -Raw).Trim()
  }
  $bad = 'ab%cd^ef!gh?ij'
  Check 'control: the old alphabet`s characters ARE mangled by cmd.exe' ((RoundTrip $bad) -ne "--unattended --windowslogonpassword $bad") (RoundTrip $bad)
  $mangled = @($pws | Select-Object -First 40 | Where-Object { (RoundTrip $_) -ne "--unattended --windowslogonpassword $_" })
  Check 'generated passwords reach a .cmd file unchanged (40 round trips)' ($mangled.Count -eq 0) "$($mangled.Count) mangled"
} finally { Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue }

# --- Select-AccountServices -------------------------------------------------------------------------------------------------
function Acct([string]$name, [string]$start) { [pscustomobject]@{ Name = $name; StartName = $start } }
$svcs = @((Acct 'a' '.\gh-runner'), (Acct 'b' 'TUF-LAPTOP\gh-runner'), (Acct 'c' '.\GH-RUNNER'), (Acct 'd' '.\gh-runner-private'),
          (Acct 'e' 'LocalSystem'), (Acct 'f' 'OTHERPC\gh-runner'), (Acct 'g' $null))
$got = (Select-AccountServices $svcs 'gh-runner' 'TUF-LAPTOP' | ForEach-Object Name) -join ','
Check 'every service logging on as the account is selected, in either spelling, ignoring case' ($got -eq 'a,b,c') $got
Check 'a similarly named account, a system account and another machine are not selected' (-not ($got -match 'd|e|f|g'))
Check 'no services gives an empty list' ((Select-AccountServices @() 'gh-runner' 'TUF-LAPTOP').Count -eq 0)

if ($failures.Count -gt 0) { Write-Host "`n$($failures.Count) FAILED: $($failures -join ', ')"; exit 1 }
Write-Host "`nall passed"
exit 0
