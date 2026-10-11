# Changelog

One version for the whole workspace (`agentlife`, `lane-restart`, `lane-state`).

## 0.12.14

- **Restarting another lane is the PM session's alone, as `stop` is** (the PM's ruling of 2026-10-11, RESTART-TOOL-
  DESIGN.md 14.7 / 14.8). `lane-restart --role R` without `--self` is refused unless the caller's `claude`
  session is the one `pm`'s state file records (pid and start time; `LANE_ROLE` is not consulted), and the
  refusal is logged and happens before anything about the target is read, a dry run included. `--self` is
  unchanged. Before this, a restart of another lane was labelled `pm` in `restart.log` whoever ran it; all 475
  lines in `restart.log` at 2026-10-11 20:25Z carry `self` (453) or `pm` (22), so the log cannot show whether any
  `pm` line was a non-PM caller (the label was never checked), only that no other role was ever recorded.
  Breaking for anything that restarts another lane from outside the PM session, such as a person at a plain
  terminal, which has no `claude` ancestor to verify.
- `lane_stop::require_pm_for_other` is the one check `stop` and the restart share.

## 0.12.13

- **`lane-restart stop --role R --reason <text> [--self] [--dry-run] [--yes]`** (RESTART-TOOL-DESIGN.md 14.3,
  PR 3). Suspends a lane in a fixed order: every check, then the complete launch spec written durably and read
  back (`state: stopping`), then the verified kill, then the spec marked `suspended`. A lane is stopped only if
  it can be started again, so a spec that cannot be written leaves the process untouched. Another lane's stop is
  **PM only, by process** (the same ancestry check as `start`), needs the lane to be idle, and is a dry run
  without `--yes`; `--self` stops the calling lane. A HANDOFF is required, with no override: it must exist, be
  newer than the session being stopped, and (for another lane) not newer than the lane's idle assertion. Role
  `pm` is refused, because `start` refuses it and a suspended PM would have no way back. A lane on Opus (or an
  unknown model) is suspended with its model replaced by `sonnet`, and the ledger says `model-pinned:<was>`.
  Exit codes: 0 stopped and suspended, 1 refused or failed, 4 stopped but the `suspended` mark could not be
  written (the earlier spec is complete, so the lane stays startable).
- **A restart now launches from the spec it just wrote**, read back by the store, not from the state file it was
  derived from (14.7), and prepares that launch before the kill. A launch flag the spec's allowlist drops is
  therefore no longer carried into a restarted session.
- **`restart.log` `requested_by` is the verified caller** for a restart of another lane: the role whose `claude`
  session ran the command, or `unverified`, where it used to be the label `pm` whenever `--self` was absent.
  `caller_role` is set alongside it. A restart of another lane is not refused on it: the idle checks and `--yes`
  are unchanged.
- New public API: `lane_restart::lane_stop::{plan_stop, build_stop_plan, execute_stop, check_handoff,
  StopRequest, StopPlan, StopRefusal, StopError, StopDone}`; `spec::find_handoff` is public.
- Not in this change: PR 4 (agentlife's `park`/`stop`/`restore` consuming these, retiring `src/down.rs`'s kill
  and `tools/launch-lane.ps1`).

## 0.12.12

- **`lane-restart host` no longer abandons its helper threads.** The PTY reader sat blocked in `ReadFile` for the
  rest of the process and the resize thread, which owns the ConPTY master, never ended at all, so the pseudo
  console was never closed. The resize thread now stops when the child has exited; dropping the master closes
  the ConPTY and ends the reader, which the host waits for (3 s bound). `HostOutcome` gains
  `abandoned_threads`: the helpers still running at return, by name (a blocked outer stdin read cannot be
  cancelled through a generic `Read` and is reported as `outer-input`). Behaviour on the wire is unchanged.

## 0.12.11

- **`lane-restart start --role R --reason <text> [--model sonnet|haiku] [--dry-run]`**
  (RESTART-TOOL-DESIGN.md 14.5, PR 2). Starts a lane that is not running, from its launch spec, and writes the
  spec back as `running` once the session is confirmed. **PM only, by process**: the caller's `claude`
  ancestor must be the pid and start time recorded by `pm`'s state file (`LANE_ROLE` is never consulted).
  `--reason` is required (<= 200 characters, no control characters) and goes to the ledger, not the prompt.
  The model is `sonnet` or `haiku`; an Opus request is refused, a spec whose model is Opus, unknown or unset
  is launched on `sonnet` and the ledger says `model-pinned:<was>`, and `LANE_RESTART_ALLOW_OPUS` is not read.
  The permission mode is always `auto`; a start of role `pm` is refused and reported. The dead session's own
  state file is never mistaken for the new session coming up.
- **`lane-restart specs import [--role R] [--dry-run]`** (14.9, PM only): builds `<role>.launch.json` from a
  lane's state file for each role with no spec, labelled `written_by: "import"`, only when the spec is
  complete. The rest are listed with the reason; an existing spec is never overwritten and no state file is
  edited. This is what makes a lane that crashed before it ever launched under a spec-writing build (baracuda,
  2026-10-11) startable.
- `restart.log` lines gain `kind`, `caller_role`, `model` and `reason`, each omitted when unset, so existing
  lines and readers are unaffected. Restart-path lines now carry `kind: "restart"`.
- New public API: `lane_restart::start`, `lane_restart::spec_import`, `spec::pin_model`,
  `SpecFile::pinned`, `SpecState::Crashed`, `log::Entry::{kind, caller_role, model, reason}`.
- Not in this change (PR 3 onward): `lane-stop`, the `lane-restart` wrapper over stop + start, and the
  reader-thread fix in `host.rs`.

## 0.12.10

- **A restart writes the lane's launch spec before it kills anything** (RESTART-TOOL-DESIGN.md 14, PR 1).
  `C:/Projects/.lane-state/<role>.launch.json`: an allowlist serialisation of how the lane is launched
  (never the raw argv; inline-JSON `--mcp-config` / `--settings` are refused, not stored), written to a temp
  file, `sync_all`'d, renamed and read back. `kill_and_relaunch` refuses with `SpecNotWritten` when it
  cannot, and marks the spec `running` after a confirmed start, so a lane that later ends has a launch path.
  Invariant: a lane may be stopped only if it can be started again.
- **`lane-restart status [--json]`**: read-only; each role `running` / `suspended` / `crashed` /
  `never_run` / `unknown`, whether its spec is complete, whether it is startable. Liveness is the process
  table's (pid and start time), never a file's age.
- The `*.launch.json` files are skipped by `lane_state_writer::all_states`, `relaunch::models_report` and
  agentlife's `import::read_states` (they are not lane states).
- New public API: `lane_restart::spec`, `relaunch::kill_and_relaunch_persisting`,
  `RelaunchError::SpecNotWritten`. `kill_and_relaunch` and `kill_and_relaunch_with` keep their signatures.

## 0.12.9

- **Design: `RESTART-TOOL-DESIGN.md` section 14**, `lane-stop` / `lane-start` with `lane-restart` as the
  wrapper over them (PM task, CireSnave 2026-10-11). Proposal only: no code, nothing published. Acked by
  the PM 2026-10-11; the code PRs follow its section 14.13.

## 0.12.8

- **`persistant` 0.4.1 -> 0.4.4** (latest on crates.io; standing dependency rule). No code change.

## 0.12.7

- **CI: `actions/checkout` re-pinned to the current major** (v7.0.1, full SHA read from upstream). The
  other two pins (`dtolnay/rust-toolchain` stable, `cargo-deny-action` v2.1.1) were already the newest.
  `docs/PRE-PUBLISH.md` now records who owns `lane-state` on crates.io.

## 0.12.6

- **`docs/PRE-PUBLISH.md`**: the checklist for the unpublished crates (`lane-state`, `lane-restart`,
  `agentlife`): what must be true, the `cargo package` evidence, dependency order, version drift, and what
  only CireSnave decides. Nothing is published.

## 0.12.5

- **`agentlife doctor [--exe <path>]`** (R16, R17): read-only. Reports the installed `lane-restart` host
  (the configured `host_program`, resolved as a task would) and the `agentlife.exe` at the hook path
  (`C:/Projects/.claude-hooks/agentlife.exe`) with the version each reports against this build's, as OK,
  MISMATCH, MISSING or UNREADABLE; exits non-zero unless both are OK; installs nothing.

## 0.12.4

- **CI hardening.** A `deny` job (`cargo deny check advisories licenses bans sources`, `deny.toml`: a licence
  allow list read from `cargo deny list`, crates.io as the only source, wildcard requirements denied), and
  every action pinned to a full commit SHA read from its upstream repository.

## 0.12.3

- **The Hello prompt names who asked (#33).** `Asking` now carries the requester stored with the pending
  record, and the dialog shows that, not this process's: after a reboot the person saw the new pid while the
  grant kept the old one. `HelloPrompt::new()` no longer takes a requester.

## 0.12.2

- **`agentlife restore` runs.** Without `--dry-run` it freezes the plan, prints the agent list, asks the
  person (Windows Hello), and only if approved spends the one-use approval and starts the plan; the report
  goes to `<home>/reports`. New `agentlife::run` owns the order for `restore`, `pending approve` and
  `pending --prompt` (the last two run the plan at once on approval).
- **A lane cannot start agents:** an agent or unclear caller is refused (R10) before anything is stored,
  shown or asked. A person and the logon task pass.
- **The plan that runs is the plan that was approved:** it is rebuilt after the answer and before the
  spend; a different hash spends nothing and starts nothing.
- **A caveat, in `docs/CONSENT.md`:** the Hello text names only the plan hash, so a logon-task restore shows
  the person no agent list at the moment of approval.

## 0.12.1

- **The real consent backend.** `consent::installed()` returns `consent::real::UserRequestBackend`, over
  `user-request` 0.11.1 from crates.io (new dependency; it needs rust 1.95, which the workspace already
  does). It asks as agentlife, **not a registered lane** (role `agentlife`, empty session, own pid and start
  time), in the one store every user of `user-request` shares (`locate::dir()`, `head_copy()`, DPAPI),
  opened per call and dropped before a prompt is shown or a restore runs.
- `agentlife pending discard` now withdraws the request from that store. Nothing else a person sees
  changed: `pending approve` still says the prompt is not wired, and `restore` without `--dry-run` still
  refuses.
- **Fake corrected.** `RestorePlan` is `Scope::AnyRequester` in the real store (the plan hash is the
  binding), so the fake no longer narrows an approval by the requester's role, and the test that said
  another role "cannot spend" it now says the plan, not the spender, is what matches. Behaviour of
  `pending::spend_approval` against the real store is unchanged; only the fake's claim was wrong.
- New `consent::real::HelloPrompt` (`Prompt` over `user-request`'s Windows Hello channel). A channel
  `Refused` is `Unavailable`, which leaves the request pending. Not yet called by a command.
- Off Windows every consent call fails closed (the crate's DPAPI refuses), so nothing can be approved.

## 0.12.0

- **Breaking (library API):** the `Consent` trait takes the shape of `user-request` 0.11.1's store
  (RESTORE-GAP-ANALYSIS section 2, PR 3a). `spend_one_use(&mut self, kind, subject, &Requester)`
  returns `Result<String, String>`, the id spent; `Err` means nothing was spent and the restore must
  not run. A new `approved_at(kind, subject, &Requester)` reads when the approval was given, so
  freshness is still judged before the spend. `consent::Spent` and `consent::SpendError` are gone; a new
  `consent::Requester` carries who asks, supplied by the caller.
- `pending::spend_approval` takes a `&Requester`. A restore request's subject is now exactly
  `plan <64 lowercase hex>` (`consent::restore_plan_subject`, `parse_restore_plan_subject`), as the
  real crate requires; it used to be `restore N agents` (the count is still in the summary).
- The fake enforces the same: it refuses a restore-plan subject that is not `plan <hash>` or names
  another plan than the bound hash. (It also said the fake finds an approval by requester role; that
  was wrong for `RestorePlan`, a `Scope::AnyRequester` kind, and was retracted in 0.12.1: the plan hash
  is the binding, not who spends.)
- No real backend yet: `consent::installed()` still reports that none is installed, no dependency was
  added, and nothing a person sees or approves changed.

## 0.11.5

- New `agentlife import-lane-state [--write] [--json] [--since 48h] [--park a,b]`: seeds the registry
  from `.lane-state/<role>.json` (read only) so the first restore after a reboot does not have to
  wait for the hooks to fill it (RESTORE-GAP-ANALYSIS R3, R11, R12, R18). A dry run unless `--write`:
  it prints what it would write and what it skipped, and why.
  - Imported: files written within `--since` whose directory exists under the portfolio root and
    that recorded launch arguments; one record per name and directory (the newest file wins).
  - Skipped, each with its reason: too old, outside the root, missing directory, no launch
    arguments, test fixtures (`*restarttest*`), a name already in the registry, and files that are
    not lane states (`model-policy.json`).
  - The recorded `permission_mode` is carried as it was (so an `auto` lane comes back as `auto`);
    nothing is raised, and `restore` still refuses `bypassPermissions` for anyone but the PM.
  - The file's own session (pid and start time) is recorded, so a lane that is still running is
    seen as running, and one that died with the reboot is restored as a killed one.
  - `--park a,b` imports those agents as parked, so a restore leaves them down.
- `registry::Origin` gains `Imported` (`"imported"` in a record). A build older than 0.11.5 cannot
  read a record that carries it.

## 0.11.4

- A launched lane no longer inherits the spawner's `LANE_ROLE`. `lane-restart`'s role resolution
  lets `LANE_ROLE` win over the cwd leaf, so a lane started by a peer (the PM running
  `lane-restart --role x`, or `agentlife restore` from a lane's shell) used to write the
  spawner's role file instead of its own. `lane-restart`'s `spawn_launch` (both the `wt.exe` and
  the `conhost.exe` command) and agentlife's `RealSpawner` (via `TabLaunch.env_set`) now set
  `LANE_ROLE` to the launched role.

## 0.11.3

- `write_atomic` (used by the registry, the process index, pending restores, frozen plans and
  restore reports) now writes through `persistant` 0.4.1 (crates.io): its blocking `fs` store with
  a per-call scratch directory beside the target's directory, removed before the call returns, so
  an abandoned write cannot leave a temp file behind.
- Cost: each write starts persistant's worker thread and runtime. Measured at about 11 ms per
  write against about 4 ms for the previous temp-and-rename (release build, 1.5 KB files, 200
  writes per round, Windows). The most frequent caller, the registry, writes a few records per
  hook event, or about 20 in one fleet command, so no caller approaches one write per second.
- `is_temp_name` is kept only to skip `.<name>.tmp.<pid>.<n>` files the previous writer left
  beside their targets.
