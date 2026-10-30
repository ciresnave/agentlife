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

It printed: account `TUF-LAPTOP\gh-runner-cpufish` (standard user), root `C:\gh-runner-cpufish` with ACL SYSTEM,
Administrators, that account; runner v2.338.0 with its pinned SHA256; `ACL preflight: clean`; extract to
`C:\gh-runner-cpufish\ThinkersJournal-cpufish`; `config.cmd ... --labels aocl --ephemeral --runasservice
--windowslogonaccount .\gh-runner-cpufish`; read-back of `StartName` and state. No group membership.

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
the libraries are under `C:\Program Files\AMD\AOCL-Windows`, which `Users` can read). oneMKL is **not installed** (no
oneAPI/MKL directory under `Program Files`; `MKLROOT` unset): a oneMKL job cannot run on this laptop.
Undo: `-Remove -Apply` for the repo (another removal token), then `Remove-LocalUser gh-runner-cpufish` and delete
`C:\gh-runner-cpufish`.
