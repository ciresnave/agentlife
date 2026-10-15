# `agentlife launcher`: the always-on launch-on-message service (design)

**Status: design, nothing built.** Written 2026-10-15 at `origin/main` 0.15.2 (0ad1a91).
Inputs read by SHA: Synapse `docs/superpowers/specs/2026-10-15-cutover-and-lane-launch.md` (#172 @006c9a9,
revised to cf0c5e5 by its lane) and OverMind `docs/LANE-LAUNCH-POLICY.md` (#145 @e4c6ae3). Where those two
disagree with each other or with this repo, this file says so.

## What it is

One Rust binary mode, `agentlife launcher`, run under Task Scheduler (Windows; the systemd `--user` unit is
print-only, as `install-task` is today) at logon and restarted on exit. It is **not a lane session**: lanes
restart at every task boundary, and the role that is down may be the one that would have launched it
(Synapse spec C2a).

Loop, per OverMind's policy section 2: long-poll `GET /v1/waiting?wait=30` as role `overmind`; for each
signal apply rules 1-7 in order; call the mechanism once; `POST /v1/launch-result`
`{signal_id, attempt, role, outcome, reason}`; once per cycle read `GET /v1/list` for the online
observation (R7) and the `transient` re-checks (`launch-clear`).

## Parts, and who owns each

| Part | Owner | State at 0.15.2 |
|---|---|---|
| Poll, decision rules 1-6, store, R7 alarm, results | AgentLife (this service) | not built |
| Synapse access and the bearer session | `synapsed-client` | **not usable: see Blockers** |
| Mechanism: start one lane by role, `--model sonnet`, reports one of 8 results | AgentLife | `lane-restart start` (0.15.0-0.15.2) covers the spec/duplicate path; `start-role` on the launcher path (memory floor, `restore.lock`) not built |
| Consent: standing grant for rule 7 | AgentLife (M5) + CireSnave ruling | **unbuilt; board 216** |
| Policy numbers (caps 3/12 per hour, no-claim 3, replay 5 min) | OverMind | proposals, unmeasured |

## Identity

At startup the service writes `<state>/overmind.launcher-caller` (`{role:"overmind", pid, pid_start_secs}`) and
removes it on a clean exit; `lane-restart start` accepts it only from that process's direct child (0.15.2).
The trust root is the Synapse credential, not this file: the service must write the record **only while it
holds a valid session**, and remove it on a 401/403 (rule 5). The file is same-user-forgeable
(`docs/LAUNCH-ON-MESSAGE-THREAT.md`).

## Store

`<state>/launcher-store.json`, written atomically (temp file, flush, rename), schema version field first.
Two tables exactly as OverMind section 4: request rows keyed by `(signal_id, attempt, epoch)` and role rows.
Fail closed (rule 1) on unreadable, newer-schema or unwritable; **never recreate on its own** (an operator
runs `agentlife launcher --init-store`). Pruned at 24 h. Reads and writes happen under one in-process lock;
two service instances are refused by a lock file naming pid and start time (the `restore.lock` pattern).

## Mapping the mechanism to Synapse (OverMind section 3)

The mechanism answers in the `--json` vocabulary of `lane-restart start` (0.15.1) today:
`started/awaiting-dialog`, `already-running`, `refused`, `failed`. OverMind's section 3 also wants
`held-for-memory`, `lock-held`, `invalid-argument`. Those exist only on `agentlife restore`
(`Outcome::NotStarted("held for memory")`, `restore.lock`), so the service needs `start-role` (one lane, same
checks, `--json` with the eight results OverMind named: Working, Started, AwaitingDialog, AlreadyRunning,
HeldForMemory, LockHeld, Failed, InvalidArgument) before section 3 can map without parsing text.

## Blockers (each is a decision or a build that is not this service's)

1. **`synapsed-client` is not on crates.io.** `crates.io/crates/synapsed-client` is `0.0.0`, "Name reserved
   for the Synapse project; no code yet" (checked 2026-10-15; owner `ciresnave`). The crate exists in the
   Synapse repo, and the rule is crates.io versions only, no path or git deps (CIRESNAVE-EXPECTATIONS 6.8). The
   Synapse lane told this lane not to hand-roll the signed claim. Needs: Synapse publishes it, or the PM
   rules otherwise.
2. **No standing grant (M5) and no ruling (board 216).** Without them rule 7 answers `refused: no-grant` for
   every signal: the service is a notifier (PM notice to the portfolio PM through AgentLife's notify path)
   and a person starts the lane. That is a valid first deliverable and should ship alone.
3. **`start-role`** (above). Until it exists the service could only call `lane-restart start`, which skips the
   memory floor and `restore.lock`. Not recommended.
4. **`epoch` in the signal** (OverMind asked Synapse; unbuilt). Until then the 5-minute window separates a
   replay from a new request, as OverMind's section 1 says.

## `launch_wait`: measured, not guessed

Not measured: `restart.log` held 4 real starts and recorded no duration. 0.15.1 adds `launch-to-live:<n>s`
(spawn to confirmed progress) to the ledger and to `start --json`. That is the *start-to-live* half; the
*claim* half needs the lane to claim its Synapse role, which exists only after cutover step 2. The service
will log, per role, `launched_at` and the first `online` observation, which gives start-to-claim directly once
a lane claims. Report when N >= 5; ship a provisional value marked UNMEASURED until then (PM ruling).

## Build order

1. Notifier-only service: poll, rules 1-6 and R7, store, results, `refused: no-grant`, notify. Needs blocker 1
   only. Tests (a)-(u) of OverMind section 5 against a stand-in daemon and a stand-in mechanism, each seen red.
2. `start-role --json` on the launcher path.
3. Grant read (M5) and rule 7, after board 216.

## Out of scope

Choosing whether a lane should be launched (OverMind), the mailbox and signal (Synapse), a terminal emulator
or tmux (Q1: direct process first), and any launch under Opus or with `--resume`.
