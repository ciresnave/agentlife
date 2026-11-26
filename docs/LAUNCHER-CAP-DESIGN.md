# Launcher spawn caps: design note (PM FINDING `jds-hu`; PM reads this before any code)

Status: DESIGN ONLY, nothing built. Read at `origin/main` 80ace5d (0.30.87). "READ" = read in source at that ref; "REPORTED" = stated by the PM or in the HANDOFF, not re-measured here.

## The problem (REPORTED)

A launcher start that never produced a working session (stuck at the dev-channels dialog, spawn failed) spends the role's daily cap. Once the cap is hit the lane is refused for 24 h exactly when it most needs a start, and nobody is told. Separately, a launch spec left in `stopping` (REPORTED: easyants since 2026-11-21) blocks the role.

## What the code does today (READ)

1. **Where the caps live.** Two places, same numbers. `crates/launcher/src/lib.rs`: `RATE_PER_ROLE = 3` and `RATE_OVERALL = 24`, hourly only, counted from the engine's own `requests` (`counts_spawn`: an `Intent` row, or a called row that is `Started` or `spawn-failed`). `crates/lane-restart/src/launcher_gate.rs`: 3/h per role, 24/h overall, and `MAX_SPAWNS_PER_ROLE_PER_DAY = 8` (the only daily cap; its comment calls it a proposal awaiting the PM), counted from `restart.log` rows where `kind == start`, `spawned == true` and the caller is a launcher. Refusal text: `rate` (hourly) or `rate-daily`.
2. **What sets `spawned: true`.** `crates/lane-restart/src/app/start_cmd.rs`, `finish(.., spawned, ..)`: `true` for BOTH the `Ok` arm (`started` and `awaiting-dialog`) and the `Err` arm (`failed:spawn-failed`). Every other `finish` passes `false`. So a start stuck at the dialog (`awaiting-dialog`) and a failed spawn each count toward all three caps. This is the defect.
3. **No row records whether the session ever ran.** The ledger row is written once, at the start, with `launch_to_live_secs` only in `--json` output. Nothing is written when the lane later comes online or dies.
4. **A cap refusal is silent to people.** `refuse` writes a ledger row and, under `--json`, a `detail` the launcher logs. The only outbound path is `Notify::alarm` (`launch-stalled:` text to the PM), which fires only for `started-never-claimed`, never for a cap.
5. **Stuck `stopping`.** `lane-restart stop` writes `stopping`, kills, then writes `suspended`; if the last write fails the spec stays `stopping` (exit 4) (docs/LAUNCHER-SERVICE.md, table). The two readers disagree: `spec.rs` (status, ~line 798) maps `Stopping` with no live process to `Lifecycle::Crashed`, while the launcher rule (docs/LAUNCHER-SERVICE.md "The launcher rule") refuses `stopping` as `closed-on-purpose`. So `status` calls a lane crashed and the launcher will never start it, with no age limit.

## Proposal

### A. A start that did not run does not spend the daily cap

Count a start toward the DAILY cap only if its session ran. Two ways to know:

- **A1 (recommended): a settle row.** When `lane-restart start` returns `started`, it already waits for progress (`launch_to_live_secs`). Write `spawned` as before (the hourly caps keep counting attempts: they protect the machine from a spawn loop, and that is correct), and add `ran: true|false|null` to the row: `true` when `launch_to_live_secs` is set, `false` for `failed`, `null` for `awaiting-dialog` (unknown at write time). The daily count then uses `ran != false` AND, for `null`, a later row: the launcher, which already observes online roles each cycle (`observe_online`), appends a `ran` row for the signal when it sees the role online, and rows still `null` after N = 10 min read as `false`. Cost: one new ledger field (additive, same schema rule as `spawned`) and one launcher write.
- **A2: sessions that ran at least N minutes.** Count a start only if the role was seen online for >= N min after it. Truer to "the lane worked", but it needs the launcher to track durations and delays the count by N min, during which a loop could exceed the cap. Not recommended as the only rule; usable as a refinement of A1's `null` case.

Hourly caps and rule 5 (`no-claim`, bound 3) are NOT changed: a dialog-stuck start still counts there, which is what stops a loop. The daily cap becomes "8 sessions that ran", not "8 attempts".

### B. A cap refusal mails the cluster PM once per role per day

On a refusal with reason `rate-daily` (and `rate` for the hourly cap only if it repeats; start with daily), the launcher sends ONE message per role per UTC day through the existing notify path: header `[FINDING] launch-cap-hit ref=<role> by=agentlife-launcher status=refused basis=<cap number>`, plus `note:` lines with the cap number (8/day or 3/h) and the last 8 spawn outcomes for that role (time, reason, `ran`). Recipient: the role's cluster PM (map: `LANE-REPORTING.md`), cc pm-platform. Dedupe: a `cap_notified_day: "YYYY-MM-DD"` per role in `launcher-store.json` (like `alarmed`), set only after delivery succeeds, so a failed send retries next cycle and a delivered one is not repeated. The last-8 text is read from `restart.log` (launcher rows for the role), so it needs no new store.

Open question for the PM: if the cluster PM cannot be resolved for a role, fall back to pm-platform. Say so in the message.

### C. A stale `stopping` spec reads as suspended

Decision needed: which of the two readings is right for a `stopping` spec older than N min with no live process? The kill already happened (stop writes `stopping` BEFORE the kill), so the lane was stopped on purpose and only the final write failed. That is `suspended`, and the launcher already refuses it (`closed-on-purpose`); the defect is that `status` says `Crashed` and nothing ever finishes the write. Proposal:

- Reader: a `stopping` spec whose `updated_at` is older than N = 15 min and whose process is gone reads as `Suspended` in `status` (one reading, both readers). Younger than N it stays `Stopping` (a stop in flight).
- `status` shows `suspended (stop did not finish, N min)` so the lane is not silently parked.
- No auto-write: the launcher does not rewrite a spec; `agentlife unpark --no-start` already rewrites `stopping` to `crashed`, which is how a person reopens it.

This does NOT make a stuck lane startable; it only makes the reading consistent. If the PM wants a stuck `stopping` lane started by mail instead, that is the opposite decision (treat as `crashed` = launchable) and loosens a safety rule; not recommended without a ruling.

## Tests (red first, fake clock, none sleeps)

1. `awaiting-dialog` then no online row for 10 min: the daily count is 0; the hourly count is 1.
2. `failed:spawn-failed`: daily 0, hourly 1.
3. 8 `started` rows with `ran: true`: the 9th is `refused: rate-daily`; 8 dialog-stuck rows then a start: allowed.
4. Cap refusal: exactly one message per role per UTC day; a failed delivery is retried next cycle; a new day sends again; the text carries the cap number and the last 8 outcomes in order.
5. `stopping`, 14 min old, process gone: `Stopping`; 16 min: `Suspended`; a live process: `Running`.
6. Old ledger rows without `ran` count as before (no migration, no undercount).

## What this note does not decide

Whether 8/day is the right number (still the lane's proposal, never ruled); whether the hourly caps should also mail; N for A1 and C (10 and 15 min are placeholders, unmeasured).

— lane: agentlife
