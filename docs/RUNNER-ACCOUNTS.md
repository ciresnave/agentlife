# A runner account per runner: design, tests first

Written 2026-10-30 at `origin/main` 7ab6da2 (0.29.42). PM RULING 2026-10-30: do **not** reuse the shared `gh-runner`
account for a new runner (password rotation across live services through a path that has never run on a real service);
each runner gets its own restricted account, or a dry-run-proven add-only path; design and tests first. **Design only.
Nothing in this note was run on the machine except read-only queries and the script's dry run** (stated per fact).

## Measured on this laptop, 2026-10-30 (read-only)

| Fact | Value |
|---|---|
| Runner services | 3, all Running/Automatic: `ciresnave-PersistAnt` as `.\gh-runner`; `ThinkersJournal-licence-tools` and `ThinkersJournal-ThinkersFoundation-site` as `.\gh-runner-private` |
| Local accounts | `gh-runner`, `gh-runner-private` (both Enabled); neither in Administrators (members: `Administrator`, `cires`) |
| `Hyper-V Administrators` | `cires`, **`gh-runner`** (PersistAnt's `machine-durability.yml` needs it: RUNNER-SETUP 8b) |
| `C:\gh-runner` ACL | `gh-runner-private:(OI)(CI)(M)`, `gh-runner:(OI)(CI)(M)`, Administrators `F`, SYSTEM `F`, `cires` `F` |
| `C:\gh-runner\ciresnave-PersistAnt` ACL | the same entries, **inherited** (`(I)(OI)(CI)(M)` for both accounts), plus the runner's own `GITHUB_ActionsRunner_*` group |
| cpufish | public, `main`; its only workflow `ci.yml` has **no self-hosted job** yet (read from `main`) |
| AOCL | `C:\Program Files\AMD\AOCL-Windows` exists; `BUILTIN\Users` has read-and-execute on it (inherited), so a standard account can read it |
| oneAPI / oneMKL | **installed** at `C:\Program Files (x86)\Intel\oneAPI` (`2026.1`, `latest`; `mkl\latest\include\mkl.h` present; `icpx.exe` in `compiler\latest\bin`); `BUILTIN\Users` has read-and-execute (inherited). `MKLROOT` unset. (An earlier version of this table said it was absent: it looked under `Program Files` only.) |

## What is wrong with reusing the account

1. **Rotation reaches every service on the account.** `gh-runner-setup.ps1` on an existing account sets a new random
   password and then, for each service logging on as it, calls `Win32_Service.Change` with that password (`Select-AccountServices`
   and the `Invoke-CimMethod ... Change` loop). **That `Change` call has never run on a real service** (my HANDOFF's
   standing note on #161; I did not re-test it, and a test of it needs an elevated shell and a live service). A failure leaves a service whose password is one nobody knows,
   which fails at its next start.
2. **The shared root gives every runner account write access to every runner's files (new finding).** Step 3 of the
   script runs `icacls $Root /inheritance:r /grant:r SYSTEM:F Administrators:F <account>:(OI)(CI)M`. `/grant:r` replaces
   only that account's entry, so the entries of earlier accounts stay, and each new account is added with **Modify on the
   whole root, inherited into every runner directory**. Measured above: `gh-runner-private` can rewrite
   `C:\gh-runner\ciresnave-PersistAnt\bin\RunnerService.exe`, which the service starts as `gh-runner` (a member of
   Hyper-V Administrators), and `gh-runner` can do the same to the private-repo runners. A job on any runner can
   therefore run code as the other runner's account at that service's next start. A new account added the same way
   widens this by one more principal. Separate accounts do **not** isolate runners while the root is shared.
3. **Group membership has no home.** Nothing in the script adds a group (good), but the one membership that matters
   (`Hyper-V Administrators`) was added by hand, so a plan cannot list it.

## Design

**One account, one root, one runner.** Three changes to `gh-runner-setup.ps1`, in this order, each with its tests first.

1. **Add-only by default.** `-Account` defaults to a name derived from the repo, `ghr-<repo>` cut to the 20-character
   SAM limit (a stable 4-hex suffix from the repo's slug when it is cut, so two long names cannot collide); it is
   still overridable. If the account **exists and has any dependent service**, the script **refuses** and says so; the old
   rotate-and-`Change` path stays but only behind `-RotateShared`, documented as never run on a live service. An
   existing account with no services is rotated as before (no dependent to break). *Plan line:* `account: create |
   exists, 0 services, password rotated | REFUSED, 2 services`.
2. **A root per account, or a per-directory ACL.** The simplest shape that needs no new ACL logic: the default `-Root`
   becomes `C:\gh-runner\<account>`-style siblings (`C:\gh-runner-<account>`), so `icacls $Root ... <account>:M` is the
   account's own tree and no other runner's. For the three existing runners, which share `C:\gh-runner`, the script
   **prints** (does not run) the elevated commands that remove the other accounts' root-level entries and grant each account
   Modify on its own directory only; CireSnave decides, because it changes live services' access (RUNNER-SETUP 2c's
   pattern). *Plan line:* `root ACL: SYSTEM, Administrators, <account> (own tree only)`.
3. **Groups are listed, never defaulted.** `-AddToGroup <name> -Because <workflow path>` adds the account to one named
   local group and writes the reason into the account's description; the plan prints `group memberships: none` or the
   list with the reason. `Administrators` and `Remote Desktop Users` stay refused. The only known use today is
   `Hyper-V Administrators` for PersistAnt's `machine-durability.yml`, and that account already exists.

`-Remove` gains `-RemoveAccount`: after the service is gone, remove the account only when no other service uses it.

## Tests first (extend `scripts/tests/*.tests.ps1`, run by `tests/runner_setup_*.rs` on the Windows leg)

| # | Asserts | Control / mutation |
|---|---|---|
| T1 | name derivation: at most 20 characters, deterministic, two long repos differ, `ThinkersJournal/cpufish` is `ghr-cpufish` | a name of 21 characters fails |
| T2 | an existing account with dependents is refused without `-RotateShared`, **before** `Set-LocalUser` is called (fake `Get-LocalUser`/CIM objects, as `Select-AccountServices` is already tested) | with `-RotateShared` the old path runs; mutation: remove the guard, T2 goes red |
| T3 | real ACLs in a temp dir: configure two "runners" with two principals; neither principal has any write right on the other's directory, and the root grants no runner principal | mutation: restore the shared-root grant, T3 goes red |
| T4 | the dry-run plan contains `group memberships: none` by default and lists a group with its `-Because` | `-AddToGroup Administrators` is refused |
| T5 | the dry run changes nothing: no account, no directory (the control is the same call with the switch that creates) | |

Not testable without elevation, and said so in the script: the real `New-LocalUser`, `icacls` on the real root, and the
service configuration. Those are read back by the script itself after the elevated run (service `StartName`, state,
directory ACL), as today.

## Slices

1. T1, T2, T4, T5 + add-only default + group flag (no ACL change). 2. Per-account root + T3. 3. The printed migration for
the three live runners, run by CireSnave; then `-CheckAclsOnly`-style read-back of each runner directory.

## cpufish, one paste (works today, no script change)

cpufish needs a runner for AOCL provider tests (RUNNER-SETUP 5.1h: tests that cannot run on GitHub run on the
self-hosted runner, same conditions). **Public repo, so ephemeral: one job per registration, and each registration
needs a fresh token and an elevated `-Apply` with CireSnave at the keyboard** (RUNNER-SETUP 2a, "until then a device
job is a manual `-Apply`"). That cost should be known before cpufish writes a job that runs on push and on a schedule.

The script already takes `-Account` and `-Root`, so a new account in its own root avoids both problems above with no
change. (The paste names the account `gh-runner-cpufish`, as the dry run did; the derived default in the design above
would be `ghr-cpufish`.) **Dry run, run 2026-10-30 (nothing created; `C:\gh-runner-cpufish` and the account were absent before and after):**

```
pwsh -File C:\Projects\agentlife\scripts\gh-runner-setup.ps1 -Repo ThinkersJournal/cpufish `
  -Account gh-runner-cpufish -Root C:\gh-runner-cpufish -Labels aocl
```

**Re-run 2026-10-30 at `origin/main` ce69169 (exit 0; before and after, `Get-LocalUser gh-runner-cpufish` found nothing and
`Test-Path C:\gh-runner-cpufish` was False; the ACL preflight read `clean`).** Its output, unedited:

```
DRY RUN (nothing changed, no network). Pass -Apply from an elevated shell to do this.

Owners        : ciresnave, ThinkersJournal   private (persistent allowed): none: every -Repo is treated as public, so ephemeral
Runner mode   : ephemeral (one job per registration) [default: a -Repo is not declared in -Private]
Account       : TUF-LAPTOP\gh-runner-cpufish (standard user; random 32-char password held in memory only)
Root          : C:\gh-runner-cpufish  (ACL: SYSTEM, Administrators, gh-runner-cpufish)
Runner        : v2.338.0  SHA256 f48e0750a21812bca5f82de5f7f5aeae71abee647fab5a582f1742d07eba455f
  https://github.com/actions/runner/releases/download/v2.338.0/actions-runner-win-x64-2.338.0.zip
ACL preflight: clean (-Apply would proceed past it)

[ThinkersJournal/cpufish]
  extract  -> C:\gh-runner-cpufish\ThinkersJournal-cpufish (only after the hash matches)
  token    -> prompted (Read-Host -AsSecureString): PM fetches it now; not stored
  C:\gh-runner-cpufish\ThinkersJournal-cpufish\config.cmd --unattended --url https://github.com/ThinkersJournal/cpufish --token <token> --name TUF-LAPTOP-ThinkersJournal-cpufish `
      --labels aocl --work _work --replace --ephemeral --runasservice --windowslogonaccount .\gh-runner-cpufish --windowslogonpassword <generated>
  .env     -> C:\gh-runner-cpufish\ThinkersJournal-cpufish\.env: PSExecutionPolicyPreference=Bypass (before config.cmd, so the service starts with it)
  read back: service ... StartName = .\gh-runner-cpufish, State = Running; .env holds the policy line
```

No group membership is granted. Board 339: CireSnave chose this option (A: own account, own root, label `aocl`, ephemeral).

**The elevated paste** (CireSnave's shell; the PM fetches the token with the `ciresnave` token for that one command, in
the same terminal, within the hour):

```
# 1. PM, any shell: the one-hour registration token (copy it; it is not stored)
GH_TOKEN="$(gh auth token --user ciresnave)" gh api -X POST repos/ThinkersJournal/cpufish/actions/runners/registration-token --jq .token

# 2. CireSnave, ELEVATED PowerShell: paste the token at the prompt
pwsh -File C:\Projects\agentlife\scripts\gh-runner-setup.ps1 -Repo ThinkersJournal/cpufish `
  -Account gh-runner-cpufish -Root C:\gh-runner-cpufish -Labels aocl -Apply

# 3. Read back (PM): labels must read self-hosted, Windows, X64, aocl
GH_TOKEN="$(gh auth token --user ciresnave)" gh api repos/ThinkersJournal/cpufish/actions/runners --jq '.runners[]|{name,status,labels:[.labels[].name]}'
```

Before step 2: cpufish's workflow must have the AOCL job (`runs-on: [self-hosted, Windows, X64, aocl]`, no
`pull_request` trigger without the same-repo condition and environment gate, no secrets, passing `runner-lint`); today
its `ci.yml` has none, so the runner would sit idle. The job must find AOCL itself (`AOCL_ROOT` is unset on the machine;
the libraries are under `C:\Program Files\AMD\AOCL-Windows`, which `Users` can read). **oneAPI/oneMKL IS installed**, at
`C:\Program Files (x86)\Intel\oneAPI` (version `2026.1`, with `latest`; `mkl\latest\include\mkl.h` exists; the compiler
dir holds `icpx.exe`), and `BUILTIN\Users` has read-and-execute on it (inherited), so the standard account can read it.
An earlier version of this note said it was not installed: it looked under `C:\Program Files` only, and oneAPI lives
under `Program Files (x86)`. `MKLROOT` and `ONEAPI_ROOT` are unset in the machine environment (read 2026-10-30), so a
oneMKL job must find the install itself (for example `"C:\Program Files (x86)\Intel\oneAPI\setvars.bat"`, or `MKLROOT`
set to `...\oneAPI\mkl\latest`). Whether a service in session 0 can use it is not measured here.

**Undo block** (no group was granted, so there is nothing else to revert). The runner is ephemeral: after its one job it
unregisters itself, so step 1 is only needed if it never ran a job or is still registered.

```
# 1. PM, any shell: a removal token (one hour; copy it, it is not stored). Skip if the runner is already gone:
GH_TOKEN="$(gh auth token --user ciresnave)" gh api repos/ThinkersJournal/cpufish/actions/runners --jq '.runners[]|{name,status}'
GH_TOKEN="$(gh auth token --user ciresnave)" gh api -X POST repos/ThinkersJournal/cpufish/actions/runners/remove-token --jq .token

# 2. CireSnave, ELEVATED PowerShell (paste the removal token at the prompt):
pwsh -File C:\Projects\agentlife\scripts\gh-runner-setup.ps1 -Repo ThinkersJournal/cpufish `
  -Account gh-runner-cpufish -Root C:\gh-runner-cpufish -Remove -Apply
Remove-LocalUser -Name gh-runner-cpufish
Remove-Item -LiteralPath C:\gh-runner-cpufish -Recurse -Force

# 3. Read back (absent, with the control that the same query finds the live gh-runner account):
Get-LocalUser gh-runner-cpufish -ErrorAction SilentlyContinue; Test-Path C:\gh-runner-cpufish
Get-LocalUser gh-runner
```


## Changeover for the live runners (board 340 option A; read-only, dry runs only, 2026-10-30, `origin/main` 4ba0d58)

**Nothing here was run with `-Apply`.** Every fact below was read from this machine, or from GitHub with the bot token where it could read.

### Measured state (before)

| Runner | Service (`StartName`, state) | Directory | Mode / labels | Repo |
|---|---|---|---|---|
| `ciresnave/PersistAnt` | `.\gh-runner`, Running, Auto | `C:\gh-runner\ciresnave-PersistAnt` | labels and `ephemeral` **not readable by the bot** (HTTP 403 on `actions/runners`); `.runner` has no ephemeral field. PM: command below | **public** |
| `ThinkersJournal/licence-tools` | `.\gh-runner-private`, Running, Auto | `C:\gh-runner\ThinkersJournal-licence-tools` | `self-hosted, Windows, X64`; online, `busy:false` (GitHub); persistent | private |
| `ThinkersJournal/ThinkersFoundation-site` | `.\gh-runner-private`, **Stopped**, Auto | `C:\gh-runner\ThinkersJournal-ThinkersFoundation-site` | `self-hosted, Windows, X64`; **offline** (GitHub); persistent | **public** (`gh api repos/... --jq .private` = `false`) |

`C:\gh-runner` root ACL now: `gh-runner (OI)(CI)(M)`, Administrators `F`, SYSTEM `F`, `cires` `F` (`gh-runner-private` removed by the 340-C step). `gh-runner` still has inherited Modify on both private runners' directories; `gh-runner-private` has explicit Modify on its own two. No `Runner.Worker` process was running. `Hyper-V Administrators`: `cires`, `gh-runner`; `Administrators`: `Administrator`, `cires`.

**Two findings that change the plan.**
1. **`ThinkersFoundation-site` is a public repo with a persistent runner**, which RUNNER-SETUP 2a forbids (public means ephemeral). `-Apply` would refuse `-Private` for it. Its service is stopped and the runner offline, so nothing is running now. The paste below **removes it and re-registers it ephemeral** (the script's default for a repo not named in `-Private`). Ephemeral costs a fresh registration token and an elevated `-Apply` per job; its `ci.yml` is `ubuntu-latest` only (section 8a), so **recommendation: do step 1 (remove) tonight and register it only when a job needs the runner.** CireSnave's call.
2. **The password-rotation path is never touched.** Each runner gets a *new* account, so `Set-LocalUser` and `Win32_Service.Change` never run on a live service. The order below removes `ThinkersFoundation-site` first so `gh-runner-private` has one dependent service, not two, in the undo.

### Decisions in the plan

- **PersistAnt: service, account, registration and files untouched.** It keeps `gh-runner` and with it `Hyper-V Administrators`. The line that needs it, `.github/workflows/machine-durability.yml` at PersistAnt `main` 82dcaad (read with `gh api`): line 49 `runs-on: [self-hosted, Windows, X64]` under line 46 `power-loss:` (`name: Power loss (Hyper-V guest, self-hosted)`; line 4: "It cannot run on a GitHub-hosted runner (no nested Hyper-V)"). `synapse-power-cut.yml` has the same job on `workflow_dispatch` (section 8b). No other account gets a group.
- New accounts: `ghr-licence-tools`, `ghr-tf-site` (standard users, no group, random password in memory), each in its **own root** (`C:\ghr-licence-tools`, `C:\ghr-tf-site`), so neither account nor `gh-runner` can write the other's tree. `gh-runner-private` is then unused and is removed at the end.
- Labels: none added (both read `self-hosted, Windows, X64` on GitHub, which `config.cmd` gives automatically).

### Dry runs (unedited; run 2026-10-30; exit 0 each; `C:\ghr-licence-tools`, `C:\ghr-tf-site`, `Get-LocalUser ghr-*` were absent before; nothing was created)

```
pwsh -File C:\Projects\agentlife\scripts\gh-runner-setup.ps1 -Repo ThinkersJournal/licence-tools -Private ThinkersJournal/licence-tools -Account ghr-licence-tools -Root C:\ghr-licence-tools
```

```
DRY RUN (nothing changed, no network). Pass -Apply from an elevated shell to do this.

Owners        : ciresnave, ThinkersJournal   private (persistent allowed): ThinkersJournal/licence-tools
Runner mode   : persistent (one registration, many jobs) [default: every -Repo is declared in -Private]
Account       : TUF-LAPTOP\ghr-licence-tools (standard user; random 32-char password held in memory only)
Root          : C:\ghr-licence-tools  (ACL: SYSTEM, Administrators, ghr-licence-tools)
Runner        : v2.338.0  SHA256 f48e0750a21812bca5f82de5f7f5aeae71abee647fab5a582f1742d07eba455f
  https://github.com/actions/runner/releases/download/v2.338.0/actions-runner-win-x64-2.338.0.zip
ACL preflight: clean (-Apply would proceed past it)

[ThinkersJournal/licence-tools]
  extract  -> C:\ghr-licence-tools\ThinkersJournal-licence-tools (only after the hash matches)
  token    -> prompted (Read-Host -AsSecureString): PM fetches it now; not stored
  C:\ghr-licence-tools\ThinkersJournal-licence-tools\config.cmd --unattended --url https://github.com/ThinkersJournal/licence-tools --token <token> --name TUF-LAPTOP-ThinkersJournal-licence-tools `
      --work _work --replace --runasservice --windowslogonaccount .\ghr-licence-tools --windowslogonpassword <generated>
  .env     -> C:\ghr-licence-tools\ThinkersJournal-licence-tools\.env: PSExecutionPolicyPreference=Bypass (before config.cmd, so the service starts with it)
  read back: service ... StartName = .\ghr-licence-tools, State = Running; .env holds the policy line
```

```
pwsh -File C:\Projects\agentlife\scripts\gh-runner-setup.ps1 -Repo ThinkersJournal/ThinkersFoundation-site -Account ghr-tf-site -Root C:\ghr-tf-site
```

```
DRY RUN (nothing changed, no network). Pass -Apply from an elevated shell to do this.

Owners        : ciresnave, ThinkersJournal   private (persistent allowed): none: every -Repo is treated as public, so ephemeral
Runner mode   : ephemeral (one job per registration) [default: a -Repo is not declared in -Private]
Account       : TUF-LAPTOP\ghr-tf-site (standard user; random 32-char password held in memory only)
Root          : C:\ghr-tf-site  (ACL: SYSTEM, Administrators, ghr-tf-site)
Runner        : v2.338.0  SHA256 f48e0750a21812bca5f82de5f7f5aeae71abee647fab5a582f1742d07eba455f
  https://github.com/actions/runner/releases/download/v2.338.0/actions-runner-win-x64-2.338.0.zip
ACL preflight: clean (-Apply would proceed past it)

[ThinkersJournal/ThinkersFoundation-site]
  extract  -> C:\ghr-tf-site\ThinkersJournal-ThinkersFoundation-site (only after the hash matches)
  token    -> prompted (Read-Host -AsSecureString): PM fetches it now; not stored
  C:\ghr-tf-site\ThinkersJournal-ThinkersFoundation-site\config.cmd --unattended --url https://github.com/ThinkersJournal/ThinkersFoundation-site --token <token> --name TUF-LAPTOP-ThinkersJournal-ThinkersFoundation-site `
      --work _work --replace --ephemeral --runasservice --windowslogonaccount .\ghr-tf-site --windowslogonpassword <generated>
  .env     -> C:\ghr-tf-site\ThinkersJournal-ThinkersFoundation-site\.env: PSExecutionPolicyPreference=Bypass (before config.cmd, so the service starts with it)
  read back: service ... StartName = .\ghr-tf-site, State = Running; .env holds the policy line
```

The two `-Remove` dry runs (`-Repo ... -Root C:\gh-runner -Remove`, exit 0) print `config.cmd remove --token <removal token>` for `C:\gh-runner\ThinkersJournal-licence-tools` and `...-ThinkersFoundation-site` (they also print an Account/Mode header for the default `gh-runner`, which `-Remove` ignores).

### Order

0. **No-jobs precheck (PM, any shell)**: every `busy` must be `false` and the list must be what is expected.
1. `ThinkersFoundation-site`: remove (offline already).
2. `licence-tools`: remove, register under `ghr-licence-tools`. Short window with no licence-tools jobs.
3. Root ACL: `gh-runner` loses its entry on `C:\gh-runner`, after its explicit grant on PersistAnt's folder is confirmed.
4. Delete the old private directories and `gh-runner-private`.
5. (Only when a job needs it) register `ThinkersFoundation-site` ephemeral under `ghr-tf-site`.

### The paste

```
# 0. PM, any shell: no jobs running. busy must be false everywhere; PersistAnt's labels/mode are read here for the first time.
$env:GH_TOKEN = (gh auth token --user ciresnave)
gh api repos/ciresnave/PersistAnt/actions/runners --jq '.runners[]|{name,status,busy,labels:[.labels[].name]}'
gh api repos/ThinkersJournal/licence-tools/actions/runners --jq '.runners[]|{name,status,busy}'
gh api repos/ThinkersJournal/ThinkersFoundation-site/actions/runners --jq '.runners[]|{name,status,busy}'
# also, elevated or not: no job process is running on the machine
Get-Process Runner.Worker -ErrorAction SilentlyContinue

# 1. PM: a removal token for the site runner (one hour; copy it, it is not stored). CireSnave, ELEVATED PowerShell, paste it at the prompt:
gh api -X POST repos/ThinkersJournal/ThinkersFoundation-site/actions/runners/remove-token --jq .token
pwsh -File C:\Projects\agentlife\scripts\gh-runner-setup.ps1 -Repo ThinkersJournal/ThinkersFoundation-site -Root C:\gh-runner -Remove -Apply

# 2. PM: a removal token, then (after it is used) a registration token for licence-tools. CireSnave, ELEVATED:
gh api -X POST repos/ThinkersJournal/licence-tools/actions/runners/remove-token --jq .token
pwsh -File C:\Projects\agentlife\scripts\gh-runner-setup.ps1 -Repo ThinkersJournal/licence-tools -Root C:\gh-runner -Remove -Apply
gh api -X POST repos/ThinkersJournal/licence-tools/actions/runners/registration-token --jq .token
pwsh -File C:\Projects\agentlife\scripts\gh-runner-setup.ps1 -Repo ThinkersJournal/licence-tools -Private ThinkersJournal/licence-tools -Account ghr-licence-tools -Root C:\ghr-licence-tools -Apply

# 3. CireSnave, ELEVATED: PersistAnt's explicit grant is already there (explicit, not inherited); confirm, then drop gh-runner from the shared root
icacls C:\gh-runner\ciresnave-PersistAnt
icacls C:\gh-runner /remove:g gh-runner
Remove-Item -LiteralPath C:\gh-runner\ThinkersJournal-licence-tools, C:\gh-runner\ThinkersJournal-ThinkersFoundation-site -Recurse -Force
# only when the read-back below shows no service on gh-runner-private:
Remove-LocalUser -Name gh-runner-private
```

**Read back (CireSnave or PM; each is a state read, not an exit code):**

```
Get-CimInstance Win32_Service -Filter "Name like 'actions.runner.%'" | % { "$($_.Name) | $($_.StartName) | $($_.State)" }
#   expect: PersistAnt | .\gh-runner | Running ; licence-tools | .\ghr-licence-tools | Running ; no site service
icacls C:\gh-runner ; icacls C:\ghr-licence-tools ; Get-LocalGroupMember 'Hyper-V Administrators'
#   expect: C:\gh-runner has no gh-runner and no gh-runner-private entry; Hyper-V Administrators = cires, gh-runner only
net localgroup Administrators                         # unchanged: Administrator, cires
Get-LocalUser ghr-licence-tools; Get-LocalUser gh-runner-private -ErrorAction SilentlyContinue   # first found; second absent (control: the first)
gh api repos/ThinkersJournal/licence-tools/actions/runners --jq '.runners[]|{name,status,labels:[.labels[].name]}'   # online; self-hosted, Windows, X64
gh api repos/ciresnave/PersistAnt/actions/runners --jq '.runners[]|{name,status}'   # still online; never touched
```

Then dispatch one licence-tools job and read that it ran on `ghr-licence-tools` (the job's `whoami`, or the run log).

### UNDO

Each undo step uses the script's tested path (new account, so no password rotation). **If step 2's registration fails after the removal**, the runner is gone and licence-tools has no runner until this is run:

```
# PM: a registration token. CireSnave, ELEVATED. gh-runner-private has no service at this point (site removed, licence-tools removed), so the script's rotate-and-Change path has no dependent to break:
pwsh -File C:\Projects\agentlife\scripts\gh-runner-setup.ps1 -Repo ThinkersJournal/licence-tools -Private ThinkersJournal/licence-tools -Account gh-runner-private -Root C:\gh-runner -Apply
```

That recreates exactly the old shape (account `gh-runner-private`, root `C:\gh-runner`, label-free). The script then re-adds the account's Modify on the whole `C:\gh-runner` root (its step 3), so afterwards re-run the 340-C ACL fix, or accept the pre-340 exposure until step 3 is repeated; say so to the PM. To undo only the new registration first, if it exists: removal token, then `... -Repo ThinkersJournal/licence-tools -Root C:\ghr-licence-tools -Remove -Apply`, `Remove-LocalUser ghr-licence-tools`, `Remove-Item C:\ghr-licence-tools -Recurse -Force`. Do **not** run `Remove-LocalUser gh-runner-private` until the undo is no longer wanted. PersistAnt needs no undo: nothing about it changes except the root-level `gh-runner` entry, restored with `icacls C:\gh-runner /grant "gh-runner:(OI)(CI)M"`.

### Not measured, so not claimed

- PersistAnt's runner labels and ephemeral mode (step 0 reads them); whether Public-repo section 2a is met for it (section 8b already flags this).
- That `config.cmd` and the service come up under a brand-new account on this box: the script's comments record a live `-Apply` on 2026-10-22 (ThinkersFoundation-site); this session did not re-verify what that run created, and the cpufish new-account run is still only a dry run. Step 2 is the first new-account run I know of.
- The per-user caches and TEMP of the old accounts (open since 340-C).
- Whether `gh-runner` can still write the *new* roots: it is not in either ACL (SYSTEM, Administrators, the account), read in the dry-run plan, not on disk yet.
