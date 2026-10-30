# M5 and the wake path: design check (scale-to-zero)

Written 2026-10-28 at `origin/main` 582be0a (0.29.23). PM task 2026-10-28 (CireSnave: *"We need the ability to
scale lanes that aren't our current priority to zero when not actively in use and spin them back up when
something messages them or on command from you or I."*). Checked against `DESIGN-REVISION-2.md` section 7,
board 216 (`CIRESNAVE-DECIDED-ARCHIVE.md`, "216: yes"), `LAUNCHER-SERVICE.md` and `LAUNCH-ON-MESSAGE-THREAT.md`.
Read from the files at that ref; nothing here was run against a real lane.

## What already exists (read from code, not assumed)

| piece | where | state |
|---|---|---|
| soft/hard stop mark in the launch spec | `lane-restart` `spec.rs` `StopKind`, `lane_stop.rs` | built (0.15.10 writer) |
| `stop --self` is always soft; `--self --hard` refused | `lane_stop.rs` | built |
| launcher reader: soft or crashed starts, hard or unclassified refused | `start.rs` (0.15.12) | built |
| caps 3/h per role, 12/h overall, 8/day per role; memory floor; `restore.lock`; per-role start lock | `launcher_gate.rs`, `start_lock.rs` (0.15.11, 0.15.13) | built, not run on a real lane |
| every launch ledgered with signal id | `restart.log` | built |
| the poll loop and rules 1-6 | `crates/launcher` | built, notifier-only |
| **rule 7 (`Directory::granted`)** | `src/launcher_service.rs:278` | **stub: returns `false`**, so every signal is `refused: no-grant` |
| **the mechanism** | `NoMechanism` | **stub: returns `mechanism-not-built`** |
| PM `lane-restart start`, unattended | `start.rs` | works (PM reports three real starts) |

So the 216 conditions are mostly in place. What is missing is the two stubs, and a sender check.

## Does 216 need the M5 store at all?

Two different consents, and `DESIGN-REVISION-2` 7.1 already keeps them apart:

- **Implied grant (216):** a soft stop *is* the grant. It lives in the spec as `stop_kind`, is written by the
  lane's own `stop --self`, and has no duration. It cannot be given to a role that was never started by a
  person, and a lane cannot hard-start itself.
- **Standing grant (7.1, M5):** a person says "agent X may launch up to mode M, until T / forever", signed
  (HMAC over a DPAPI key), listed, revocable, with `approvals list|show|revoke`.

**Finding:** the wake path for soft-stopped lanes does not need the M5 store. 7.1's "no wildcard, named
agents, `max_mode`, flag allowlist" is for grants a person types; the implied grant has none of those fields,
and the launcher can only start with `--model sonnet`, mode `auto`, no flags, no prompt of its own. Forcing it
through a HMAC store would add a second source of truth for the same fact (`stop_kind`) that can disagree with it.

Where the two do meet: 7.5 says a grant is void if the agent's launch fields grew. The implied grant needs the
same guard, and `start` already re-runs the *recorded* spec rather than taking fields from the caller, so a lane
that changes its cwd or mode between its stop and the wake is launched as recorded, not as changed. **Open:**
whether the spec can be edited between the soft stop and the wake by the lane itself (same-user write). 7.4
states the limit for the signed store ("a casually misbehaving lane cannot create one, not more"); the spec
file has no MAC today, so the implied grant is weaker than that. That is the cost of not using the store.

## Proposed slices (each one PR, each test seen red first)

1. **Rule 7 reads the implied grant.** `RegistryDirectory::granted(role)` is true iff the role's spec is
   `suspended`/`stopping` with `stop_kind = soft`, or `crashed`/dead-`running` (the same predicate `start.rs`
   already applies), and registry intent is not `Closed`. One predicate, shared with `start.rs`, not a copy.
   Tests: soft true; hard, unclassified, `Closed`, no spec, unreadable spec all false.
   **Built in the PR carrying this file (0.29.25 placeholder).** `RegistryDirectory::granted` and `lane` now call
   `lane_restart::start::launcher_may_start` (made `pub`), so rule 7 and `lane-restart start` read one predicate;
   a test visits all 15 (state, kind) pairs against it. Found while doing it: rule 3 (`lane`) refused *every*
   `suspended`/`stopping` spec as closed-on-purpose, so a soft-stopped lane could never reach rule 7, and a
   registry `Closed{exited}` (what `agentlife stop --soft` leaves) refused it too. Both now defer to the spec,
   but only with `lazy_enabled` on; off, behaviour is as before (a test pins that). A closed registry intent is
   still not overridden by a spec that merely says `running`. **Left as it was, and open:** a lane with no
   registry record is `unknown-role` whatever its spec says, so a lane managed only through `lane-restart`
   (29 `*.launch.json` exist in `C:\Projects\.lane-state`, read by count only) is not wakeable until the PM says
   a spec alone makes a lane known.
2. **The real mechanism.** Replace `NoMechanism` with a call to `lane-restart start --signal-id <role>:<id>`
   as a direct child of the launcher (the identity `launcher_gate.rs` already accepts), mapping its `--json` to
   the eight results. Off by default until the PM turns it on (`MILESTONES.md` M7 ships `lazy_enabled = false`).
3. **Sender allowlist (216 condition "signed allowlisted Synapse senders only"). The wake mechanism is not
   switched on without it (PM ruling 2026-10-28).** Read at synapse `origin/main`
   (`docs/superpowers/specs/2026-10-15-cutover-and-lane-launch.md` C2): the `/v1/waiting` signal carries
   *"role, signal id, count and timestamps only. No mail body, no sender"*, by OverMind constraints 1 and 3. So
   the launcher cannot enforce an allowlist from the signal, and I find no sender check in
   `crates/launcher/src/lib.rs` or `src/launcher_service.rs` (grep for allowlist/sender/signed: 0 hits in those
   two files). `launcher_roles` says who may *poll*, not who sent the mail. Two ways to close it, both need the
   Synapse lane: **(a)** the daemon counts only mail from allowlisted sender roles toward a launch signal
   (a `launch_senders` setting beside `launcher_roles`; the launcher learns nothing new, the constraint holds);
   **(b)** the signal gains the sender roles of the pending mail (names, no bodies), which relaxes the stated
   constraint. Recommend (a). Needs the Synapse lane, and the PM to say whether relaxing "no sender" (b) is
   CireSnave's. Launcher side once (a) lands: nothing to enforce, but a test that a signal for a role is only
   acted on when the daemon is configured so; until then the mechanism stays off.
4. **Integrity of the soft marker (PM ruling 2026-10-28).** Today `stop_kind` is a plain field in
   `<role>.launch.json`; any process that can write `.lane-state` can set `soft` and, with a stopped spec, make
   `rule 7` true. A MAC over the spec's identity fields (role, cwd, model, `stop_kind`, `state`, written-at)
   keyed from a DPAPI-protected key (the `src/consent/real.rs` pattern) makes an *edited* file fail closed:
   unverified means unclassified, which means hard. **Stated limit, as in `DESIGN-REVISION-2` 7.4:** `stop --self`
   runs as the lane, so the lane's own user can reach the same DPAPI key; this stops an edit and a casual
   forger, not a same-user process that calls the signer. Real separation needs the signer to be a different
   principal (the launcher service signing a stop it observed in `restart.log`), which is a larger design.
   The bound if it is forged anyway is the one `LAUNCH-ON-MESSAGE-THREAT.md` already accepts: start a lane that
   has a spec, on Sonnet in mode `auto`, under the hourly and daily caps. Whether that residual risk needs
   CireSnave is the PM's to say; I recommend it does not, since it adds nothing to that bound.
   **Built (0.29.32 placeholder):** `crates/lane-restart/src/marker.rs`. The MAC covers the whole serialized spec, not only the identity fields, so an edited `cwd`, `launch_args` or `model` also fails; the check is in `spec::read` (one chokepoint, every reader sees an unverified soft stop as unclassified), the signing in `spec::write_atomic`. Soft stops written before this build read as unclassified until the lane is stopped again. The same-user limit above stands. Slices 3 and 4 are now both in; Slice 2 (the real mechanism, 0.29.34 placeholder: `LaneRestartMechanism`, handed to the engine only when `lazy_enabled` is on) is now in too; switching the wake path on still needs the PM's word.
5. **Explicit M5 store** (`approvals list|show|revoke`, 1h/1d/forever): separate, only if the PM wants grants
   for lanes that were never soft-stopped (`lazy` agents). Not needed for scale-to-zero.

**Order:** 1 and 2 are mergeable now with the mechanism off. 3 and 4 gate *switching it on*, not merging it.

## Scale-to-zero, the stop side

Rule today (PM): a lane with no queued work runs `lane-restart stop --self` instead of `--self` restart.
The tool already supports it (`stop --self` is always soft, per `LAUNCHER-SERVICE.md` and `lane_stop.rs`); the
lane-side instruction is the PM's edit to `CLAUDE.md`. Not checked: whether `assert-idle` and `stop --self`
interact well for a lane that is about to leave an empty queue.

## Questions for the PM (not for CireSnave directly)

1. Answered A (2026-10-28): slices 1 and 2 ship without the M5 store, mechanism off by default.
2. Allowlist: (a) or (b) in slice 3, and does the Synapse lane take it?
3. Versions are yours; each slice is a minor bump (new behaviour, not breaking).
