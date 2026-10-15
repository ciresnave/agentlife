# `agentlife launcher`: the always-on launch-on-message service (design)

**Status: design, nothing built.** Written 2026-10-15 at `origin/main` 0.15.2 (0ad1a91).
Inputs read by SHA: Synapse `docs/superpowers/specs/2026-10-15-cutover-and-lane-launch.md` (#172 @006c9a9,
revised to cf0c5e5 by its lane) and OverMind `docs/LANE-LAUNCH-POLICY.md` (#145 @e4c6ae3). Where those two
disagree with each other or with this repo, this file says so.

## What it is

One Rust binary mode, `agentlife launcher`, run under its **own** Task Scheduler task (Windows; the systemd
`--user` unit is print-only, as `install-task` is today). The existing logon task runs `restore --from-logon`
and has no restart setting, so it cannot host this: the build adds a second task, `agentlife-launcher`, whose
action is `agentlife launcher`, trigger logon, settings restart-on-failure every minute (no attempt limit, no
execution time limit, a second instance is ignored). `install-task` registers and removes both. It is **not a
lane session**: lanes restart at every task boundary, and the role that is down may be the one that would have
launched it (Synapse spec C2a).

**Relation to Revision 3.** `DESIGN-REVISION-3.md` section 3 already specifies `agentlife watch` (poll
`/v1/list`, wake `lazy` agents under a grant). This service is that process with a better signal: Synapse's
`/v1/waiting` (level-triggered, with `signal_id`/`attempt`) instead of diffing `pending` counts. One process,
not two; the name `watch` or `launcher` is a naming choice for the PM.

Loop, per OverMind's policy section 2: long-poll `GET /v1/waiting?wait=30` as role `agentlife-launcher` (a distinct role in Synapse's `launcher_roles`; never `overmind`, which is the OverMind lane's and holds one session); for each
signal apply rules 1-7 in order; call the mechanism once; `POST /v1/launch-result`
`{signal_id, attempt, role, outcome, reason}`; once per cycle read `GET /v1/list` for the online
observation (R7) and the `transient` re-checks (`launch-clear`).

## Intent: a parked or closed lane is never woken

OverMind's rule 3 is "known lane" only. Revision 2/3 intent still binds (*"any lane that is intentionally
closed should remain closed unless intentionally reopened"*, CireSnave, quoted in `DESIGN-REVISION-2.md`).
The service adds a rule between OverMind's 3 and 4, from the **registry**: a role whose intent is `closed`
(`parked` or `exited`) is `refused: closed-on-purpose`, terminal for that signal, never launched; only `wanted`
(not running) and `lazy` are launchable. Unverified and to be settled before build: whether a lane stopped by
`lane-restart stop` (spec `suspended`) is recorded in the registry as `lazy`, `wanted` or `closed`; today
`lane-restart` writes only the spec, so the mapping has no evidence behind it. Until it does, a role with no
registry intent is `refused: unknown-role`, which is the safe side.

## Parts, and who owns each

| Part | Owner | State at 0.15.2 |
|---|---|---|
| Poll, decision rules 1-6, store, R7 alarm, results | AgentLife (this service) | not built |
| Synapse access and the bearer session | `synapsed-client` | **not usable: see Blockers** |
| Mechanism: start one lane by role, `--model sonnet`, reports one of 8 results | AgentLife | `lane-restart start` (0.15.0-0.15.2) covers the spec/duplicate path; `start-role` on the launcher path (memory floor, `restore.lock`) not built |
| Consent: standing grant for rule 7 | AgentLife (M5) + CireSnave ruling | **unbuilt; board 216** |
| Policy numbers (caps 3/12 per hour, no-claim 3, replay 5 min) | OverMind | proposals, unmeasured |

## Identity

After taking the instance lock and loading the store, the service writes `<state>/agentlife-launcher.launcher-caller`
(`{role:"agentlife-launcher", pid, pid_start_secs}`); on exit it removes the file **only if it still names its own pid
and start time**. A second instance never gets that far (it fails the lock first). `lane-restart start`
accepts the record only from that process's direct child (0.15.2).
The trust root is the Synapse credential, not this file: the service must write the record **only while it
holds a valid session**, and remove it on a 401/403 (rule 5). The file is same-user-forgeable
(`docs/LAUNCH-ON-MESSAGE-THREAT.md`).

## Store

`<state>/launcher-store.json`, written atomically (temp file, flush, rename), schema version field first.
Two tables as OverMind section 4: request rows and role rows. Fail closed (rule 1) on unreadable,
newer-schema or unwritable; **never recreate on its own** (an operator runs `agentlife launcher
--init-store`). Pruned at 24 h.

**Request key and generations.** Rows are keyed `(signal_id, attempt, epoch, generation)`. `epoch` is
Synapse's counter once it exists, else a constant. `generation` starts at 0 and is incremented when the same
`(signal_id, attempt, epoch)` arrives again **after** the 5-minute replay window while its row is still
retained: the older generation stays (it still counts toward the hour caps) and the new one is evaluated
afresh. So an epochless repeat never overwrites or collides with the earlier result, and a change of `epoch`
is a new request, never a replay. `LAUNCH-ON-MESSAGE-THREAT.md` uses the same key.

**Crash safety: write intent first, then act, then record, then answer.** Each request row moves through
`intent` -> `called` -> `answered`, each transition an atomic write **before** the next step:

1. `intent`: written before the mechanism is called, with the role and the time. It counts as a spawn
   toward rules 5 and 6 from this moment (conservative: a crash never under-counts a launch).
2. The mechanism is called once. Its result is written to the row (`called`, with the mapped outcome and
   reason) **before** the POST to Synapse.
3. `POST /v1/launch-result`; only on a 2xx does the row become `answered`. A row that is `called` but not
   `answered` is the **outbox**: it is re-posted each poll cycle until it succeeds. Whether the POST is
   idempotent for the same `(signal_id, attempt)` is not stated in the Synapse spec: **asked of the Synapse
   lane**; the service treats a 409/duplicate reply as success.

On restart a row left in `intent` (crash between steps 1 and 2) is resolved by reading the process table:
`role_status` shows the role running -> record `started`/`already-running` from the observation and move on;
not running -> record `failed: spawn-failed`, counted, and let Synapse re-signal. The mechanism is **never
called again for a row that reached `intent`**. This can lose one launch (the lane did not start and Synapse
re-signals with `attempt+1`, which is a new key) but cannot double one.

**One instance.** The service holds an OS advisory lock on `<state>/launcher.lock` for its lifetime
(`File::try_lock`, stable since Rust 1.89), taken with a create-or-open of the same path, so there is no
stale-file reclaim and nothing to race: the OS releases it on a crash. The file's pid/start-time text is
informational only. A second instance fails to lock and exits, **before** it touches the caller record or the
store.

## Two starters at once

`plan_start` reads the process table and `execute_start` spawns later, with no lock in between: a launcher
request racing a person's `lane-restart start`, or two launches of one role, can both pass the check
(`duplicate::check` narrows it, but a session that has not yet written a state file is invisible to both).
The instance lock above does not cover a human or another caller. **Required of the mechanism before the
service launches unattended** (a `start`/`start-role` change, its own PR): a per-role start lock
`<state>/<role>.start.lock` (OS advisory lock, held across check, spawn and the confirmed-live wait), and a
re-check of `role_status` and `duplicate::check` after the lock is taken and immediately before the spawn.
The second starter then gets `already-running` (exit 3), which is the contract it already has. Until that
lands, notifier-only mode (rule 7 refuses) is the only safe mode, which is the build order below anyway.

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

Not measured: `restart.log` held 4 real starts and recorded no duration. Two biases to state, both found in
review of this design:

- **`launch-to-live` (0.15.1) is an upper bound, not spawn-to-progress.** `main.rs` starts the timer before
  `execute_start` and reads it after the spec write-back returns, so it includes pre-spawn setup and the
  `running` spec write, and it is truncated to whole seconds. Both are small against a 120 s value but they
  are not zero. A follow-up code PR makes `execute_start` return the elapsed time around
  `launch_and_wait_with` alone, and `--json`/ledger report that. Until then the figure is labelled an upper
  bound.
- **The first `online` observation is not the claim.** Observing `online` at the next 30 s poll measures when
  the service noticed, so it overstates start-to-claim by up to one poll interval. The claim time itself
  must come from Synapse: **requested of the Synapse lane**, a per-role `claimed_at` (session start) in
  `/v1/list`, or in the signal. `last_seen` is a heartbeat and moves, so it cannot stand in. Until it
  exists the service records `observed_online_at`, reports it as an upper bound with the poll interval as the
  error, and never as a measured claim time.

Report when N >= 5 real launches; ship a provisional value marked UNMEASURED until then (PM ruling).

## Build order

1. Notifier-only service: poll, rules 1-6 and R7, store, results, `refused: no-grant`, notify. Needs blocker 1
   only. Tests (a)-(u) of OverMind section 5 against a stand-in daemon and a stand-in mechanism, each seen red.
2. `start-role --json` on the launcher path.
3. Grant read (M5) and rule 7, after board 216.

## Out of scope

Choosing whether a lane should be launched (OverMind), the mailbox and signal (Synapse), a terminal emulator
or tmux (Q1: direct process first), and any launch under Opus or with `--resume`.
