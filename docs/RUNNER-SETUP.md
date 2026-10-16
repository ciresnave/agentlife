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
4. **Ephemeral stays the default**, and it has a cost the PM should decide on: an ephemeral runner takes ONE job,
   so a CI run of N jobs needs N registrations, each with a fresh token. Four repos' normal CI is dozens of jobs a
   day, which no one can hand-register. Options: (a) automate re-registration (needs a token source: a GitHub App or
   a fine-grained PAT, a CireSnave decision); (b) for PRIVATE repos only, a persistent runner (`-Private <repo>
   -Ephemeral:$false`), which this policy permits because no outsider can reach it, at the price that jobs share
   `_work` and a malicious same-repo change persists. **Recommendation: (b) for the private repos now, (a) later.**
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

## 5. Service or interactive: UNMEASURED, and what to measure

The script installs a **Windows service** under `gh-runner`. The question is whether CUDA and Vulkan work from a
service, which runs in session 0 with no desktop.

Measured 2026-10-16 on this box (RTX 4070 Laptop + AMD 610M iGPU, driver 617.42, WDDM, CUDA 13.3 toolkit present),
**interactive session, as CireSnave's account**: `nvidia-smi -L` lists the 4070; `vulkaninfo --summary` lists both
GPUs (NVIDIA Vulkan 1.4.351). That shows only that the machine has working drivers.

**Not measured: session 0.** Registering a non-interactive (`S4U`) task to approximate it returned *Access is denied*
without elevation, so a lane cannot take that read. The spike (section 5a) takes it as `gh-runner` in service mode.

Decision rule: if the spike prints both GPUs from the service, keep the service (it survives logoff and reboot). If
Vulkan or CUDA fails there, the alternative is an interactive runner (`run.cmd`) in a logged-on session of the
runner account: that needs the account to hold a desktop session next to CireSnave's, which is why it is the
fallback, and it is **not implemented here**.

### 5a. Spike workflow (not written yet)

One private, throwaway repo (the PM creates it). `on: workflow_dispatch` only, `runs-on: [self-hosted, windows,
gpu]`, steps: `whoami`, session id, `nvidia-smi`, `vulkaninfo --summary`, and a CUDA call (`nvcc --version` is not
enough: run a tiny kernel or `nvidia-smi -q`). Its result goes in this section with the date.

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
   -Apply`; paste the token when prompted. Read back `gh api repos/<repo>/actions/runners` (labels `self-hosted`,
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
  by the ephemeral default (section 2a); with `-Ephemeral:$false`, `_work` is reused and nothing isolates them.
- Nothing in this change has run elevated. The script is verified only to parse, refuse a repo of another owner,
  refuse a persistent runner for a public repo, refuse a `-Private` entry that is not in `-Repo`, refuse without
  elevation, and print the dry-run plan. The `gh api` visibility check in `-Apply` has not run.
