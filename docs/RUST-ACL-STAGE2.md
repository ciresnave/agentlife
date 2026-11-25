# C:\rust: tighten the shared Rust install (stage 2). A plan: nothing here has been run

Written 2026-11-12 for PM board 429 (CireSnave decides; the PM schedules; **a lane runs none of it**, every step needs an elevated shell). Facts are read, not inferred, unless marked **[inferred]**.

## Why, and the one thing that makes it two stages

`C:\rust` (the machine Rust: `.cargo` with the rustup shims and `bin`, `.rustup` with the toolchains) inherits `Authenticated Users:(M)` from `C:\`. Any job on any runner account can replace the toolchain every lane uses, and `C:\rust\.cargo\credentials.toml` (57 bytes, contents not read) is readable by all local accounts. Stage 1 (the credentials file alone) is on the board and breaks nothing. Stage 2 removes write access to the rest, for everyone but `cires`.

**What it can break:** the MACHINE environment sets `CARGO_HOME=C:\rust\.cargo` and `RUSTUP_HOME=C:\rust\.rustup`, and the existing runners' `.env` sets neither, so every runner account inherits them and WRITES its registry cache and lock files under `C:\rust\.cargo` today (newest registry write 2026-11-12). Tighten first and their `cargo` steps fail with permission denied. So each Rust runner gets its own `CARGO_HOME` first (A below), then the ACL (B).

Writers today, by folder (newest file per folder, scan 2026-11-12): `.cargo\registry\{cache,index,src}` and `.cargo\.global-cache`, `.package-cache*` (every cargo run), `.cargo\advisory-dbs` (cargo-deny), `.cargo\git`, `.cargo\bin` (cargo install), `.rustup\toolchains`, `update-hashes`, `settings.toml` (rustup update; 2026-11-12 11:31-11:38). Lanes run as `cires` **[inferred: process owners could not be read]**, who keeps Modify.

## A. Per Rust runner: a private CARGO_HOME

Which runners need A1 is read in the one table under "State read 2026-11-24" below (A1 rows: `ghr-fuel` and `ghr-baracuda`; mlmf is done; aocl is read at the elevated step; the rest need nothing). The older six-row list that stood here was superseded 2026-11-24 (PM FIX rust-acl-table-discrepancy).

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
* **Known, unmeasured:** a job that `cargo install`s a tool and then runs it by name now installs it into `<cargo-home>\bin`, which is not on the runner's PATH (only `C:\rust\.cargo\bin` is), so that step fails with "not recognized" after A1. Watch the first run of each workflow; the fix is a `.env` PATH line with `<cargo-home>\bin`, or the workflow calling the tool by full path. Nothing measured this: no job has run under a private `CARGO_HOME` yet.
* The AntSpawn launcher accounts (above) and any future runner: `gh-runner-setup.ps1` (#273) sets `CARGO_HOME` for every runner it installs from then on.

## State read 2026-11-21 (PM TASK `rust-acl-stage2-state`; read-only, nothing changed; one run, one machine)

`icacls C:\rust` and `C:\rust\.cargo` still show `Authenticated Users:(I)(M)` and `(I)(OI)(CI)(IO)(M)`; MACHINE `CARGO_HOME=C:\rust\.cargo`, `RUSTUP_HOME=C:\rust\.rustup`. Seven `RunnerService.exe` services (registry scan, every `ImagePath` match), none for humboldt or ThinkersFoundation-site on this box; `ciresnave-aocl` is registered and Stopped. A lane cannot read the runner directories (`.env`: access denied for `cires` on six of seven), so `.env` contents are NOT read except PersistAnt's (names only: `PSExecutionPolicyPreference`; no CARGO_HOME or RUSTUP_HOME). Job logs (`gh run view --log`) are the other evidence.

| runner (account) | self-hosted jobs at the default branch (parsed YAML) | private CARGO_HOME / RUSTUP_HOME | needs before B |
|---|---|---|---|
| PersistAnt (`gh-runner`) | `power-loss` x2: no cargo, rustup or toolchain step (the `dtolnay` step is in the ubuntu `guest-binary` job) | neither (.env read) | nothing |
| cpufish (`gh-runner-cpufish`) | none: every `ci.yml` job is GitHub-hosted (the old `aocl.yml` moved to ciresnave/aocl) | not read | nothing; the runner has no job |
| licence-tools (`ghr-licence-tools`) | 3 jobs, no cargo step | not read | nothing |
| baracuda (`ghr-baracuda`) | `device-tests.yml:device`, `device-tests-kernel-features.yml:kernel-features`: `dtolnay/rust-toolchain` + cargo | NO: run 44898166938 (2026-11-21 11:14Z, success) printed `CARGO_HOME: C:\rust\.cargo`, and the action ran `rustup toolchain install stable --force-non-host`, which writes `RUSTUP_HOME` | A1 (CARGO_HOME) AND a workflow change (drop `dtolnay` as mlmf#243 did) or a private RUSTUP_HOME: under B the toolchain install is denied **[inferred, unmeasured]** |
| fuel (`ghr-fuel`) | 4 gpu jobs, cargo, no `dtolnay`, no `rustup` | not read; the run logs print no CARGO_HOME | A1 |
| mlmf (`ghr-mlmf-corpus`) | `corpus-tests.yml:corpus` (gated by `vars.CORPUS_RUNNER_ENABLED`), cargo, no `dtolnay` | YES for CARGO_HOME: run 44908109292 (2026-11-21 13:17Z, success) printed `CARGO_HOME=C:\gh-runner-ghr-mlmf-corpus\cargo-home`; RUSTUP_HOME stays `C:\rust\.rustup` (shared, read-only use) | nothing |
| aocl (`ghr-aocl`) | `selfhosted.yml:scalapack`, dispatch only, cargo | not read (run 44915169257, success 14:40Z, prints no CARGO_HOME); the script writes it for new runners | read `.env` names elevated once, else probe like mlmf |

**Order:** stage 1 (credentials.toml) any time; the baracuda workflow PR (drop `dtolnay`) first; A1 for fuel and baracuda between jobs; one green job each with a CARGO_HOME probe step; read the aocl `.env` names (elevated: `Get-Content <dir>\.env | % { ($_ -split '=',2)[0] }`); then B with its backup (`icacls C:\rust /save C:\rust-acl-20261112.txt /t /c`) and restore (`icacls C:\ /restore C:\rust-acl-20261112.txt`) exactly as above, then the deny control from step 4. The commands above are unchanged and were NOT run by this lane.

## State read 2026-11-24 (PM TASK `rust-acl-stage2-refresh`; read-only, nothing applied; one run, one machine)

**Live service registry** (HKLM `Services` with a `RunnerService.exe` ImagePath, since `Get-CimInstance Win32_Service` throws here): the same seven services as 2026-11-21, no new runner (no spawn, wsl or humboldt service). Six Running; `actions.runner.ciresnave-aocl...` Stopped. `C:ust` still has `Authenticated Users:(I)(M)` and `(I)(OI)(CI)(IO)(M)`; MACHINE `CARGO_HOME=C:ust\.cargo`. Runner `.env` files are unreadable to a lane (access denied on fuel and baracuda), so `.env` is not read; job logs are the evidence.

| runner (account) | mark | evidence |
|---|---|---|
| fuel (`ghr-fuel`) | **A1** | run 45305681272 (2026-11-24 01:43Z, success) printed `CARGO_HOME=C:ust\.cargo`, `RUSTUP_HOME=C:ust\.rustup`; no `dtolnay` in its self-hosted jobs |
| baracuda (`ghr-baracuda`) | **A1** (workflow half done) | `device-tests.yml` and `device-tests-kernel-features.yml` on origin/main no longer use `dtolnay/rust-toolchain` (the comment cites this stage); run 45296368885 (2026-11-24 00:20Z) still printed `CARGO_HOME=C:ust\.cargo`, so the `.env` change is what remains. `ci.yml` still has `dtolnay`: its `runs-on` was not read here |
| mlmf (`ghr-mlmf-corpus`) | **done** for CARGO_HOME (2026-11-21 run 44908109292 printed the private path; not re-read today); RUSTUP_HOME stays shared read-only | |
| aocl (`ghr-aocl`) | **unread**: service Stopped, `.env` names unread, no job today | read the `.env` names elevated first (command in the 2026-11-21 section) |
| PersistAnt, cpufish, licence-tools | **none** | no cargo step at the default branch on 2026-11-21; not re-read job by job today |
| antspawn accounts | **none** (not services) | set `CARGO_HOME` in the launcher before a first Rust job |

**The scripts** (this lane wrote `scripts/rust-acl-stage2.ps1`; **dry run by default, `-Apply` needs an elevated shell and is never run by a lane**). Run unelevated with no `-Apply` on 2026-11-24: `-Step A1 -Runner ghr-fuel`, `-Step A1 -Runner ghr-baracuda` and `-Step B` each found the one service, printed the exact commands and changed nothing (outputs in the PR). **Not run:** `-Undo` for either step and the `-Apply` elevation refusal (a permission classifier declined that batch; the code path is read, not exercised), and no `-Apply` anywhere. The A1 block is the plan's, with a refusal to run twice (the `.env.bak-<stamp>` must not exist). One-line restores: A1 `-Step A1 -Runner <acct> -Undo -Apply` (copies the `.env` backup back, restarts the service); B `-Step B -Undo -Apply` (`icacls C:\ /restore C:ust-acl-20261112.txt`).

**Job in flight:** a service stop kills a running job. None of the runners was read for a policy; before each A1, from the PM: `gh run list -R <repo> --status in_progress` must be empty. No runner is known to wait for a job to finish on its own.

**ONE ordered list for the board (a single elevated item, runner stops only between jobs):**
1. Stage 1: `icacls C:ust\.cargo\credentials.toml /inheritance:r /grant:r *S-1-5-18:F *S-1-5-32-544:F TUF-LAPTOP\cires:M` if not already done (the PM reports it done 2026-11-12; read the ACL to confirm).
2. `rust-acl-stage2.ps1 -Step A1 -Runner ghr-baracuda -Apply`, after no baracuda job is in progress; dispatch one device-tests run and look for `C:\ghr-baracuda\cargo-homeegistry`.
3. The same for `ghr-fuel`.
4. Read the aocl `.env` names; give it A1 only if it lacks `CARGO_HOME`.
5. `rust-acl-stage2.ps1 -Step B -Apply` (backup, then tighten), then the deny control and one cargo job per Rust runner (fuel, baracuda, mlmf, aocl if used).
6. Restore if any step 5 job fails: `-Step B -Undo -Apply`.
