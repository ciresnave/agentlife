# Durable lane timers (design, no code for the timer itself)

Status: DESIGN. Written 2026-11-19 (agentlife lane) for the PM's TASK `durable-lane-timers` (CireSnave asked: can a
timer outlive a soft stop and wake the lane by a Synapse message?). Nothing here was run except what is marked MEASURED.
Revised the same day around the pieces that already exist (PM notice `durable-timers-use-existing-pieces`).

## The question

A `CronCreate` timer lives in the lane's own session and dies with it, so a lane that restarts at a task boundary loses it.
The lane needs a wake-up that outlives the session and brings the lane back by mail.

## What already exists (read at origin/main d0c36c7 and live on 2026-11-19)

| Piece | What it does | Gap |
|---|---|---|
| `WAIT: <event> \| check: <cmd> \| wake: <RFC3339 UTC>` lines in a lane's HANDOFF | the lane's own record of what it waits for; `lane-restart waits` prints each, `assert-idle` refuses while one is unarmed | none |
| `lane-restart waits-poll [--dry-run]` (`crates/lane-restart/src/app/waits_poll_cmd.rs`) | one supervisor pass: for each role it reads the HANDOFF and, when a wait is due, sends `[NOTICE] wait-due ...` once per (role, event, wake) through `notify_transport`; allowlisted checks only | reads only roles with `state == Running && activity.is_some() && spec Complete && role != pm`, so **a soft-stopped lane is never read** |
| `stop --self` records `stop_kind = soft` (signed) | a soft-stopped lane may be started by the launcher when mail waits | a hard or unsigned stop is refused, by design |
| `agentlife-launcher` task (registered, At logon) | polls Synapse `/v1/waiting`, starts the lane with `lane-restart start --signal-id` | see the two blockers below |

Nothing scheduled runs `waits-poll` today: the only caller is `tools/lane-pulse.sh` line 33, dry by default (`WAITS_POLL`),
run by the PM's oversight tick. `schtasks` shows only `agentlife-launcher`, `synapsed-cires` and `synapse-provision-weekly`
among ours (2026-11-19).

## Proposal: the timer is a scheduled `waits-poll` that also covers soft-stopped lanes

1. **Extend `waits-poll`'s subjects** to roles whose spec is `suspended` with `stop_kind = soft` (HANDOFF path from the
   spec's cwd, which is where `status_all` already finds it). Everything else in the pass is unchanged: same allowlisted
   checks, same once-only ledger, same notice text.
2. **Send on the Synapse transport** (`notify_transport = synapse`, in `~/.overmind/lane-restart.json`; it is NOT set today,
   so notices go over the claude-peers broker, which only reaches live sessions). The notice is queued in
   `<state>/notify-outbox` and relayed by the launcher through the daemon.
3. **Idempotency:** the message id becomes a hash of `role + event + wake` (today it is `lane-restart-<ms>-<pid>-<n>`),
   so a pass that crashed between send and ledger write repeats a duplicate at the daemon, not a second message. The
   ledger stays (500 keys).
4. **The "timer":** one scheduled task that runs `lane-restart waits-poll` (not dry) every 5 minutes as the interactive
   user (`schtasks /Create /SC MINUTE /MO 5`). No new service. Cost per pass: one spec read and one HANDOFF read per
   soft-stopped lane plus allowlisted `gh`/`git` checks, each with a 30 s kill. Registering it is a persistent change:
   the PM's or CireSnave's decision, not a lane's.
5. **Where timers are recorded:** the lane's own HANDOFF `WAIT:` line is the record. `lane-restart waits --role <r>` lists
   them and the lane deletes the line to cancel. No second file, so a restart cannot lose one and nothing can disagree.
6. **Cancel and idempotency:** deleting the line cancels (the next pass does not see it). A wait already notified stays in
   the ledger, so re-adding the same (event, wake) is silent: change the wake time to re-fire.

### What happens in each state of the lane when a wait is due

| Lane | Result |
|---|---|
| live | the notice arrives as a channel event; the lane acks it (today's `waits-poll` path) |
| soft-stopped | the notice queues; the daemon raises a launch signal; the launcher starts the lane (MEASURED N=1, below); the lane reads its inbox first |
| parked or hard-stopped | stays closed on purpose; the notice queues unread. Correct |
| role never claimed | the daemon does not know it, so the mail cannot queue or wake; `lane-new`/`lane-provision` now claim it (this branch) |

## Two blockers to the wake itself

**B1: the sender is not allowed to wake.** The daemon raises a launch signal only for mail from `SYNAPSE_LAUNCH_SENDERS`.
The daemon task cmd (`synapse-pilot\task\synapsed-run-20261112T140213301.cmd` line 7) sets `pm@ciresnave` only. A notice
relayed by the launcher comes from `agentlife-launcher@ciresnave`, so it would queue but not wake. The environment wins over
`[launch] senders` in `synapsed.toml`. Options: (A, recommended) add `agentlife-launcher@ciresnave` to the list (a daemon
setting and a daemon restart: CireSnave's decision per `LAUNCHER-SERVICE.md`); (B) make the PM's pulse the sender. Not (C): a
scheduled task must not claim the PM role.

**B2: the launcher cannot see the lane come up.** MEASURED 2026-11-19: `agentlife-launcher@ciresnave` holds `see` on only
pm, pm-kernels/ants/platform/web, onemkl, analysant, thinkersjournal-com, epistemologicant, ontologicant and semanticant, so
its `/v1/list` hides every other lane and the launcher's own stall alarm `launch-stalled: <role> started-never-claimed`
fires 4 minutes after every successful wake (kiss: spawn 15:49:39Z, alarm 15:53:36Z; the rule is 2 x launch_wait = 240 s).
It is a false alarm, caused by a missing grant. Fix: ONE `see` grant for the launcher over every wakeable lane (one grant
with many objects: each separate grant grew `access.json` by about 35 KB and the PM's batch of 2026-11-19 slowed the daemon
to 15 s per probe), issued by the PM or the account holder.

## Wake-path requirements (from the kiss measurement, 2026-11-19)

MEASURED, N=1 (one lane, kiss; one sender, pm; one try): soft stop 15:48:02Z, PM notice 15:48:49Z, launcher start
15:49:39.84Z (50 s after the send), launch-to-live 7 s (ledger `restart.log` line 3010), lane acted 15:49:57Z. No dialog
blocked it. This closes "a wake works" for that case only; it is not a general result.

1. **The woken lane must know why.** The start prompt was the generic "read kiss HANDOFF and continue, and arm every WAIT";
   the notice reached the session later, as a channel event, after the HANDOFF read and several tool calls, and the lane
   concluded it was not woken by mail. **Built on this branch:** a lane the launcher starts for a mail signal gets "You were
   started because mail arrived for you (signal <id>). Fetch your inbox FIRST, ack and act on that message, then read
   <role> HANDOFF and arm every WAIT in it"; a start by a person or a restart keeps the generic prompt (tests in
   `start.rs`). **Limit:** the launcher knows only the signal id (`<role>:<enqueue time of the oldest waiting message>`),
   not the sender, the message id or the text, so the prompt cannot name them.
2. **The woken lane acks promptly.** The notice stayed leased until the lane acted on the late event. With requirement 1 the
   first act is the fetch, then the ack.
3. **No fresh claim happened for kiss** (epoch 7 before the stop and after). `synapse claim` caches the session per home
   ("one holder per (home, role)"), so a lane restarted in the same home may reuse it; the effect on when the notice is
   pushed is UNVERIFIED.
4. **The launcher's log must say what it did.** Built: each posted result is logged as `cycle: role=<r>
   outcome=<started|already-running|refused:<why>|failed:<why>>` (before, `decided=1 posted=1` named neither).

## Scratch test plan, on the `launchtest` role first (nothing runs on a real lane)

Precondition: B1 and B2 settled for `launchtest` only (its own `see` grant for the launcher; the sender allowed).

| # | Step | Read-back |
|---|---|---|
| 1 | `launchtest` writes a HANDOFF with `WAIT: scratch | check: false | wake: <now + 6 min>` and soft-stops itself | `restart.log` shows `soft stop; launch spec suspended`; spec `stop_kind: soft` |
| 2 | a scheduled `waits-poll` pass (the extended one, once, by hand) runs after the wake time | one `[NOTICE] wait-due` queued for `launchtest` with message id = hash(role+event+wake); a second pass queues nothing (ledger) |
| 3 | the launcher wakes it | `restart.log` kind start, reason `wake on mail`, launch-to-live; `launcher.log` has `cycle: role=launchtest outcome=started` |
| 4 | the lane's first act | its first tool call is the inbox fetch; the notice acked within 10 s of live; first-sight time recorded |
| 5 | epoch | `synapse list` epoch before and after; record whether it moved |
| 6 | no false alarm | with the launcher's `see` grant present, no `started-never-claimed` at +4 min; repeat once WITHOUT the grant to prove the alarm follows the grant |
| 7 | repeat 5 times | N=5 per sender; report seconds from the notice to live and to ack for each |

Pass: the lane is woken once per (event, wake), reads the notice first, acks it, raises no alarm. Anything less is reported
with the lines, not retried silently.

## Not decided here

`SYNAPSE_LAUNCH_SENDERS` widening (CireSnave); registering the scheduled `waits-poll` task (PM/CireSnave); whether a lane
that stopped hard and has a due wait should be told to the PM instead (suggested: yes, one line per pass, so a forgotten
wait is not silent).
