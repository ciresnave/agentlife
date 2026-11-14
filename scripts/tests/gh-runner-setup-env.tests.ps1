# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Tests Get-RunnerEnvLines and Test-ProbeOutput of scripts\gh-runner-setup.ps1: the runner's .env must put Git's bash ahead of
  System32's WSL launcher and give the job a Rust toolchain (mlmf's first job, 2026-11-12, died on both). Pure functions;
  the one process-level check runs `bash` and `cargo` under the generated environment as the CURRENT user (it measures the PATH
  logic, not another account's permissions) and is skipped, with a printed reason, when Git for Windows or C:\rust is absent.
  Exit 0 = all passed. Run by tests\runner_setup_env.rs on the Windows CI leg, or by hand.
#>
$ErrorActionPreference = 'Stop'
$setup = Join-Path (Split-Path -Parent $PSScriptRoot) 'gh-runner-setup.ps1'
if (-not (Test-Path -LiteralPath $setup)) { throw "setup script not found: $setup" }
$ast = [Management.Automation.Language.Parser]::ParseFile($setup, [ref]$null, [ref]$null)
foreach ($name in 'Get-RunnerEnvLines', 'Test-ProbeOutput') {
  $fn = $ast.FindAll({ param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq $name }, $true)
  if ($fn.Count -ne 1) { throw "expected exactly one $name in $setup, found $($fn.Count)" }
  . ([scriptblock]::Create($fn[0].Extent.Text))
}
$failures = New-Object System.Collections.Generic.List[string]
function Check([string]$name, [bool]$ok, [string]$detail = '') {
  if ($ok) { Write-Host "ok   $name" } else { Write-Host "FAIL $name $detail"; $failures.Add($name) }
}
function Throws([scriptblock]$sb) { try { & $sb | Out-Null; $null } catch { $_.Exception.Message } }

$git = 'C:\Program Files\Git'; $rust = 'C:\rust'; $cargo = 'C:\gh-runner-ghr-x\cargo-home'
$machine = 'C:\Python;C:\WINDOWS\system32;C:\WINDOWS;C:\Program Files\Git\bin;C:\Python'
$lines = Get-RunnerEnvLines $git $rust $cargo $machine
$map = @{}; foreach ($l in $lines) { $kv = $l -split '=', 2; $map[$kv[0]] = $kv[1] }
Check 'keeps the execution-policy line first' ($lines[0] -eq 'PSExecutionPolicyPreference=Bypass')
$parts = $map['PATH'] -split ';'
Check 'Git bin and usr\bin are the first two PATH entries' ($parts[0] -eq "$git\bin" -and $parts[1] -eq "$git\usr\bin") ($parts[0..1] -join ' | ')
Check 'the Rust shims come next' ($parts[2] -eq "$rust\.cargo\bin") $parts[2]
Check 'Git precedes System32 (the WSL bash.exe lives there)' ([array]::IndexOf($parts, "$git\bin") -lt [array]::IndexOf($parts, 'C:\WINDOWS\system32'))
Check 'a machine PATH entry that repeats is kept once (ignoring case and a trailing backslash)' (@($parts | Where-Object { $_ -ieq 'C:\Program Files\Git\bin' }).Count -eq 1 -and @($parts | Where-Object { $_ -eq 'C:\Python' }).Count -eq 1)
Check 'RUSTUP_HOME is the shared toolchain dir and CARGO_HOME the account''s own' ($map['RUSTUP_HOME'] -eq "$rust\.rustup" -and $map['CARGO_HOME'] -eq $cargo)
Check 'CARGO_HOME is not the shared one' ($map['CARGO_HOME'] -notlike "$rust*")
Check 'CARGO_BUILD_BUILD_DIR is <runner dir>\cb, inside the account''s own tree and not C:\cb' ($map['CARGO_BUILD_BUILD_DIR'] -eq 'C:\gh-runner-ghr-x\cb')  $map['CARGO_BUILD_BUILD_DIR']
Check 'RUSTC_WRAPPER is set EMPTY so a machine-level wrapper cannot leak in' (($lines -contains 'RUSTC_WRAPPER=') -and $map.ContainsKey('RUSTC_WRAPPER') -and $map['RUSTC_WRAPPER'] -eq '')
$rd = 'C:\gh-runner-ghr-x\ciresnave-y'
$cl = Get-RunnerEnvLines $git $rust "$rd\cargo-home" $machine $rd
$cm = @{}; foreach ($l in $cl) { $kv = $l -split '=', 2; $cm[$kv[0]] = $kv[1] }
Check 'with a runner dir, the job-completed hook is set to the copy inside it' ($cm['ACTIONS_RUNNER_HOOK_JOB_COMPLETED'] -eq "$rd\job-completed-cleanup.ps1") $cm['ACTIONS_RUNNER_HOOK_JOB_COMPLETED']
Check 'CARGO_HOME, CARGO_TARGET_DIR and CARGO_BUILD_BUILD_DIR all sit inside the runner dir the hook cleans' (($cm['CARGO_HOME'], $cm['CARGO_TARGET_DIR'], $cm['CARGO_BUILD_BUILD_DIR'] | Where-Object { $_ -like "$rd\*" }).Count -eq 3) "$($cm['CARGO_HOME']) | $($cm['CARGO_TARGET_DIR']) | $($cm['CARGO_BUILD_BUILD_DIR'])"
Check 'every cargo dir the .env names is one the hook deletes (cargo-home, cargo-target, cb)' (((($cm['CARGO_HOME'], $cm['CARGO_TARGET_DIR'], $cm['CARGO_BUILD_BUILD_DIR']) | ForEach-Object { Split-Path -Leaf $_ }) -join ',') -eq 'cargo-home,cargo-target,cb')
$hookText = Get-Content -LiteralPath (Join-Path (Split-Path -Parent $PSScriptRoot) 'runner-job-cleanup-hook.ps1') -Raw
Check 'the hook script names exactly those three dirs' ($hookText -match "'cargo-target', 'cargo-home', 'cb'")
Check 'without a runner dir (the old four-argument call) no hook or target line is added' (-not $map.ContainsKey('ACTIONS_RUNNER_HOOK_JOB_COMPLETED') -and -not $map.ContainsKey('CARGO_TARGET_DIR'))
Check 'a relative runner dir is refused' ((Throws { Get-RunnerEnvLines $git $rust $cargo $machine 'runner' }) -match 'must be an absolute')
$noRust = Get-RunnerEnvLines $git '' '' $machine
Check 'an empty RustRoot leaves Rust out but keeps Git first' (-not (($noRust -join "`n") -match 'CARGO_HOME|CARGO_BUILD_BUILD_DIR|RUSTC_WRAPPER|RUSTUP_HOME|\.cargo') -and ($noRust | Where-Object { $_ -like 'PATH=C:\Program Files\Git\bin;*' }))
$none = Get-RunnerEnvLines '' '' '' $machine
Check 'neither Git nor Rust: only the policy line, PATH untouched' ($none.Count -eq 1)
Check 'a ; in a path is refused' ((Throws { Get-RunnerEnvLines 'C:\a;b' $rust $cargo $machine }) -match 'must be an absolute')
Check 'a % in a path is refused' ((Throws { Get-RunnerEnvLines $git 'C:\%x%' $cargo $machine }) -match 'must be an absolute')
Check 'a relative path is refused' ((Throws { Get-RunnerEnvLines 'Git' $rust $cargo $machine }) -match 'must be an absolute')

# --- Test-ProbeOutput -------------------------------------------------------------------------------------------------------
$good = "bash-path=C:\Program Files\Git\bin\bash.exe`r`nbash-run=ok`r`ncargo=cargo 1.99.0 (abc 2026-01-01)`r`nrustc=rustc 1.99.0 (def 2026-01-01)`r`n"
Check 'a good probe passes' ((Test-ProbeOutput $good $git $true).Count -eq 0) ((Test-ProbeOutput $good $git $true) -join '; ')
$wsl = $good -replace 'C:\\Program Files\\Git\\bin\\bash.exe', 'C:\Windows\System32\bash.exe'
$r = @(Test-ProbeOutput $wsl $git $true)
Check 'bash resolving to the WSL launcher fails, naming it' ($r.Count -eq 1 -and $r[0] -match 'System32.*not Git') ($r -join '; ')
$r = @(Test-ProbeOutput ($good -replace 'bash-run=ok', 'bash-run=Windows Subsystem for Linux has no installed distribution') $git $true)
Check 'a bash that runs but does not print ok fails' ($r.Count -eq 1 -and $r[0] -match 'not ok') ($r -join '; ')
$r = @(Test-ProbeOutput ($good -replace 'cargo=cargo 1.99.0 \(abc 2026-01-01\)', 'cargo=cargo : not recognized') $git $true)
Check 'a cargo that does not run fails' ($r.Count -eq 1 -and $r[0] -match 'cargo --version') ($r -join '; ')
Check 'empty output fails every check' ((Test-ProbeOutput '' $git $true).Count -ge 3)
Check 'cargo and rustc are not demanded when Rust is left out' ((Test-ProbeOutput "bash-path=C:\Program Files\Git\bin\bash.exe`r`nbash-run=ok" $git $false).Count -eq 0)

# --- the PATH logic under a real process (as the current user) -------------------------------------------------------------
$gitBash = "$git\bin\bash.exe"
if (-not (Test-Path -LiteralPath $gitBash) -or -not (Test-Path -LiteralPath "$rust\.cargo\bin\cargo.exe")) {
  Write-Host "skip process check: need $gitBash and $rust\.cargo\bin\cargo.exe on this machine"
} else {
  $mp = [Environment]::GetEnvironmentVariable('Path', 'Machine')
  $tmpCargo = Join-Path ([IO.Path]::GetTempPath()) ('ghenv-cargo-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
  New-Item -ItemType Directory -Path $tmpCargo | Out-Null
  try {
    $el = Get-RunnerEnvLines $git $rust $tmpCargo $mp
    $sb = @(); foreach ($l in $el) { $kv = $l -split '=', 2; $sb += "`$env:$($kv[0]) = '$($kv[1] -replace "'", "''")'" }
    $sb += '$w = (where.exe bash 2>$null | Select-Object -First 1); "bash-path=$w"'
    $sb += '"bash-run=" + (& bash -c "echo ok" 2>&1 | Select-Object -First 1)'
    $sb += '"cargo=" + (& cargo --version 2>&1 | Select-Object -First 1)'
    $sb += '"rustc=" + (& rustc --version 2>&1 | Select-Object -First 1)'
    $ps1 = Join-Path $tmpCargo 'probe.ps1'
    Set-Content -LiteralPath $ps1 -Value $sb -Encoding Ascii
    $out = & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $ps1 2>&1 | Out-String
    $fail = @(Test-ProbeOutput $out $git $true)
    Check 'under a real process with the generated .env, bash is Git''s and bash, cargo and rustc run' ($fail.Count -eq 0) ("$($fail -join '; ') :: $out")
    # control: the same probe WITHOUT the Git-first PATH reproduces the failure only where System32 really holds a WSL bash.exe
    if (Test-Path -LiteralPath "$env:SystemRoot\System32\bash.exe") {
      $plain = $out -replace 'bash-path=.*', "bash-path=$env:SystemRoot\System32\bash.exe"
      Check 'control: a probe that saw System32\bash.exe first would be refused' ((Test-ProbeOutput $plain $git $true).Count -ge 1)
    }
  } finally { Remove-Item -LiteralPath $tmpCargo -Recurse -Force -ErrorAction SilentlyContinue }
}

if ($failures.Count -gt 0) { Write-Host "`n$($failures.Count) FAILED: $($failures -join ', ')"; exit 1 }
Write-Host "`nall passed"
exit 0
