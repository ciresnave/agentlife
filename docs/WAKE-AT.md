# Wake-at registry (design; PM TASK `scheduled-wake-outside-the-session`, 2026-11-23)

Status: BUILT 2026-11-23 on branch feat/o19-roster-epoch (unmerged), unit-tested with a fake clock; NOT run on a real lane. Nothing here is MEASURED unless marked. Built as designed, except: `WAKES_PER_CYCLE` and the 24 h expiry live in `crates/launcher/src/lib.rs`; a wake refused for a retryable reason leaves no engine row (so no `launch-clear` is ever sent for a wake) and is not logged each cycle (`lane-restart wakes` shows it); the wake step runs after the online read so a lane just seen online has its rule-5 counter reset first.

## The question

A lane that only waits soft-stops. Its `WAIT: ... | wake: <time>` line is read only by a STARTING lane (`lane-restart waits`),
and a session timer dies with the session, so today only mail wakes a stopped lane. Wanted: the wake time lives outside the
session, and the launcher starts the lane at that time with no mail and no session.

This supersedes the mail-based "scheduled `waits-poll`" proposal in `DURABLE-TIMERS.md` for the WAKE itself (that note's
blockers B1, the sender allowlist, vanish: no mail is sent). `waits-poll` stays for live lanes.

## Design

**1. Registry.** `<state>/wakes.json`: `{schema:1, wakes:[{role, event, wake_utc, registered_utc}]}`, one file, written
atomically (temp + rename, as the ledger is). The HANDOFF stays the lane's record; the registry is a *copy the supervisor can
read without the lane*. A lane's entries are REPLACED wholesale on every write, so a stale entry cannot outlive the HANDOFF line
that made it (delete the line, re-run `assert-idle`, the entry is gone).

**2. Writers.** `assert-idle` (after its checks pass and the state is recorded) replaces the role's entries with its valid
`WAIT:` lines. `lane-restart start` (any cause) deletes the role's entries whose `wake_utc <= now`: the lane is up and runs
`lane-restart waits` itself. Nothing else writes.

**3. Reader: the launcher cycle.** `Source` gains `due_wakes(now) -> Vec<Wake>` (default: none; the root crate's `Source`
reads `wakes.json`). Each cycle, for each due wake the launcher builds a synthetic signal
`{role, signal_id: "<role>:wake-<wake_utc compact>", attempt: 1, epoch: "wake"}` and runs it through the SAME
`Engine::handle` as a mail signal, so it inherits: unknown/closed-on-purpose refusal (a hard stop or park is not woken), the
per-role 3/h, overall 24/h and 8/day caps, the no-claim bound (rule 5), the grant (rule 7) and `Intent` crash safety.
Differences, all in one place: rule 7b (sender allowlist) is skipped for a wake (no sender); the result is NOT posted to
Synapse (there is no signal to answer; the row is marked posted at once).

**4. Outcomes and the wake's life.**

| Engine answer | Wake entry |
|---|---|
| Started / AwaitingDialog | consumed (deleted) |
| AlreadyRunning (started by mail first, or the lane never stopped) | consumed: no double start, the running lane runs `waits` |
| Refused `closed-on-purpose` or `unknown-role` | consumed, one log line (stopped on purpose; the PM sees it in `wakes`) |
| Refused `rate`, `no-claim`, `no-grant`, `registry-unreadable` | KEPT, retried next cycle (nothing is lost to a busy hour) |
| Failed | KEPT, retried next cycle, counted by the caps like any spawn |

**5. Stagger.** A wake is due at `wake_utc + jitter(role)`, `jitter = fnv1a(role) mod 600 s`, stable per role; and at most
`WAKES_PER_CYCLE = 2` wake-starts per cycle. A burst of 20 wakes at 00:00Z therefore spreads over about 10 minutes and the
caps absorb the rest; none is dropped. Past-due at registration: due at once, same cap and per-cycle limit, jitter not added
beyond the stable value (it is in the past too).

**6. What the woken lane is told.** Signal ids with the `wake-` prefix after `<role>:` get a different start prompt: "You were
started at your scheduled wake time (no mail). Read <role> HANDOFF, run `lane-restart waits`, run each check and act." A
mail start keeps today's prompt; a person's start keeps the generic one. `signal_names_role` already accepts the id (its
characters are alphanumerics, `-`, `:`).

**7. Read-out.** `lane-restart wakes [--json]`: one row per pending wake: role, event, wake time, due time (with jitter), time
left, and the state (`pending`, `due-waiting-for-launcher`, `role-running`). Read-only, never starts anything.

**8. Clock.** `due_wakes` and the pure selection function take `now` as a parameter: tests use a fake clock; no test sleeps.

## Known limits (stated, not solved here)

- **B2 (`DURABLE-TIMERS.md`)**: the launcher's `see` grant covers few lanes, so a woken lane that the launcher cannot see
  draws `started-never-claimed` 4 minutes later. Unchanged by this design; the PM's one-grant fix applies.
- The launcher must be running (it is a logon task). A wake due while the box is off is due at once when the launcher next
  starts (a past time, step 5).
- A `WAIT:` line's `check:` is not evaluated by the launcher; the lane runs it after waking. A wake is a time, not a result.
- Not measured on a real lane yet: the first real wake is the scratch `launchtest` test (`DURABLE-TIMERS.md` plan).

## Tests (fake clock, red first)

1. registry round-trip; replace-wholesale drops a removed line; a corrupt file is refused, not treated as empty.
2. `assert-idle` registers valid waits, and a refused assert-idle registers nothing.
3. selection: not due, due, due only after jitter, past-due at registration is due now.
4. cycle: due wake starts the lane once; the second cycle does nothing (consumed).
5. already running (mail got there first): consumed, mechanism answer AlreadyRunning, no second spawn.
6. closed-on-purpose: consumed, not started. Rate cap: kept, started after the hour clears.
7. stagger: 5 due wakes start 2 per cycle; none lost.
8. start prompt for a `wake-` id; mail prompt unchanged.
9. `wakes` read-out lists pending, due, and role-running.
