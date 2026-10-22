# Self-hosted GitHub runners (board 221, EXPECTATIONS 5.1g/5.1h)

CireSnave, 2026-10-16, verbatim: *"I think we should set up local runners.  I agree with all of your conditions on
them.  Lets get that going."* and, widening it, *"I mean that policy should apply to all of our repos...not just that
one."* GitHub's docs say self-hosted runner usage is free (read by the PM 2026-10-16).

**Scope: every repo owned by `ciresnave` or `ThinkersJournal`**, registered the first time one of its tests needs a
device (GPU) or anything else GitHub's hosted runners cannot give. Tests that cannot run on GitHub run on the runner
under this policy; they are never `#[ignore]`d.

## 1. Policy (agreed 2026-10-16: *"Agreed.  Proceed."*)

1. **Device tests run on the runner on `push` to `main`, `schedule` and `workflow_dispatch`.**
2. **A same-repo PR may run on the runner only behind BOTH** `github.event.pull_request.head.repo.full_name ==
   github.repository` **and** a job `environment:` whose protection rule has a required reviewer.
3. **A fork PR NEVER runs on the runner.** It gets hosted tests only. **An unreviewed fork PR's device behaviour is
   UNTESTED**, and nothing here pretends otherwise.
4. **Default for a fork PR:** review it, hosted tests, merge, the device run happens post-merge on `main`, and the
   merge is **reverted if that run is red**.
5. **Later option (not built):** a maintainer `workflow_dispatch` that names the exact reviewed SHA.
6. **A restricted Windows account, not CireSnave's**, and no secrets beyond what a job needs.
7. **Public repo (the default assumption): ephemeral runner, the repo setting requiring approval for outside
   contributors' fork workflows, the lint in CI, and no secrets.** All four before any `-Apply` (section 2a).

A self-hosted runner executes whatever the repo's workflows say, as the runner's account, on the real machine and
GPU. There is no sandbox, so the policy is the whole security model, and each rule has a mechanism:

| Rule | Mechanism | Where |
|---|---|---|
| only our repos | `-Owners` (default `ciresnave`, `ThinkersJournal`); a `-Repo` whose owner is not listed is refused before anything changes; each repo is named explicitly, each time | `scripts/gh-runner-setup.ps1` |
| public means ephemeral | a repo is PUBLIC unless named in `-Private`; `-Apply` reads `gh api repos/<r> --jq .private` and refuses a repo declared private that is public; `-Ephemeral:$false` is refused for any public repo | script |
| restricted account | local account `gh-runner`: standard user, refused if in Administrators or Remote Desktop Users; root directory ACL is SYSTEM, Administrators, that account | script |
| password never at rest | generated in memory (32 chars, rejection-sampled), given to `New-LocalUser` and to `config.cmd`, never written, printed or logged; an existing account is rotated the same way | script |
| rules 1 to 3 | the lint, section 6 | `crates/runner-lint`, in each repo's CI |
| no secrets beyond need | the account has no gh login, no `~\.cargo` credentials, no ssh keys; a job gets only repo secrets it names. Never add an org-wide or CireSnave token. | operator rule |

## 2. What the lint cannot see

The lint reads workflow files. It cannot see repo settings, so these stay with the PM: the environment exists, has a
**required reviewer** (the lint only checks that a job names an environment), and the fork-approval setting is on.
**Write access to a repo is the remaining trust boundary**: anyone who can push a workflow to `main` runs code as
`gh-runner`.

## 2a. Public repos: REQUIRED before any `-Apply`

Anyone can fork a public repo, open a PR and read every workflow. GitHub's own docs say *"We recommend that you only
use self-hosted runners with private repositories"* because *"forks of a public repository can run dangerous code on
your runner machine through a pull request"* (docs read 2026-10-16). All four hold before `-Apply`:

1. **Ephemeral runner.** The script registers with `--ephemeral` by default. The runner takes one job, unregisters and
   its service stops, so one job cannot leave a file, a process or a poisoned cache for the next. **Cost:** one job
   per registration, and each registration needs a fresh token from the PM. Automating re-registration needs a
   token source and is a separate decision; until then a device job is a manual `-Apply`.
2. **Repo setting** Settings > Actions > General > *Approval for running fork pull request workflows from
   contributors*: **Require approval for all external contributors**. GitHub names it this way; earlier notes here
   said "outside collaborators", which is not the label. Read from GitHub's docs, **not seen on a settings page**
   (no browser in a lane, and the repo-admin API returns 403 to the lane's account). Per the same docs, this
   overrides organisation settings and **`pull_request_target` workflows run regardless of it**, which is why the
   lint treats `pull_request_target` like `pull_request`, and never as safe.
3. **The lint** (section 6) in that repo's CI, passing.
4. **No secrets** reachable by the account or named by a self-hosted job.

A private repo may use a persistent runner (`-Private <repo>`, verified against GitHub), and the rest of the policy
still applies. GitHub's docs: environments (the required-reviewer gate in rule 2) on **GitHub Free** can only be
configured for **public** repositories; a private repo needs Pro or Team. **So for a private repo on a Free plan the
same-repo-PR path is unavailable, and only `push` to `main`, `schedule` and `workflow_dispatch` apply** (the PM's
ruling, 2026-10-16: every repo that needs the PR path is public, where Free works). The lint cannot tell the plan,
so a private repo's workflow that uses the PR path would pass the lint and then wait forever for an environment
that cannot exist.

## 2b. Private repos (CireSnave, 2026-10-16, verbatim: *"Yes.  Use self-hosted runners for all private repos."*)

GitHub-hosted Actions on our private repos fail in 2 seconds on a **billing** failure, so those repos have **no CI at
all** until their jobs run here. The private-repo branch of the policy:

1. **Every private repo uses the runner**, labels `self-hosted, Windows, X64` (automatic) and **nothing else unless
   needed**: add `gpu` (`-Labels gpu`) only for a repo whose jobs need the GPU. Most of these jobs need none.
2. **A pull-request job is allowed only if it is same-repo**: a private repo takes no outside fork PRs, and its
   fork-PR-workflows setting stays **off**. The lint's `--private` mode still demands the `github.event.pull_request.
   head.repo.full_name == github.repository` condition on such a job, so turning forks on later does not silently open
   the runner. (The setting's exact label was not read today; the PM sets and reads it.)
3. **No environment gate**: on GitHub Free, environments (required reviewers) exist only for public repos (section
   2a). So a private repo's PR path rests on the same-repo condition and on there being no forks, nothing more. The
   lint's `--private` mode therefore does not require `environment:`. It **proves** the repo is private first
   (`gh api repos/$GITHUB_REPOSITORY --jq .private` must say `true`, else exit 2), because a public repo linted as
   private would pass a PR job with no environment; the same check runs in `gh-runner-setup.ps1 -Apply`.
4. **Ephemeral or persistent for private repos: RULED (board 223, CireSnave 2026-10-16: "Go with your recommendations.")
   = Variant A, persistent, under the restricted `gh-runner` account; public repos stay ephemeral.** The script's
   default now follows `-Private`: persistent when every `-Repo` is named in it, ephemeral otherwise (one public or
   undeclared repo in the call makes the whole call ephemeral); `-Ephemeral` overrides either way, and `-Ephemeral:$false`
   is still refused for a public repo. Registration still waits on board 224 (the ACL preflight). The comparison below is
   kept as the reasoning behind the ruling. An ephemeral runner takes ONE job, so a CI run of N jobs needs N registrations,
   each with a fresh token; four repos' normal CI is dozens of jobs a day, which no one can hand-register.

   | | Variant A: persistent (**ruled**) | Variant B: ephemeral |
   |---|---|---|
   | Command per private repo | `.\scripts\gh-runner-setup.ps1 -Repo <o>/<r> -Private <o>/<r> -Apply` (the default; `-Ephemeral:$false` says the same) | `.\scripts\gh-runner-setup.ps1 -Repo <o>/<r> -Private <o>/<r> -Ephemeral:$true -Apply` |
   | Jobs per registration | unlimited: one registration, one service | one: a fresh token per job |
   | Works for normal CI today | yes | **no**, until re-registration is automated |
   | Automation needed | none | a token source (GitHub App or fine-grained PAT) and a re-register loop: a CireSnave decision |
   | Isolation between jobs | none: `_work` is reused, and a malicious same-repo change persists on the runner | each job on a fresh runner registration |
   | Why acceptable | no outsider can reach a private repo's runner (no forks, section 2b rule 2) | n/a |
   | Public repos | **refused by the script** | the only variant |

   Nothing is registered by this change; the lint and the policy do not change between the variants.

   **Exception, stated: the two Humboldt runners that already exist** (section 8a) run under CireSnave's own account,
   which breaks rule 6. They stay as they are until a restricted replacement that can use Docker exists, because
   moving them under `gh-runner` would either break the backend job (no Docker) or give a restricted account
   root-equivalent Docker access. This is a recorded exception, not a precedent; no other repo gets one.
5. `docs` rule 6 still holds: no secrets beyond a job's needs, the restricted `gh-runner` account.

What a plain Windows runner changes for a workflow written for `ubuntu-latest` (each is in the table in section 8a):
- **No Linux and no macOS runner.** A matrix leg on `ubuntu-latest` or `macos-latest` has nowhere to run; dropping it
  **loses that coverage**, and the doc below says so per job.
- **The default `run:` shell is PowerShell**, not bash. POSIX steps need `shell: bash` (or `defaults.run.shell:
  bash`). On this machine `bash` on the runner's PATH is the **WSL launcher** (`C:\Windows\System32\bash.exe`), which
  shadows Git Bash: Humboldt's workflow already works around it (a step putting Git Bash ahead, or the full path
  `C:\Program Files\Git\bin\bash.exe`). Whether the `gh-runner` service's PATH has the same trap is not measured.
- **Container actions do not run** (`EmbarkStudios/cargo-deny-action` is one): install the tool in a step instead.
- **The `gh-runner` account has no toolchains of its own.** `dtolnay/rust-toolchain`, `actions/setup-node` and
  `actions/setup-python` install per job; machine-wide tools (MSVC, Git, `gh`, Docker) must be reachable by that
  account. Not measured.

## 2c. HARD REQUIREMENT: the lane tools must not be writable by the runner account (board 224)

OverMind's `icacls` finding, **reproduced on this machine 2026-10-16** (`scripts\gh-runner-setup.ps1 -Repo <r>
-CheckAclsOnly`): `C:\Projects`, `C:\Projects\.claude-hooks` (with `with-secret.exe` and `lane-restart.exe`) and
`C:\Projects\.lane-state` carry `NT AUTHORITY\Authenticated Users: Modify`, **inherited from `C:\`**. Any local
account, `gh-runner` included, could replace `with-secret.exe` or `lane-restart.exe` and have it run as `cires` **with
vault access**, which turns "a repo's workflow runs code as `gh-runner`" into "as the owner, with the vault". It is
also why a restricted runner account is worth nothing while these ACLs stay as they are.

So the script **refuses to create or change the account** (`-Apply`) while any `-ProtectedPaths` entry (default: the
five above) has an Allow entry granting a write-capable right (write data, append, attributes, delete, change
permissions, take ownership, or the generic write/all bits) to Everyone, Authenticated Users, Users, Interactive or
the runner account itself. It prints the exact **elevated** commands and runs none of them:

    icacls "C:\Projects" /inheritance:r /grant:r "*S-1-5-18:(OI)(CI)F" "*S-1-5-32-544:(OI)(CI)F" "<you>:(OI)(CI)F"

That makes everything under `C:\Projects` readable and writable only by SYSTEM, Administrators and the owner, and **every
other local account loses access to it** (for example `CodexSandboxUsers` today has read access via `Users`): that is
the point, and whether to do it is **CireSnave's call (board 224)**; keeping read for one group is a flag: `-AlsoGrant CodexSandboxUsers`, not this script's. `-CheckAclsOnly` re-reads the
state and must pass before `-Apply`; the dry run also shows whether `-Apply` would be refused. A child with its own
(non-inherited) entry needs the same command. The check reads **Allow** entries only, so a Deny entry is not credited as
a mitigation; it does not read group membership beyond the four broad groups and the account itself.

Tested on real ACLs in a temp directory (`scripts\tests\gh-runner-setup-acl.tests.ps1`, run by
`tests\runner_setup_acl.rs` on the Windows CI leg): each broad principal with each write right is refused with the path
and the fix named; read-only grants, a missing path and a clean directory pass; a file is checked as well as a
directory; the dry run reports without throwing. Two mutations (an empty write mask; dropping the Authenticated Users SID) make it fail. It passes under Windows PowerShell 5.1 and PowerShell 7; the ACL is read with `GetAccessRules`, not `Get-Acl` or `.Access` (the first cannot load under a mismatched `PSModulePath`, the second is `$null` on a raw security object and would read as a silent "clean"). **Not
tested:** the `-Apply` refusal when elevated, which needs an elevated shell and would otherwise create the account.

## 2d. THE standard step: an MSVC environment for Windows jobs (measured gap, spike run 2, 2026-10-17)

The `gh-runner` service has no Visual Studio developer environment, so `nvcc` dies with `Cannot find compiler 'cl.exe' in
PATH` (the probe's CUDA step; measured). Any Windows job that needs `cl.exe`, `nvcc`, `lib.exe` or a `cc`-crate build must run
this step **first**; it is our own code (no third-party action to pin). It finds Visual Studio with `vswhere` (component
`Microsoft.VisualStudio.Component.VC.Tools.x86.x64`), runs `VsDevCmd.bat -arch=x64` in a scratch `.cmd`, exports the environment
to later steps through `GITHUB_ENV` and `GITHUB_PATH`, and **throws, naming the missing piece**, if vswhere, the component,
`VsDevCmd.bat` or `cl.exe` is absent. Why not baracuda's approach: its forge passes a vswhere-located `cl.exe` to nvcc as
`-ccbin`, which a `build.rs` that calls nvcc directly never gets (baracuda lane, 2026-10-17; the miss is unexplained), and its
`cargo gpu-test` wrapper `call`s `vcvars64.bat >nul`, which hides a bad path. This step never discards the tool's output and
checks `cl.exe` afterwards. The same text is the probe's step in `docs/spike/runner-spike.yml` (a test keeps them identical).

```yaml
      - name: MSVC environment (cl.exe for nvcc and cc-rs; fails loudly)
        shell: powershell
        run: |
          # THE standard step for Windows jobs that need cl.exe/nvcc (docs/RUNNER-SETUP.md section 2d). Locates Visual Studio
          # with vswhere, runs VsDevCmd.bat -arch=x64 in a scratch .cmd, and exports the resulting environment to LATER steps.
          $ErrorActionPreference = 'Stop'
          $component = 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64'
          $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
          if (-not (Test-Path -LiteralPath $vswhere)) { throw "MSVC env: vswhere.exe not found at $vswhere; install Visual Studio with the component $component" }
          $vs = & $vswhere -latest -products '*' -requires $component -property installationPath | Select-Object -First 1
          if (-not $vs) {
            # vswhere hides an instance that is incomplete or waiting for a reboot unless -all is given (measured on this box:
            # Visual Studio 2026 with isComplete=false, isRebootRequired=true). Its tools may still work: use it, say so, verify below.
            $vs = & $vswhere -all -products '*' -requires $component -property installationPath | Select-Object -Last 1
            if ($vs) { Write-Warning "MSVC env: $vs is incomplete or needs a reboot (vswhere shows it only with -all); using it anyway" }
          }
          if (-not $vs) { throw "MSVC env: no Visual Studio install has the component $component (MSVC x64/x86 build tools)" }
          $bat = Join-Path $vs 'Common7\Tools\VsDevCmd.bat'
          if (-not (Test-Path -LiteralPath $bat)) { throw "MSVC env: $bat does not exist" }
          $env:VSCMD_SKIP_SENDTELEMETRY = '1'
          $cmd = Join-Path $env:RUNNER_TEMP 'msvc-env.cmd'
          Set-Content -LiteralPath $cmd -Encoding Ascii -Value @('@echo off', "call `"$bat`" -arch=x64 -host_arch=x64 -no_logo", 'if errorlevel 1 exit /b 1', 'set')
          $dump = & cmd.exe /d /c $cmd
          if ($LASTEXITCODE -ne 0) { throw "MSVC env: VsDevCmd.bat failed (exit $LASTEXITCODE): $($dump -join ' | ')" }
          $oldDirs = @($env:PATH -split ';')
          $exported = 0
          foreach ($line in $dump) {
            if ($line -notmatch '^([^=\s][^=]*)=(.*)$') { continue }
            $k = $Matches[1]; $v = $Matches[2]
            if ($k -ieq 'PATH') {
              $env:PATH = $v
              foreach ($d in ($v -split ';')) {
                if ($d -and ($oldDirs -notcontains $d)) { [IO.File]::AppendAllText($env:GITHUB_PATH, "$d`n"); $exported++ }
              }
            }
            elseif ([Environment]::GetEnvironmentVariable($k) -ne $v) {
              [IO.File]::AppendAllText($env:GITHUB_ENV, "$k=$v`n"); $exported++
            }
          }
          $cl = Get-Command cl.exe -ErrorAction SilentlyContinue
          if (-not $cl) { throw "MSVC env: VsDevCmd ran under $vs but cl.exe is still not on PATH (is the component $component complete?)" }
          "Visual Studio : $vs"
          "cl.exe        : $($cl.Source)"
          "exported      : $exported variables or PATH entries to later steps"
```

Measured in spike run 2 (section 5a): from session 0 as `gh-runner` the step found Visual Studio 2026 (incomplete / reboot pending,
so via the `-all` fallback, with its warning) and `cl.exe` 14.51.36231 resolved; the CUDA kernel then built and ran.

## 3. The runner binary

Pinned in the script, from the official release notes (`actions/runner` v2.338.0, read 2026-10-16):
`actions-runner-win-x64-2.338.0.zip`, SHA256 `f48e0750a21812bca5f82de5f7f5aeae71abee647fab5a582f1742d07eba455f`.
The script downloads it, hashes it, and **deletes it and stops on a mismatch before extracting anything**. To
update: change `-RunnerVersion` and `-RunnerSha256` together, from the new release's notes. The hash comes from the
same host as the zip, so it catches corruption and a swapped asset, not a compromised release.

## 4. Registration tokens

Per repo, fetched by the PM at run time (`gh api -X POST repos/<owner>/<repo>/actions/runners/registration-token`)
and typed at the script's prompt (`Read-Host -AsSecureString`). It expires in an hour, is used once, and is never
stored. `config.cmd` takes it on its command line, so it is visible to administrators for the length of the call.
Removal needs a removal token the same way (`-Remove`).

**Two more defects from the same live runs (2026-10-22), fixed in the script:** the generated password may hold only letters, digits and `- _ @` (`config.cmd` is a `.cmd` file: `% ^ !` are mangled by cmd.exe, which showed as `Invalid windows credentials entered` and left a runner registered with no service), and when `gh-runner` already exists its rotated password is applied in the same run to every service that logs on as it (`Win32_Service.Change`), because a password nobody knows would otherwise break each existing runner at its next start. Tested in `scripts/tests/gh-runner-setup-service.tests.ps1` (a real cmd.exe round trip; fake services). Not run: the `Change` call against a real service, and a full `-Apply` after the fixes.

**Service names are not predictable (found 2026-10-22, live `-Apply` for `ThinkersJournal/ThinkersFoundation-site`):** the runner truncates a long service name (the real one ended `...TUF-LAPTOP-ThinkersJ-9595`; the untruncated name is 105 characters), so the script no longer builds the name. `Find-RunnerService` finds the service whose executable (`<runner dir>\bin\RunnerService.exe`) lives under the runner's directory, for the read-back after configure and for the absence check after `-Remove` (the service is looked up before `config.cmd remove`, since afterwards there is nothing to find). Two services under one directory are refused rather than guessed. Tested with fake service objects (`scripts/tests/gh-runner-setup-service.tests.ps1`), including the over-80-character case and a sibling directory that shares the prefix; not re-run against a real install.

## 5. Service or interactive: MEASURED in spike run 2 (2026-10-17)

The script installs a **Windows service** under `gh-runner`. The question is whether CUDA and Vulkan work from a
service, which runs in session 0 with no desktop.

Measured 2026-10-16 on this box (RTX 4070 Laptop + AMD 610M iGPU, driver 617.42, WDDM, CUDA 13.3 toolkit present),
**interactive session, as CireSnave's account**: `nvidia-smi -L` lists the 4070; `vulkaninfo --summary` lists both
GPUs (NVIDIA Vulkan 1.4.351). That shows only that the machine has working drivers.

**Session 0, measured in spike run 2 (section 5a):** `gh-runner` in service mode sees the 4070 through `nvidia-smi`, both GPUs
through Vulkan, and runs a CUDA kernel. (A non-interactive `S4U` task, tried first, returned *Access is denied* without elevation.)

Decision rule: if the spike prints both GPUs from the service, keep the service (it survives logoff and reboot). If
Vulkan or CUDA fails there, the alternative is an interactive runner (`run.cmd`) in a logged-on session of the
runner account: that needs the account to hold a desktop session next to CireSnave's, which is why it is the
fallback, and it is **not implemented here**.

**Rule applied (2026-10-17): keep the service.** Run 2 printed both GPUs from the service and the kernel ran, so no interactive
runner is needed. (CUDA covers the NVIDIA GPU only; the AMD 610M is visible to Vulkan, not CUDA.)

### 5a. Spike workflow (`docs/spike/runner-spike.yml`)

One private, throwaway repo, `ciresnave/runner-spike` (the PM created it, 2026-10-16, on CireSnave's "Go for the throwaway
spike"). The workflow goes in it as `.github/workflows/spike.yml`: `workflow_dispatch` and `push` to `main` only, `runs-on:
[self-hosted, windows, gpu]`, no secrets, no third-party actions, no checkout. It passes `runner-lint` (0 findings). Steps:
`whoami` and the **session id** (0 means a service with no desktop), a write attempt on the three protected paths (each must
say False), `nvidia-smi -L` and a query, `vulkaninfo --summary`, and a **CUDA kernel launch** (compile and run a 4-thread
kernel; `nvcc --version` or `nvidia-smi -q` would only show the toolkit and NVML exist). A missing tool prints `NOT ON PATH`
or `UNMEASURED`, never a pass: the `gh-runner` account has none of CireSnave's PATH. **Results below.**

**Results, run 1 (2026-10-17, PM, `-Apply -Labels gpu` from agentlife 0.16.18 scripts, run
https://github.com/ciresnave/runner-spike/actions/runs/39039080872): the setup worked, the job ran, every step FAILED, nothing
was measured.** Setup: elevated `-Apply` created `gh-runner`, installed runner 2.338.0 (SHA256 verified), service Running as
`.\gh-runner`, labels `self-hosted,Windows,X64,gpu`, online. The dispatched job was picked up, but each `shell: powershell` step
died with "running scripts is disabled on this system" (PSSecurityException): the runner writes the step to a temp `.ps1` and
dot-sources it, and the execution policy visible to the `gh-runner` account is Restricted (reported by the PM from the log; the
policy scope that wins was not read). Session id, interactive flag, protected-path write test, `nvidia-smi`, `vulkaninfo` and the
CUDA kernel are all **UNMEASURED** in run 1; run 2 (below) measured them.

Consequence beyond the spike: **every `shell: powershell` step of every workflow moved onto this runner hits the same wall**
(section 8a assumes the default shell works). Options, PM decides because it is a security setting:

| | Option | Scope | Caveat |
|---|---|---|---|
| a | policy in `gh-runner`'s own registry hive | that account only | the account has never logged on, so its hive may not exist; needs load/unload of `NTUSER.DAT` |
| b | `LocalMachine` `RemoteSigned` | every account on the machine | widens a machine setting for CireSnave's accounts too |
| c | workflows use `shell: cmd`, or a custom `shell:` template | every workflow, edited | a custom template's temp-file extension under the runner is unverified |
| d | `PSExecutionPolicyPreference=Bypass` in `<runner dir>\.env` (written by the setup script) | that runner's jobs only | unmeasured; a Group Policy `MachinePolicy` would still win |

**Decision (PM, 2026-10-17): d.** The setup script will write `PSExecutionPolicyPreference=Bypass` to the runner's own `.env`
(process scope, that runner only, no machine or hive change). Run 2 proved it (below). The script change was a
separate PR (0.16.20), then the PM re-applied and dispatched the probe again. The PM did not say whether the `.env` came from the
script or by hand, so that path of the script itself is still not proven end to end.

**Execution policy is not a security boundary here.** A job already runs arbitrary commands as `gh-runner`, so Bypass removes no
protection that existed. The protection is the policy of section 1 and 2c: trusted repos only, no fork code, and the ACLs on the lane tools.

**Results, run 2 (2026-10-17, PM; `ciresnave/runner-spike` ref `f5b051e`, run
https://github.com/ciresnave/runner-spike/actions/runs/39048397061; push and dispatch both green), verbatim from the PM's relay of the job log:**

- Execution policy: the `shell: powershell` steps ran, so the `.env` `PSExecutionPolicyPreference=Bypass` took effect for the service.
- Identity: `whoami` = `tuf-laptop\gh-runner`, **session id 0**, **interactive False** (a service with no desktop).
- Protected paths writable? `C:\Projects` False; `C:\Projects\.claude-hooks` False; `C:\Projects\.lane-state` False.
- MSVC step (section 2d): Visual Studio 18 Community flagged incomplete / reboot pending, used via the `-all` warning path;
  `cl.exe` = `...\MSVC\14.51.36231\bin\HostX64\x64\cl.exe`.
- `nvidia-smi -L`: `GPU 0: NVIDIA GeForce RTX 4070 Laptop GPU`; driver 617.42, 8188 MiB (the PM's summary).
- `vulkaninfo`: **both** `NVIDIA GeForce RTX 4070 Laptop GPU` (NVIDIA proprietary) and `AMD Radeon(TM) 610M` (AMD proprietary driver 26.8.1).
- CUDA: `nvcc` = CUDA v13.3; `kernel OK: 0 2 4 6`, from session 0 on the 4070.

Not shown by the relay, so still **UNMEASURED**: the full `nvidia-smi` query lines, the `vulkaninfo` API versions, a CUDA run on the
AMD GPU (not applicable), behaviour after a reboot or logoff, and whether the Visual Studio reboot-pending state matters over time.
The `gpu` label is added by `-Labels gpu` (the script adds no label by default).

## 6. The lint (`crates/runner-lint`)

`runner-lint [FILE...]` reads workflow files (no arguments: `git ls-files '.github/workflows/*.yml'
'.github/workflows/*.yaml'`, and it prints that this enumeration ran) and **exits 1 naming the file and job** for any
job that can run on the runner and breaks the policy. A job "can run on the runner" when its `runs-on` is or contains
`self-hosted` or a label only the runner has (`gpu`), **or when it cannot be shown to be hosted** (an expression,
a `group:`, a label that is not a standard GitHub image such as `windows-gpu`). It fails closed. The common
`runs-on: ${{ matrix.os }}` is **resolved**: every literal value under `strategy.matrix.os` and each `include`
entry must be a hosted image; a matrix that is itself an expression, or a missing key, is not.

For such a job, the workflow's `on:` (read in all three YAML shapes, as YAML 1.2 so the key `on` is a string)
decides:

| Trigger | Result |
|---|---|
| `push`, `schedule`, `workflow_dispatch` only | passes |
| `pull_request` or `pull_request_target` | passes only if the job has **both** the same-repo `if:` and an `environment:` |
| any other trigger (`workflow_call`, `workflow_run`, `issue_comment`, `pull_request_review`, ...) | fails: unverifiable, or reachable from outside |

The same-repo `if:` is `github.event.pull_request.head.repo.full_name == github.repository` (either operand order,
optional `${{ }}`), as the whole condition or one `&&` term. Any `||`, `!` or parenthesis in the condition fails the
job: a condition that can be satisfied another way is not a gate. The lint cannot know the environment has a
reviewer (section 2). A job-level `uses:` of a reusable workflow is not read: it fails under a pull request trigger.
It is **a lint, not a sandbox**: it reads what the file says, not what GitHub will do with it.

`runner-lint --private` lints a private repo's workflows: a pull-request job needs the same-repo `if` but no
`environment:` (section 2b). It refuses (exit 2) unless GitHub says the repo is private.

Distribution: the lint is in this repo's workspace. Running it in another repo's CI needs it published to crates.io
(a PM step), since a git or path dependency is not allowed.

## 7. Procedure (PM, then CireSnave)

1. Review this doc and the script; run the script **without** `-Apply` and read the plan.
2. Elevated PowerShell: `.\scripts\gh-runner-setup.ps1 -Repo <owner>/<throwaway> -Private <owner>/<throwaway>
   -Labels gpu -Apply`; paste the token when prompted. **`-Apply` reads the repo's visibility with `gh api`, and a private
   repo answers 404 to `ciresnave-bot`** (not a collaborator), which throws "cannot read visibility". The PM sets
   `$env:GH_TOKEN = (gh auth token --user ciresnave)` in that elevated shell first (per command, never `gh auth switch`). Read back `gh api repos/<repo>/actions/runners` (labels `self-hosted`,
   `Windows`, `X64`, `gpu`).
3. Run the spike; record section 5.
4. Only then register the first real repo, one `-Apply` each, with section 2a satisfied and the lint already in its CI.
5. Undo: `-Remove -Apply` per repo, then `Remove-LocalUser gh-runner` and delete `C:\gh-runner`.

## 8. Per-repo registration or an organisation runner for ThinkersJournal

A **personal account** (`ciresnave`) has no organisation runners: its runners are per repo, so per-repo is the only
shape there. For `ThinkersJournal`, an organisation can register one runner and share it through a **runner group**
limited to chosen repositories (GitHub docs, read 2026-10-16: a group can be limited to a list of repositories;
public repositories are excluded from a group by default and need an explicit override; a runner registered without
a group lands in the **Default** group).

| | Per repo | Org runner + group |
|---|---|---|
| Boundary | the registration itself: a repo can only use its own runner | the group's repository list, one more setting |
| Failure if misconfigured | none; there is nothing to forget | the **Default** group is the landing place, so an unset group is available to every repo the Default group admits |
| Cost | one runner directory and service per repo, one token per repo | one runner for many repos |
| Token | repo admin | org owner |
| Same procedure as `ciresnave` repos | yes | no |

**Recommendation: per repo**, the same procedure everywhere, because the boundary is then the registration and cannot
be widened by a settings mistake, and ephemeral runners are one-job anyway, so sharing one across repos saves
nothing. Revisit an organisation runner only if `ThinkersJournal` ends up with several repos needing devices and
persistent runners on private repos; then use a **dedicated group, never Default**, public repos left excluded, and
register with an explicit group. The script registers per repo only; an organisation runner is not implemented.

## 8a. Private repos: what each workflow must change (read 2026-10-16 from each repo's default branch)

**These repos are not edited here; their lanes do it.** `RO` = `runs-on: [self-hosted, windows]` (add `gpu` only where
stated). "Fits" is a reading of the file, **not a run**: nothing below has executed on `gh-runner`.

| Repo | Workflow | Job | Today | Change | Fits a plain Windows runner? |
|---|---|---|---|---|---|
| `ciresnave/HumboldtUnifiedKidTracker` | `ci.yml` | backend, frontend, rehearsal-smoke | `[self-hosted, humboldt]` | none while the existing `humboldt` runners (below) keep serving them; if `gh-runner` replaces them, register with `-Labels humboldt` or change to `RO` | **backend needs Docker** (`docker run postgres:16`): Docker access is root-equivalent on the host, which a restricted account must not have. Keep the existing runners for it, or do not move it |
| | `ci.yml` | deny | `ubuntu-latest` | `RO`, and replace `EmbarkStudios/cargo-deny-action` (a container action; the file's own comment says a Windows runner cannot run it) with a step that installs `cargo-deny` | yes once installed in a step |
| | `main-guard.yml` | guard | `ubuntu-latest` | `RO`; steps run `bash .github/scripts/main-guard.sh`: add `shell: bash` and mind the WSL trap; needs `gh` on PATH | probably; `GH_TOKEN` comes from `github.token` |
| `ciresnave/ee-amt` | `ci.yml` | fmt, clippy, docs, msrv, proptest | `ubuntu-latest` | `RO` | yes (Rust via `dtolnay/rust-toolchain`) |
| | `ci.yml` | test | matrix `ubuntu, windows, macos` | matrix `os: [self-hosted-windows]` i.e. one leg on `RO`; **ubuntu and macOS coverage is lost** | the Windows leg yes |
| | `ci.yml` | miri | `ubuntu-latest`, `cargo miri` | `RO` | **unmeasured**: Miri runs on a Windows host but with a smaller supported surface than Linux |
| | `ci.yml` | audit | `ubuntu-latest`, `rustsec/audit-check@v2` | `RO` | unmeasured (a JavaScript action that installs `cargo-audit`) |
| | `bench.yml` | bench | `ubuntu-latest` | `RO` | runs, but timings on a shared laptop are noise |
| | `msrv-check.yml` | msrv-consistency | `ubuntu-latest`, `set -euo pipefail` + awk | `RO` and `shell: bash` (the default shell would be PowerShell) | yes with Git Bash (it bundles awk) |
| `ThinkersJournal/ThinkersFoundation-site` | `ci.yml` | freshness, check, build, test | `ubuntu-latest` | `RO` | yes: Node via `actions/setup-node`; **test runs Playwright**, whose browsers install per job: unmeasured on this account |
| `ThinkersJournal/licence-tools` | `ci.yml` | spdx, fleet | `ubuntu-latest`, `python3 ...` | `RO`; `python3` may not exist on Windows (use `python`/`py`, or confirm `setup-python` provides it) | unmeasured; `fleet` checks out several repos and runs a bash heredoc: add `shell: bash` |
| | `ci.yml` | tests | matrix `ubuntu, windows` x Python 3.11-3.13 | one `os` leg on `RO`; **the ubuntu leg is lost** | the Windows legs yes |

**A runner for Humboldt already exists, and it does not meet this policy.** Read 2026-10-16 from this machine (not
from GitHub, whose runner-list API returns 404 to the lane): `C:\actions-runners\humboldt` and `humboldt-2`, each
configured (`.runner`) for `ciresnave/HumboldtUnifiedKidTracker`, each running as a `Runner.Listener.exe run`
**process owned by `TUF-LAPTOP\cires` (CireSnave's own account)**, started 2026-10-10 08:44, persistent (not
ephemeral), not a service. The label `humboldt` is held by GitHub and was not read. So that repo's jobs already run on
CireSnave's account, which is the opposite of policy rule 6 (and why its `docker` use works). Moving them under
`gh-runner` is a PM/CireSnave decision, and Docker (above) is the obstacle.

None of these needs the GPU, so none gets `gpu`. Each repo's CI should also run `runner-lint --private` (section 6), with
`GH_TOKEN` and `GITHUB_REPOSITORY` set.

## 9. Known limits

- The machine is a laptop: a runner is offline when it sleeps or is off, and a queued job waits (no hosted fallback
  unless the workflow is written with one).
- A job can read anything `gh-runner` can read and use the GPU without limit. Jobs are isolated from each other only
  by the ephemeral default (section 2a) on public repos; on a private repo's persistent runner (the ruled default), `_work` is
  reused and nothing isolates them.
- Nothing in this change has run elevated. The script is verified only to parse, refuse a repo of another owner,
  refuse a persistent runner for a public repo, refuse a `-Private` entry that is not in `-Repo`, refuse without
  elevation, and print the dry-run plan. The `gh api` visibility check in `-Apply` has not run.
