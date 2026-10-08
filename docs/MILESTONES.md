# agentlife implementation milestones

Approved by the PM 2026-10-07 against `DESIGN-REVISION-1.md`, `-2.md`, `-3.md`. Sizes are relative (S/M/L),
not estimates. Order is chosen so data accumulates before anything acts on a real lane.

**Gates (PM rulings, 2026-10-07).** M0 to M3 start now: nothing in them acts on a real lane or restores
anything. M4 (real restore, consent) waits for OverMind's `user-request` crate PRs. M5 and M6 follow M4. M7
(lazy start) stays gated on Synapse S-1 (pending counts), the Claude Code channel adapter (M7) and the
cutover (M9). After M1 the PM installs the `SessionStart`/`SessionEnd` hook entries on **one** lane,
observes, then widens (the entries CireSnave approved; **no `Notification` hook**).

| # | size | delivers | exit criteria | depends on |
|---|---|---|---|---|
| **M0** | S | foundations: config loader (flag > env > file > default; unknown key refuses the file), the journal with monthly segments, the registry store (atomic write + lock), identity by pid + process start time, install by rename | unit tests; a 20-thread concurrent-write test that loses no record; tests never touch `C:/Projects/.lane-state` (an `AGENTLIFE_HOME` override) | OverMind: `recorded_cwd` and `write_atomic` public, `name` recorded, process start time recorded; consumed as a git dependency by rev |
| **M1** | M | `agentlife hook SessionStart` and `SessionEnd` (one entry per documented matcher), and `list`; interactive-only filtering; `agent_id` carried by environment variable; **uses a temporary copy of OverMind code, see below** | real hook JSON in, real record on disk, read back by `list` (the acceptance check is a real write, not a diagnostic); `SessionEnd` finishes well inside its 1.5 s budget | M0 |
| **M2** | M | intent model; `park`, `stop`, `unpark --no-start`; graceful `down` (HANDOFF + `assert-idle` signals); the closed-on-purpose inference; `pin` and `waiting` marks | the intent-rule table test, asserted by agent name | M1 |
| **M3** | L | plan builder and launcher: candidates, order, batches, RAM floor, `max_running`, tabs per window, idempotence, frozen plan + hash, launch through `lane-restart host`, per-batch liveness, report, `restore --dry-run` | planner property tests; fleet-sim at 40 and 1,000 stand-ins; a real spawn/kill test of a 2 to 3 stand-in roster through the real `wt.exe` (the `conhost.exe` fallback on CI) | M2 |
| **M4** | L | durable pending restore; consent through the `user-request` crate (`bound_hash` = plan hash, no timeout); control tab; `agentlife pending`; logon task, unlock trigger, network wait | pending record survives a kill; staleness = agents that still exist; first real restore on canary stand-ins, then one real lane, with CireSnave aware | **OverMind `user-request` PRs** |
| **M5** | M | standing grants: 1 h / 1 d / until / forever with typed confirmation, `approvals list\|show\|revoke`, HMAC over DPAPI, audit; bypass is PM-only in the plan builder | expiry boundary, revoke, tamper, field-change-voids-coverage | M4 |
| **M6** | M | `docs/ACCEPTANCE.md` with real runs; the canary experiment (does `SessionEnd` fire on window close, `taskkill`, a real shutdown, and its payload); env var through `wt.exe`; unlock trigger; toast if feasible | logged commands and outputs | M4, M5 |
| **M7** | L | lazy start: `agentlife watch`, wake poller, idle sweep, exemptions S1/S2/S4, wake grants, storm limits; ships `lazy_enabled = false` | a test against a real `synapsed` | Synapse S-1, channel adapter, cutover |

## Temporary copy: `src/claude_proc.rs` (from OverMind `c3935ba`)

**TEMPORARY. Blocker:** `lane-restart` is not on crates.io (404, 2026-10-07), and the portfolio rule
(`CLAUDE.md` §9 "Sources"; `CIRESNAVE-EXPECTATIONS.md` §6.8) forbids a new `git =` dependency.
**Blocker to clear:** OverMind extracts a small crate holding these items and CireSnave clears its
publish. **Owner:** the OverMind lane. **End state:** `src/claude_proc.rs` is deleted and `hook.rs`
depends on the published crate. The PM ruled this on 2026-10-07 (sign-off for the provenance rule
included). Origin, a hash of every copied item, the three adaptations and how to re-verify are in
`docs/COPIED-FROM-OVERMIND.md`.

## Not in any milestone

Whether `SessionEnd` fires at shutdown is **unknown** until M6; M1 to M5 are built to work either way
(`DESIGN-REVISION-1.md` §2.6). The restore *order* is CireSnave's (board 121); the planner implements the
default (PM first, then most recently active) with an empty optional `priority` list.

**M4 progress (2026-10-08).** `user-request` is unpublished, so M4's consent is built behind a trait with a fake (PM ruling, `docs/CONSENT.md`): pending record, `agentlife pending`, consent contract. Logon and unlock tasks: `agentlife install-task`, `restore --from-logon`, `pending --prompt` (`docs/TASKS.md`; creating the unlock trigger without elevation is measured to work; that it fires is M6). Not yet: real backend, control tab, supersede-by-newer-boot.
