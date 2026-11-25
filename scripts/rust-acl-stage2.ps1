# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Board 429 stage 2 (docs/RUST-ACL-STAGE2.md): per-runner private CARGO_HOME (-Step A1 / -Undo A1) and the
  C:\rust tightening (-Step B / -Undo B). DRY RUN BY DEFAULT: without -Apply it only READS (the service registry,
  the runner .env if readable, the current ACL) and prints the exact commands it would run. A lane never passes
  -Apply; -Apply needs an ELEVATED PowerShell and is run by the PM or CireSnave after review.
.PARAMETER Step   A1 (give one runner its own CARGO_HOME) or B (tighten C:\rust).
.PARAMETER Runner For A1: the account of the row: ghr-fuel or ghr-baracuda (the rows still needed on 2026-11-24).
.PARAMETER Undo   Print/run the undo of the step instead (A1: restore the .env backup; B: icacls /restore).
.PARAMETER Stamp  Suffix of the backup files, default 20261112 (the plan's).
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory)][ValidateSet('A1', 'B')][string]$Step,
  [string]$Runner,
  [switch]$Undo,
  [switch]$Apply,
  [string]$Stamp = '20261112'
)
$ErrorActionPreference = 'Stop'

function Get-RunnerService([string]$dir) {
  # The service registry, read from HKLM (Get-CimInstance Win32_Service can fail on this box).
  $found = @()
  foreach ($k in Get-ChildItem 'HKLM:\SYSTEM\CurrentControlSet\Services') {
    $p = Get-ItemProperty $k.PSPath -ErrorAction SilentlyContinue
    if ($p.ImagePath -and $p.ImagePath -like '*RunnerService.exe*' -and $p.ImagePath -like "*$dir\*") {
      $found += [pscustomobject]@{ Name = $k.PSChildName; Account = $p.ObjectName; Image = $p.ImagePath }
    }
  }
  return $found
}
function Show([string]$s) { Write-Host ($(if ($Apply) { 'RUN   ' } else { 'WOULD ' }) + $s) }
function Do-Step([string]$text, [scriptblock]$sb) { Show $text; if ($Apply) { & $sb } }

if ($Apply) {
  $id = [Security.Principal.WindowsIdentity]::GetCurrent()
  if (-not ([Security.Principal.WindowsPrincipal]$id).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw '-Apply needs an elevated PowerShell (a lane never applies this)'
  }
} else { Write-Host 'DRY RUN: nothing is changed (add -Apply, elevated, to run it)' }

if ($Step -eq 'A1') {
  $rows = @{ 'ghr-fuel' = 'C:\ghr-fuel\actions-runner'; 'ghr-baracuda' = 'C:\ghr-baracuda\actions-runner' }
  if (-not $Runner -or -not $rows.ContainsKey($Runner)) { throw "-Runner must be one of: $($rows.Keys -join ', ')" }
  $d = $rows[$Runner]; $acct = $Runner; $cargo = "C:\$Runner\cargo-home"
  $svc = @(Get-RunnerService $d)
  if ($svc.Count -ne 1) { throw "expected one runner service under $d, found $($svc.Count)" }
  $name = $svc[0].Name
  if ($svc[0].Account -notlike "*\$acct") { throw "service $name logs on as $($svc[0].Account), expected $acct" }
  Write-Host "runner: $name  dir: $d  account: $($svc[0].Account)  status: $((Get-Service $name).Status)"
  $envFile = "$d\.env"; $bak = "$d\.env.bak-$Stamp"
  try { $cur = Get-Content -LiteralPath $envFile -ErrorAction Stop; Write-Host ".env names: $((($cur | ForEach-Object { ($_ -split '=', 2)[0] }) -join ', '))" }
  catch { Write-Host ".env not readable from this account ($($_.Exception.Message.Split([char]10)[0])): it is read at -Apply" }
  if ($Undo) {
    Do-Step "Stop-Service $name -Force" { Stop-Service $name -Force }
    Do-Step "Copy-Item $bak $envFile -Force   # undo: the backup from -Apply" { Copy-Item $bak $envFile -Force }
    Do-Step "Start-Service $name" { Start-Service $name }
    return
  }
  # Refuse a second run: it would double the CARGO_HOME line and overwrite the first (clean) backup.
  if ($Apply -and (Test-Path -LiteralPath $bak)) { throw "$bak exists: A1 already ran for $Runner; use -Undo first" }
  Do-Step "Stop-Service $name -Force            # a stop kills a running job: do it between jobs" { Stop-Service $name -Force }
  Do-Step "Copy-Item $envFile $bak" { Copy-Item $envFile $bak }
  Do-Step "Add-Content $envFile 'CARGO_HOME=$cargo'" { Add-Content -LiteralPath $envFile -Value "CARGO_HOME=$cargo" -Encoding Ascii }
  Do-Step "New-Item -ItemType Directory -Force $cargo" { New-Item -ItemType Directory -Force -Path $cargo | Out-Null }
  Do-Step "icacls $cargo /inheritance:r /grant:r SYSTEM:F Administrators:F ${env:COMPUTERNAME}\${acct}:M" {
    icacls $cargo /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' "${env:COMPUTERNAME}\${acct}:(OI)(CI)M" }
  Do-Step "Start-Service $name" { Start-Service $name }
  Write-Host "READ BACK: (Get-Service '$name').Status is Running; '$envFile' ends with CARGO_HOME=$cargo and still has PSExecutionPolicyPreference=Bypass; the first cargo job on this runner creates $cargo\registry."
  return
}

# Step B: C:\rust
$backup = "C:\rust-acl-$Stamp.txt"
if ($Undo) {
  Do-Step "icacls C:\ /restore $backup    # undoes inheritance removal too" { icacls C:\ /restore $backup }
  Write-Host 'READ BACK: icacls C:\rust shows Authenticated Users again.'
  return
}
Write-Host 'current ACL of C:\rust:'; icacls C:\rust
Do-Step "icacls C:\rust /save $backup /t /c   # about 600,000 files, minutes; KEEP the file" { icacls C:\rust /save $backup /t /c }
Do-Step "icacls C:\rust /inheritance:r /grant:r SYSTEM:F Administrators:F TUF-LAPTOP\cires:M Users:RX" {
  if (-not (Test-Path -LiteralPath $backup)) { throw "no backup at ${backup} - not tightening" }
  icacls C:\rust /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' 'TUF-LAPTOP\cires:(OI)(CI)M' '*S-1-5-32-545:(OI)(CI)RX' }
Write-Host 'READ BACK: icacls C:\rust and C:\rust\.cargo\registry list no Authenticated Users. Then the deny control as a runner: a job step `cmd /c "echo x > C:\rust\.cargo\probe.txt"` must say Access is denied, and one cargo job per Rust runner must still build.'
