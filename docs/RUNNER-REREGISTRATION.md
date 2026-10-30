# Re-registering ephemeral runners without a human: what exists, what is missing, what the PM decides

Written 2026-10-30. PM RULING 2026-10-30: design the re-registration automation as a ROADMAP row (GitHub App token
source per CireSnave's 2026-10-22 lean); note first, no code. **Read, not run:** every fact below about AntSpawn is read
from `ThinkersJournal/antspawn` at `origin/main` = `ecb6f76` (0.0.46: `docs/ADAPTER-GITHUB.md`, `docs/MEASUREMENTS.md`,
`ROADMAP.md`, `HANDOFF.md`); every fact about this repo from `origin/main` here. Nothing was run for this note.

## The problem (RUNNER-SETUP 2a, board 339)

A public repo's runner must be ephemeral: one job, then it unregisters and its service stops. Each registration
needs a fresh one-hour token from the PM and an elevated `-Apply` with CireSnave at the keyboard. A device job on push
or on a schedule (cpufish's AOCL tests) therefore waits on a person every time. The PM's interim recommendation
(board 339): run AOCL by hand until re-registration is automated.

## This is not a new component: it is AntSpawn's GitHub adapter

AntSpawn (spec `C:\Projects\ANTSPAWN-SPEC.md`) was designed for exactly this: *"GitHub Actions just-in-time runner
(`generate-jitconfig`, one job, runner exits, environment destroyed)"*, credential = a GitHub App (board 241, CireSnave
2026-10-22: *"Lets go with the GitHub app."*). A watcher sees a queued job, mints a **JIT runner config** with the
App's installation token, and hands it to a launcher that starts the runner in a fresh environment and destroys it
afterwards. A JIT config needs no registration token, no `config.cmd`, no Windows service and no stored password, so
it removes the per-job paste **and** the shared-account problems of `gh-runner-setup.ps1` (RUNNER-ACCOUNTS.md) for the
jobs it serves.

| Piece | State (antspawn `ROADMAP.md` / `MEASUREMENTS.md`) |
|---|---|
| GitHub App `antspawn-runner` (id 5441608), org install on ThinkersJournal; key DPAPI-protected under the watcher account, outside every sandbox (agentlife's `github-app-setup.ps1`) | live; permissions read back 2026-10-23: `actions=read`, `metadata=read`, `organization_self_hosted_runners=write` |
| JWT, installation token, `generate-jitconfig`, list/delete runners | verified once against the live App: minted in 1763 ms, listed, removed; **no job ran** |
| `JobSource` (list queued runs/jobs) | read live: `0 queued job(s)`; zero jobs cannot tell "works" from "sees nothing" (their words) |
| Watcher loop (pure logic, mutation-checked), backoff on mint failure | done (row 7) |
| Hand-off of the JIT config by anonymous pipe to `spawn-jit-shim` | host end done (row 8, #8/#36/#38); the guest listener done (#43-#45) |
| A real run against `antspawn-proof`, and a full runner job in 3 consecutive VMs | **open (row 10)**: no job has run end to end |
| Self-hosted runner for AntSpawn's own `windows_sandbox`/`account` tests | held (row 11, board 321) |

So the automation is largely built and **unproven end to end**. The honest status for cpufish today: no automated path
has run a single GitHub job.

## What is missing for cpufish's AOCL job (decisions, not code)

1. **Org runner group vs the PM's ruling.** AntSpawn mints through an **org-level runner group** (`POST /orgs/{org}/
   actions/runners/generate-jitconfig`, group `visibility = selected`, repo list checked equal before every mint, public
   repos need the group's public-repository switch on). The PM's 2026-10-30 ruling for the *persistent per-repo* script was
   "no org runner group now" (RUNNER-SETUP 8). These are different mechanisms (an ephemeral JIT runner in a group
   restricted to named repos, versus a shared persistent runner in a group), but it is the PM's to say whether the
   ruling covers the JIT group. It also leaves out `ciresnave/*` repos: an org group serves only `ThinkersJournal` repos. A repo-level
   `generate-jitconfig` exists too and needs `administration: write` (the `-RepoAdminFallback` of `github-app-setup.ps1`);
   not requested today.
2. **Which AntSpawn launcher runs cpufish's AOCL job.** `account` (a pre-provisioned restricted account, ACL denying
   `C:\Projects`, wiped work dir, kill-on-close: certified 5/5 on this machine, host CPU and host files visible) or
   `windows-sandbox` (fresh VM per job; one VM at a time; mapped read-only folders). AOCL lives in
   `C:\Program Files\AMD\AOCL-Windows` on the host. Under `account` the job reads it where it is; under the sandbox it must be
   mapped read-only, and whether AOCL's CPU dispatch sees the 7940HX's features inside the sandbox is **unmeasured**. The
   `account` launcher's account must be one of RUNNER-ACCOUNTS.md's per-runner accounts, with no group by default.
3. **The 5.1h conditions still apply to the workflow** (same-repo gate, no secrets, lint); JIT does not change them. A
   JIT runner serves whichever queued job the watcher routes it to, so the routing key (the group's labels, `runs-on:
   [self-hosted, antspawn, <launcher>]`) is what keeps a fork PR's job away; the workflow lint must check that label.
4. **Who starts the watcher.** A process under the watcher account (the only one that decrypts the key) that runs
   whenever a job may queue: a logon task or service. **[inferred, not read]** Starting it is an elevated, one-time step for
   CireSnave, like the runner services were; AntSpawn's `HANDOFF.md` lists `schtasks /Run` by a non-admin as held for elevation (row 12). The laptop sleeping means the job waits, as with any runner (RUNNER-SETUP 9).
5. **App permission already covers org-level JIT**; `Actions: read` was granted (board 264). No new permission is
   needed unless item 1 chooses repo-level JIT.

## ROADMAP row

Row 9 in `docs/ROADMAP.md`: re-registration = AntSpawn's GitHub adapter; agentlife's part is (a) this decision list to
the PM, (b) once decided, the host-side pieces that are agentlife's: the watcher account/task (extending
`github-app-setup.ps1` / a `gh-runner-setup.ps1` mode that creates the launcher account and its ACL, RUNNER-ACCOUNTS.md),
and (c) the workflow lint rule for the routing label. The end-to-end proof (AntSpawn row 10) is the antspawn lane's; until it
passes, nobody should plan around it.

## Rulings (PM, 2026-10-30), answering the three questions this note asked

1. **Org group:** the PM's "no org runner group now" meant a *shared persistent runner pool*. AntSpawn's ephemeral JIT via
   the board-241 App over an org group limited to named repos (list checked equal before every mint) is a different
   mechanism, already decided, and allowed for `ThinkersJournal/*` repos. Repo-level JIT for `ciresnave/*` needs the App to
   gain `administration: write`: **not now**; it goes to CireSnave if a `ciresnave` repo needs it (none does today).
2. **Launcher:** `account`, with a row-8 per-runner account (AOCL is on the host; `windows-sandbox` CPU dispatch is
   unmeasured).
3. **Proof first:** the antspawn lane is asked for the date of its row-10 end-to-end proof (asked 2026-10-30 over Synapse);
   **cpufish writes no self-hosted job until that proof exists.**
