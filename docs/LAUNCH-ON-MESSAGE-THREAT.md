# Launch on message: what the `agentlife-launcher` caller can and cannot do

Scope: `lane-restart start` accepts a second caller, role `agentlife-launcher` (renamed from `overmind` at 0.15.4 by PM ruling 2026-10-15: the service must never claim `overmind`, the OverMind lane's role; the old name is accepted for one more release), for the launcher service that acts on a
Synapse mail-waiting signal (CireSnave 2026-10-15, relayed by the PM: "Synapse->OverMind->AgentLife launch").
Written at 0.15.2 against `crates/lane-restart/src/start.rs`. Nothing here was run against a real service: the
service does not exist yet, and `start` is PM-only on this machine.

## What the identity is

- The service writes `<state>/agentlife-launcher.launcher-caller` (`{role, pid, pid_start_secs}`; deliberately not
  `*.json`, which every other reader treats as a lane's state file) naming **itself**.
- `lane-restart start` accepts it only when the **direct parent** of the running `lane-restart` is that pid
  **and** the process start time matches (a reused pid does not inherit it). One level, not the ancestry.
- The PM is still recognised first, by its `claude` ancestor and the PM state file, as before.
- The trust root intended by the PM is the Synapse `launcher_roles` credential of role `agentlife-launcher`, held by the service
  through `synapsed-client`. **This record is only a local mirror of that fact**; `lane-restart` does not talk to
  Synapse and cannot check the credential. Whether the record is written only while the service holds a valid
  session is the service's contract (PR3), not something this gate verifies.

## What a caller holding that identity CAN do

- Start a lane **that already has a launch spec** (`<role>.launch.json`, written by `stop`, `specs import` or
  `create`), is not running and is not a duplicate-risk. Never a role without a spec, never `pm`.
- Only for a `--signal-id` of the form `<role>:<id>` whose role is the lane being started, so one signal
  cannot be spent on another lane. The id is otherwise not checked here (a forged id for the right role passes).
- With the model pinned: `sonnet` or `haiku` only, as for the PM; Opus and `LANE_RESTART_ALLOW_OPUS` are refused.
- Every call is ledgered: `requested_by` and `caller_role` = `agentlife-launcher`, the signal id inside the reason, the
  refusal or the outcome. A refused call is ledgered too.

## What it CANNOT do

- `stop` and a restart: both read the `claude`-ancestry caller only, and `require_pm_for_other` refuses any role
  but `pm` (tested with `agentlife-launcher` and `overmind` in the refused list).
- Start `pm`, change a spec's model upward, pass flags, a prompt or a working directory: `start` takes the spec
  as written, with `--model` pinned and permission mode `auto` (`plan_start`); it adds nothing the caller chooses.
- Read or answer a dialog, kill a process, or write anything but the spec's `running` mark and the ledger.

## If the identity is forged or the service is compromised

Anything running as the same user can write the record and run `lane-restart` as a child of a process it
controls, exactly as it could forge the PM's state file today. So the bound is *what a forged `agentlife-launcher` can do*:

- At worst: start lanes **that already exist and have a spec**, as often as it likes, each on Sonnet or Haiku,
  and name any role in the signal id. Cost: tokens and memory, not data or code execution.
- The rate cap (3 per role and 12 overall per hour) and the no-claim bound live in OverMind's service, **not
  here**. This gate has no rate limit of its own; a caller that bypasses the service is limited only by the
  `AlreadyRunning`/duplicate-risk refusals and the disk floor. `start` has no memory floor (that is
  `agentlife restore`'s), so a burst of distinct roles is not slowed by anything here.
- It cannot widen its own authority: the record and the gate grant `start` only, and `stop` is unreachable.

## Not covered

- Consent: an unattended launch is still gated by CireSnave's ruling on standing grants (M5, unbuilt). This
  gate says *who may call*; it does not say that a mail-triggered launch is approved. The service must refuse
  `no-grant` until that exists (OverMind's policy, rule 7).
- Replay: `start` is idempotent through `already-running` (exit 3), not through the signal id. De-duplication
  by `(signal_id, attempt, epoch)`, with a generation counter for an epochless repeat after the replay window, is
  the service's (`docs/LAUNCHER-SERVICE.md`, Store); an `epoch` change is a new request, never a replay.
