# C:\rust: tighten the shared Rust install (stage 2). A plan: nothing here has been run

Written 2026-11-12 for PM board 429 (CireSnave decides; the PM schedules; **a lane runs none of it**, every step needs an elevated shell). Facts are read, not inferred, unless marked **[inferred]**.

## Why, and the one thing that makes it two stages

`C:\rust` (the machine Rust: `.cargo` with the rustup shims and `bin`, `.rustup` with the toolchains) inherits `Authenticated Users:(M)` from `C:\`. Any job on any runner account can replace the toolchain every lane uses, and `C:\rust\.cargo\credentials.toml` (57 bytes, contents not read) is readable by all local accounts. Stage 1 (the credentials file alone) is on the board and breaks nothing. Stage 2 removes write access to the rest, for everyone but `cires`.

**What it can break:** the MACHINE environment sets `CARGO_HOME=C:\rust\.cargo` and `RUSTUP_HOME=C:\rust\.rustup`, and the existing runners' `.env` sets neither, so every runner account inherits them and WRITES its registry cache and lock files under `C:\rust\.cargo` today (newest registry write 2026-11-12). Tighten first and their `cargo` steps fail with permission denied. So each Rust runner gets its own `CARGO_HOME` first (A below), then the ACL (B).

Writers today, by folder (newest file per folder, scan 2026-11-12): `.cargo\registry\{cache,index,src}` and `.cargo\.global-cache`, `.package-cache*` (every cargo run), `.cargo\advisory-dbs` (cargo-deny), `.cargo\git`, `.cargo\bin` (cargo install), `.rustup\toolchains`, `update-hashes`, `settings.toml` (rustup update; 2026-11-12 11:31-11:38). Lanes run as `cires` **[inferred: process owners could not be read]**, who keeps Modify.

## A. Per Rust runner: a private CARGO_HOME

The six runner services (read from the service registry 2026-11-12, all Running): 

| account | runner directory | repo workflows that run cargo on it (read) | action |
|---|---|---|---|
| `gh-runner` | `C:\gh-runner\ciresnave-PersistAnt` | PersistAnt `machine-durability.yml`, `synapse-power-cut.yml` | A1 |
| `gh-runner-cpufish` | `C:\gh-runner-cpufish\ThinkersJournal-cpufish` | cpufish `ci.yml`, `aocl.yml` | A1 |
| `ghr-baracuda` | `C:\ghr-baracuda\actions-runner` | baracuda `ci.yml`, `device-tests*.yml` | A1 |
| `ghr-fuel` | `C:\ghr-fuel\actions-runner` | fuel `rust-ci.yml`, `cuda-metal-compile-check.yml`, `checkpoint-tests.yml`, `device-tests.yml` | A1 |
| `ghr-licence-tools` | `C:\ghr-licence-tools\ThinkersJournal-licence-tools` | `ci.yml` has no cargo line | none |
| `ghr-mlmf-corpus` | `C:\gh-runner-ghr-mlmf-corpus\ciresnave-mlmf` | mlmf `corpus-tests.yml` | A2: `-Rearm` (agentlife#272/#273) writes `CARGO_HOME` itself |

Not runners but accounts that will inherit the machine variables: `antspawn-run`, `antspawn-v0-broker`, `ghr-spawn1`, `ghr-spawn2` (AntSpawn's launcher). They run no job today; the antspawn lane sets `CARGO_HOME` in the launcher environment before its first Rust job. `james`, `kims2`, `waemi`, `Administrator`, `Guest` are disabled.

**A1, a runner that is registered and online** (no token needed; do it between jobs, a stop kills a running job). Elevated pwsh, once per row, with that row's values:

```powershell
$d     = 'C:\ghr-fuel\actions-runner'          # the row's runner directory
$acct  = 'ghr-fuel'                            # the row's account
$cargo = 'C:\ghr-fuel\cargo-home'              # a new directory beside the runner, never under C:\rust
$svc = @(Get-CimInstance Win32_Service -Filter "PathName LIKE '%RunnerService.exe%'" | Where-Object { $_.PathName -like "*$d\*" })
if ($svc.Count -ne 1) { throw "expected one runner service under $d, found $($svc.Count)" }
Stop-Service $svc[0].Name -Force
Copy-Item "$d\.env" "$d\.env.bak-20261112"
Add-Content -LiteralPath "$d\.env" -Value "CARGO_HOME=$cargo" -Encoding Ascii
New-Item -ItemType Directory -Force -Path $cargo | Out-Null
icacls $cargo /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' "${env:COMPUTERNAME}\${acct}:(OI)(CI)M"
Start-Service $svc[0].Name
```

Read back: `(Get-Service $svc[0].Name).Status` is `Running`; `Get-Content "$d\.env"` ends with the `CARGO_HOME=` line and still has `PSExecutionPolicyPreference=Bypass`; `icacls $cargo` lists SYSTEM, Administrators and the account only. Undo: stop the service, `Copy-Item "$d\.env.bak-20261112" "$d\.env" -Force`, start it. Then the real read-back is a job: dispatch one cargo workflow on that runner and look for `C:\ghr-fuel\cargo-home\registry` appearing (`Test-Path`), the first build re-downloads the registry into it (slower, needs network). The account's PATH is unchanged, so a job that `cargo install`s a tool and then runs it by name now installs into `<cargo-home>\bin`, which is not on PATH: watch the first run of each workflow for that **[inferred, unmeasured]**.

**A1 on a runner that is offline or not listed** (`gh api repos/<r>/actions/runners` from the PM): it already took its one job (ephemeral) and needs a registration anyway, so use `-Rearm`: `.\scripts\gh-runner-setup.ps1 -Repo <o>/<r> -Account <acct> -Labels <the labels it has> -Rearm -Apply`. Mind the mode: a public repo re-arms EPHEMERAL (the default); a runner that is persistent today (private repo) needs `-Private <o>/<r>` or it is re-registered as ephemeral. Read the labels and the mode from the PM's runners listing first.

## B. The ACL, after every row above has run a green cargo job

Elevated pwsh. Keep the backup file; `/save` of this tree is several hundred thousand files and takes minutes.

```powershell
# 1. backup of the CURRENT ACLs (the restore line is step 5)
icacls C:\rust /save C:\rust-acl-20261112.txt /t /c
# 2. tighten: cires keeps Modify (every lane builds as cires); Users keep read-and-execute so runner accounts can still RUN the shared toolchain
icacls C:\rust /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' 'TUF-LAPTOP\cires:(OI)(CI)M' '*S-1-5-32-545:(OI)(CI)RX'
```

Children inherit, so one command covers the tree (a child with its OWN entry would keep it: step 3 looks).

3. Read back: `icacls C:\rust` and `icacls C:\rust\.cargo\registry` show no `Authenticated Users`; `icacls C:\rust\.cargo\credentials.toml` shows what stage 1 left (or, if stage 1 was skipped, run it now: `icacls C:\rust\.cargo\credentials.toml /inheritance:r /grant:r "*S-1-5-18:F" "*S-1-5-32-544:F" "TUF-LAPTOP\cires:M"`).
4. Prove both directions. As `cires`: `cargo build` of any lane repo and `rustup show` work. As a runner account: dispatch one cargo workflow per Rust runner (all four A1 rows plus mlmf): it must build, with the registry now under its own `cargo-home`; and a step that tries to write under `C:\rust` (`cmd /c "echo x > C:\rust\.cargo\probe.txt"`) must say Access is denied (that step is the control: without it a pass proves nothing).
5. **Restore** (everything is undone, including inheritance): `icacls C:\ /restore C:\rust-acl-20261112.txt`, then step 3 again shows `Authenticated Users` back.

## What stays open

* Whether `credentials.toml` holds a crates.io token (not read): if it does, rotate it after stage 1, because it has been readable by every account.
* `dtolnay/rust-toolchain` anywhere else: it writes the toolchain, which this stage forbids to a runner account. The mlmf lane already dropped it (mlmf#243); the A1 rows' workflows were checked only for `cargo`/`rust-toolchain`/`rustup` lines, not read job by job.
* The AntSpawn launcher accounts (above) and any future runner: `gh-runner-setup.ps1` (#273) sets `CARGO_HOME` for every runner it installs from then on.
