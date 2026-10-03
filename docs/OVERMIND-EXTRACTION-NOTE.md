# Note for the OverMind lane: extract `relaunch` from `main.rs` into the library

From the `agentlife` lane. **A design note, not a request to start.** It is for the PM to forward when
`DESIGN.md` §8 phase 2 is approved. `V0.1.md` does **not** depend on it. Evidence: OverMind `origin/main`
`c6c8c1e`, `crates/lane-restart/src/main.rs` (2200 lines), read with `git show`.

## Why

agentlife must launch and verify lanes with exactly lane-restart's mechanics (ConPTY host, the argv
allowlist, the `;` guard, the session-env strip, the liveness protocol). Today all of it is a private
`mod relaunch` inside the binary: `main.rs` lines 723–2200, of which the tests are lines 1341–2200. The
library (`lib.rs`) exports `approvals authorize facts handlers host lane_state_writer log notify paths
state tab_close` and nothing from `relaunch`, so another crate can only copy it. "One tool, not two"
(brief, point 8) means one copy.

## What moves

Move `mod relaunch` to `crates/lane-restart/src/relaunch.rs` and add `pub mod relaunch;` to `lib.rs`.
`main.rs` then does `use lane_restart::relaunch;` and nothing else about it changes: its only uses are
`relaunch::RelaunchOutcome` (lines 319, 326, 595, 607, 683, 685, 686), `describe_dry_run` (520),
`RealStateReader` (568) and `kill_and_relaunch` (584). Its imports are already library paths
(`lane_restart::facts`, `lane_restart::state`). The tests move with it, unchanged.

## What must become `pub` (and nothing more)

`valid_identifier`, `RelaunchError`, `RelaunchOutcome`, `StateReader`, `RealStateReader`,
`describe_dry_run`, `kill_and_relaunch` (already `pub`), plus the pieces agentlife needs that are private
today: `claude_argv`, `extra_launch_args`, `first_unsafe_argument`, `host_wrapped_argv`, `spawn_relaunch`,
`wait_for_relaunch_liveness`, `SESSION_IDENTITY_ENV_VARS` / `strip_session_identity_env`.

## Three seams a plain move does not give agentlife (second, separate commits)

1. **`spawn_relaunch` and `claude_argv` take a `&LaneState`.** A restore of a dead lane has a roster
   entry, not a state file. Introduce `LaunchSpec { role, name, cwd, model, permission_mode,
   remote_control, launch_args }` with `From<&LaneState>`; the two functions take `&LaunchSpec`. Behaviour
   identical for lane-restart.
2. **`wait_for_relaunch_liveness` hard-codes `PROGRESS_TIMEOUT` 20 s and `TOTAL_TIMEOUT` 15 min** (its own
   comment: "not yet a CLI flag"). Restore wants a per-batch 120 s. Make both parameters of a
   `LivenessTiming` struct whose `Default` is today's values. It also takes `old_session_id: &str` and
   `killed_at_secs`; restore has "no previous session" and "launched at" instead. Take
   `Option<&str>` and rename to `launched_after_secs`, keeping the 5 s clock margin where it is.
3. **`kill_and_relaunch` bundles kill + 500 ms sleep + spawn + wait.** Split out `launch_and_wait(spec,
   timing, …)` (no kill) and keep `kill_and_relaunch` as `kill_verified` then `launch_and_wait`.
   Restore calls the second directly.

## Constraints

- **No behaviour change to the installed `lane-restart.exe`** in the move commit. Verify by running the
  existing tests unchanged (the spec cites 79 at one point; count them before and after) and diffing `describe_dry_run` output for a recorded state file before and
  after. A moved test that is edited is a finding, not a fix.
- The PM allocates the version at gate time (CLAUDE.md §9). The installed binary is in use by every
  hook, so the PM installs it with the rename-not-overwrite sequence (OverMind spec §11.2).
- The real-process tests (`real_parent_process_cmdline_of_reads_a_real_spawned_childs_actual_argv`,
  `cwd_of_reads_a_real_spawned_childs_actual_working_directory`) must still run in the library crate's
  CI, not only the binary's.

## Separate small fix, same PR family but its own commit

`-n` (the short form of `--name`) is in neither `ALLOWED_LAUNCH_ARG_FLAGS` nor `DROPPED_LAUNCH_ARG_FLAGS`
(lines 856–893). `pm.json` has `-n PM` in `launch_args`. It is dropped as an "unknown flag" and its value
falls through as a bare positional, which is dropped silently: harmless today only because a fresh
`--name` is emitted from `role`, so the PM relaunches named `pm`, not `PM`. Adding `("-n",
FlagArity::One)` to the dropped list makes that explicit. Also: `lane_state_writer` never sets `name`
(`apply_event` copies the previous value; nothing writes one), so every `name` is null. Reading `-n`/
`--name` from the process command line next to `remote_control` would fix it. Both are findings for the
OverMind lane to rule on, not part of the extraction.

## Open for the OverMind lane

- Does it want the library split further (a `relaunch` module vs a `launch` and a `liveness` module)? The
  note keeps one module to keep the diff a pure move.
- Git dependency from agentlife on OverMind's workspace crate, or a shared crate in its own repo
  (DESIGN Q12)? Affects only how agentlife's `Cargo.toml` refers to it.
