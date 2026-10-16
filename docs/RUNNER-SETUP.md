# Self-hosted GitHub runners (board 221)

CireSnave, 2026-10-16, verbatim: *"I think we should set up local runners.  I agree with all of your conditions on
them.  Lets get that going."* GitHub's docs say self-hosted runner usage is free (read by the PM 2026-10-16).

## 1. Conditions (agreed)

1. **Triggers only `push` to `main`, `schedule`, `workflow_dispatch`. Never untrusted PR code.**
2. **Trusted repos only**, first `ciresnave/fuel`, `ciresnave/baracuda`, `ciresnave/lightbulb`.
3. **A restricted Windows account, not CireSnave's**, and no secrets beyond what a job needs.

A self-hosted runner executes whatever the repo's workflows say, as the runner's account, on the real machine and
GPU. There is no sandbox. So the three conditions are the whole security model, and each has a mechanism below.

## 2. What enforces each condition

| Condition | Mechanism | Where |
|---|---|---|
| trusted repos only | `-TrustedRepos` list; `-Repo` outside it is refused before anything changes | `scripts/gh-runner-setup.ps1` |
| restricted account | local account `gh-runner`: standard user, refused if in Administrators or Remote Desktop Users; root directory ACL is SYSTEM, Administrators, that account | script |
| password never at rest | generated in memory (32 chars, rejection-sampled), given to `New-LocalUser` and to `config.cmd`, never written, printed or logged; an existing account is rotated the same way | script |
| no untrusted triggers | a lint that fails any job with `runs-on: self-hosted` in a workflow with a `pull_request` or `pull_request_target` trigger | section 6, **not built yet** |
| no secrets beyond need | the account has no gh login, no `~\.cargo` credentials, no ssh keys; a job gets only repo secrets it names. Never add an org-wide or CireSnave token. | operator rule |

Repo settings the PM sets on each trusted repo (cannot be set from here): Actions > General > *Fork pull request
workflows*: require approval for all outside collaborators; workflow permissions: read-only by default.
**A public repo is the dangerous case**: anyone can open a PR, so for a public trusted repo the lint and "no
`pull_request` job on a self-hosted label" are the only thing between a stranger and the machine.

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

One private, throwaway repo. `on: workflow_dispatch` only, `runs-on: [self-hosted, windows, gpu]`, steps: `whoami`,
session id, `nvidia-smi`, `vulkaninfo --summary`, and a CUDA call (`nvcc --version` is not enough: run a tiny kernel
or `nvidia-smi -q`). Its result goes in this section with the date.

## 6. The lint (not written yet)

Fails, with the file and job named, any workflow where a job's `runs-on` is or contains `self-hosted`, **or a label
that only a self-hosted runner has (`gpu`)**, and the workflow's `on:` includes `pull_request` or
`pull_request_target`. It must read `on:` in all three YAML shapes (string, list, map), and `workflow_call`
workflows are reported as unverifiable rather than passed. It runs in the CI of every repo that gets a runner.

## 7. Procedure (PM, then CireSnave)

1. Review this doc and the script; run the script **without** `-Apply` and read the plan.
2. Elevated PowerShell: `.\scripts\gh-runner-setup.ps1 -Repo ciresnave/<throwaway> -Apply`; paste the token when
   prompted. Read back `gh api repos/<repo>/actions/runners` (labels `self-hosted`, `Windows`, `X64`, `gpu`).
3. Run the spike; record section 5.
4. Only then register the trusted repos, one `-Apply` each, with the lint already in their CI.
5. Undo: `-Remove -Apply` per repo, then `Remove-LocalUser gh-runner` and delete `C:\gh-runner`.

## 8. Known limits

- The machine is a laptop: a runner is offline when it sleeps or is off, and a queued job waits (no hosted fallback
  unless the workflow is written with one).
- A job can read anything `gh-runner` can read and use the GPU without limit; nothing isolates two jobs from each
  other (`_work` is reused, no ephemeral runner). Ephemeral (`--ephemeral`) runners are a later hardening.
- Nothing in this change has run elevated. The script is verified only to parse, refuse an untrusted repo, refuse
  without elevation, and print the dry-run plan.
