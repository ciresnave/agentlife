kind: launcher daily cap spends only on sessions that ran, one cap mail per role per day, stale `stopping` reads suspended
---
`docs/LAUNCHER-CAP-DESIGN.md` (PM-approved 2026-11-26, jds-kt), built:

- **A1.** `restart.log` start rows gain `ran` (additive, absent on old rows). `started:started` writes `ran: true`, `failed:spawn-failed` writes `ran: false`, a start left at the dialog writes none. The per-role DAILY cap (8) no longer counts a failed start, or a dialog-stuck one older than `SETTLE_SECS` (10 min) unless a `settle` row (`ran: true`, same `signal_id`) exists; the hourly caps and rule 5 still count every attempt. The launcher cycle writes that `settle` row once, when it sees the role online (`Source::settle`, `Engine::unsettled_online`, `Request.settled`).
- **B.** A start refused by the daily cap (`rate-daily`) is now recorded as `refused`, not `failed:spawn-failed`, so it spends no hourly budget either. The launcher mails a `[FINDING] launch-cap-hit` once per role per UTC day (`RoleRow.cap_notified_day`, set only after delivery) with the cap number and the role's last 8 outcomes, to the role's cluster PM read from the table named by `AGENTLIFE_LANE_REPORTING` (`LANE-REPORTING.md`), else `pm-platform` with a note saying so.
- **C.** `status` reads a launch spec left in `stopping` for over 15 min (`STOPPING_STALE_SECS`) with no live process as `suspended` ("stop did not finish"); the launcher already refused it as closed-on-purpose, so the two now agree. No auto-write.
- `scripts/synapsed-task.ps1 -FetchLog <v>` writes `set "SYNAPSE_FETCH_LOG=<v>"` into the generated wrapper; unset writes no line.

Verified by unit tests (fake clock, no sleeps) in `launcher_gate`, `spec`, the launcher crate's `service` and `launcher_service`; the `-FetchLog` text by running the script's dry run with and without it. Not run against a real lane.
