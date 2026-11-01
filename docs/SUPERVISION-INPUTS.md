# Supervision inputs: what AgentLife knows today, for the generic supervision layer

Written 2026-10-31 by the agentlife lane, as the first input to the stack mapping; first drafted at `origin/main`
ff24e6a (0.29.51) and **refreshed the same day at 2125121 (0.29.63)**, after #202, #203, #205, #206 and #209-#212 merged
(section 2 now says merged where the first draft said "not merged", and has failures 9 to 11 added). **Facts only**: no
design, no recommendation, no refactor. Every statement is read from code, docs or files on this machine unless it says
UNVERIFIED (not measured, or reported to me and not re-read). Items that wait on an unmerged PR say so and name it.

CireSnave, verbatim, 2026-10-31: *"AgentLife is a project for starting and stopping Claude Code instances.  That's all it
is right now.  However, it doesn't have to stay just that.  AgentLife as a concept is a tool for controlling the
lifecycle of an agent.  It can start agents, stop agents, soft-stop agents, and restart agents.  Those functions are
common to almost any agent and not specific to Claude Code.  It seems like our Claude Code portions of AgentLife should
be moved into an AgentLife-ClaudeCode project and brought into AgentLife as one backend module it can support.  That
would allow AgentLife to also control agent lifecycle through OverMind, Ollama, etc. just by creating modules like
AgentLife-OverMind, AgentMind-Ollama, etc."* (As relayed by the PM; "AgentMind-Ollama" is as quoted.) The design note
(core vs module split, module interface and capability declaration, hooks and events, a migration that never breaks
`lane-restart`) is commissioned after the stack decision; **this note is not it**. Sections 1 and 3 are written so the
split can be read off them: what is lifecycle in general, and what is Claude Code in particular.

## 1. The state model as it is today

### 1.1 The unit and the files

The unit is a **role** (a plain identifier: `valid_identifier`). Everything is files under one **state directory**
(`C:\Projects\.lane-state` by default, `lane_restart::state_dir()`):

| File | Written by | Holds |
|---|---|---|
| `<role>.json` | the lane's own Claude Code hooks (`lane-restart state <event>`) | `LaneState`, schema-less JSON: `role`, `session_id`, `pid`, `pid_start_secs`, `cwd`, `name`, `model`, `permission_mode`, `remote_control`, `busy`, `subagents_running`, `no_background_shells`, `launch_args` (the running `claude`'s real argv, read at `SessionStart` from the OS process list), `updated_at`, `updated_by_event` |
| `<role>.launch.json` | `lane-restart` (start, stop, restart, `specs ...`, `lane-new`) | `SpecFile`, `schema` 1 (Windows) or 2 (`launch: wsl`): `role`, `name`, `cwd`, `model`, `permission_mode`, `remote_control`, `launch_args` (allowlisted extra flags only), `handoff_path`, `handoff_mtime`, `state`, `written_at`, `written_by`, `stopped_session_id`, `stopped_pid_start_secs`, `stop_reason`, `stop_kind`, `stop_mac`, `tool_version`, `launch`, `wsl` |
| `<role>.mcp.json` | `lane-new` (and by hand for older lanes) | the lane's MCP servers; the file named after `--mcp-config` |
| `<role>.start.lock` | `lane-restart start` | an OS advisory lock (no stale file: released when the holder exits) |
| `host-<role>-<pid>.log` | `lane-restart host` | the host's boot log; non-empty and newer than the start is how `lane-new` confirms a start |
| `restart.log` | every state-changing command | the ledger (JSON lines: time, `requested_by`, `role`, `outcome`, `acted`, `kind`, ...), read by `lane-restart ledger` |
| `<role>.launch.json.pre-*` | `specs ...` commands | one backup per kind, never overwritten (`pre-synapse`, `pre-both-channels`; `pre-soak` and others by hand) |
| `retired/` | `specs retire` | moved spec and state files |

Other files in the directory belong to other tools (logs, scratch, `roles-soak.txt`, `*.json` that are neither a state nor a
spec: `status` lists them under `skipped`, never drops them).

### 1.2 The states

Two vocabularies. **The spec's state** is recorded; **the lifecycle** is derived and never stored.

`SpecState` (recorded in `<role>.launch.json`):

| State | Meaning (code comment) |
|---|---|
| `running` | written at a confirmed launch: the lane was up with these parameters |
| `stopping` | written, complete and read back, immediately before the kill |
| `suspended` | the process is confirmed gone; the spec is the way back |
| `crashed` | imported from a lane state whose process was already gone (`specs import`) |
| `failed_relaunch` | a restart stopped the lane and the new session never came up; startable, a failure, not a stop |

`Lifecycle` (derived by `spec::role_status`, serialised in `status --json`, `schema: 1`):
`running | suspended | crashed | failed_relaunch | never_run | unknown`. The rule, in order:
1. **`running` iff the process table says so**: the recorded `pid` is a live `claude` AND its start time equals
   `pid_start_secs` (when recorded). Never a file's age (`state_process_is_live`).
2. A spec that cannot be read, with no live process: `unknown`.
3. Otherwise by the spec: `suspended` -> `suspended`; `running`/`stopping`/`crashed` with no live process ->
   `crashed`; `failed_relaunch` -> `failed_relaunch`.
4. No spec but a state file: `crashed` ("no launch spec: it is written at the next launch"); neither: `never_run`.
5. `startable` = the spec is complete (`SpecFile::check` after the model pin).

Beside the lifecycle `status` derives: `activity` (live roles only: `busy`, `subagents_running`,
`no_background_shells`, the one idle verdict `LaneState::is_idle`, `idle_blockers`, `quiet_secs`), `handoff` size against
its ceiling, `nested_in`, `duplicate_risk`, `channels` (which of `--mcp-config`, `server:claude-peers`, `server:synapse` the
LIVE session was launched with; in #202, merged), `stop_kind`, `stop_reason`, `model`.

**Idle** is defined in one place (`LaneState::is_idle`, PM ruling 2026-10-14): not busy, no subagents running, and the
lane has asserted no background shell is running. "Never asserted" is not idle. A fourth term the ruling names, "no unread
TASK", is not recorded anywhere and so is not part of it.

**Stop kind** (`soft` / `hard`, board 216): `soft` = the lane ended in the course of its own work, so a message may start
it again; `hard` = a person or the PM stopped it on purpose. A spec with no kind is *unclassified* and is never started by
the launcher. A soft stop is signed (`stop_mac`, `marker`); with no key it reads back unclassified. Only the PM or
CireSnave can turn a hard stop into anything else.

### 1.3 The transitions (who moves the state)

| Event | Command / actor | Spec written | Process |
|---|---|---|---|
| first launch | `lane-restart lane-new` (PM, `--apply --plan <hash>`) or hand-written spec | `suspended`, no `stop_kind` | then `lane-restart start` |
| start | `lane-restart start --role R --reason T` (PM; or the launcher service with a `--signal-id`) | `running` after liveness is confirmed | a `lane-restart host` in a new terminal tab hosts `claude` in a ConPTY |
| stop | `lane-restart stop --role R --reason T [--soft\|--hard] [--yes]` (PM for others; `--self` soft only) | `stopping` before the kill, `suspended` after | `claude` killed with its start time checked |
| restart | `lane-restart --role R` / `--self` | `stopping`, then `running`, or `failed_relaunch` | stop then start in one command; the spec is written and read back BEFORE the kill |
| crash | nothing writes it | none | the next `status` derives `crashed` |
| import | `specs import` (PM) | `crashed` (from a dead lane's state) | none |
| retire | `specs retire` (PM) | files moved to `retired/` | none |
| activity | Claude Code hooks (`state <event>`), `assert-idle` (the lane itself) | none | updates `<role>.json` only |

### 1.4 What in this is Claude-specific

Read from the code; the boundary I can draw from what the files and commands name:

- **Claude Code only**: `<role>.json` and the hook events that fill it (`SessionStart`, `PreToolUse`, `Stop`,
  `UserPromptSubmit`, `PostModelSwitch`, ...); `launch_args` as a `claude` argv and its allowlist (`extra_launch_args`:
  `--dangerously-load-development-channels`, `--mcp-config`, ...); the model rules (never Opus, `--model`); `permission_mode`
  and `remote_control`; the startup-window dialogs and their approvals; `HANDOFF.md` and the assert-idle protocol; the
  terminal host (ConPTY, `wt.exe`, `host-*.log`); `--name`/`claude`-named duplicate detection; the `wsl` launch mode (a
  Linux `claude` in a distro).
- **Not about Claude** (names a lane, a process, a lifecycle): the role identifier; `suspended`/`crashed`/`failed_relaunch`
  and `stop_kind`; the spec file as "the way back"; the ledger; the start lock; the process-table liveness rule
  (pid + start time); the double-launch guard's idea; `status --json`'s shape.
- **Mixed**: idle (defined from Claude hook data today); the stop (kills a `claude` after a wrap-up protocol); the
  launcher service (wakes a lane on a message).

UNVERIFIED: which of the "mixed" would carry over to an agent that has no hooks (OverMind states its agents have none).

## 2. Failure modes already paid for

Each: what happened, the rule it became, the test that holds it. "PR" = not yet on `main` when this was written.

1. **A restart overwrote the spec with the running session's flags.** 2026-10-31: after `specs both-channels` kiss's spec
   read both channels; kiss's own `lane-restart --self` rewrote it (`written_by=restart`, claude-peers only) and the new
   session had no Synapse tool. Cause: the spec to write AND launch from was built from `LaunchSpec::try_from(state)`, whose
   `launch_args` are the RUNNING argv; `lane_stop::build_stop_plan` did the same. **Rule:** a restart or stop keeps the
   launch flags of the complete spec already recorded (`SpecStore::existing`, `carry_spec_flags`), with the session's
   explicit `--model` kept. **Tests:** `stop::tests::a_restart_keeps_the_edited_specs_flags_in_the_spec_and_in_the_launch`,
   `lane_stop::tests::a_stop_keeps_the_edited_specs_launch_flags`, each with a control and a mutation. **Merged: agentlife#205**
   (0.29.54). It protects a lane only once that lane's restart runs the new exe; the running hosts held the Oct 22 build when
   written (UNVERIFIED whether that has since been installed). Until then the safe order is stop, then edit the suspended
   spec, then start (a `stop` rewrites the spec from the session, so the edit must follow it).
2. **A role got a second session while the first was stuck at a dialog.** 2026-10-14: a lane did `--self` at 09:03 and
   the PM ran `start` at 09:11 while the first host and `claude` were alive but at a startup dialog; the state file names a
   session only after `SessionStart`. **Rule:** the double-launch guard reads the process table only: a
   `lane-restart host --role R` process, or a `claude` whose argv carries `--name R` (or the spec's name); matched by
   command line, never by cwd. Surfaced as `duplicate_risk` in `status` and enforced in `start` (`duplicate::check`), plus
   the per-role start lock for two starters racing. **Tests** (`duplicate.rs`):
   `a_stuck_at_dialog_host_and_claude_are_both_found_with_no_state_file`,
   `the_equals_form_of_name_and_the_specs_own_name_both_match`,
   `a_restart_ignores_its_own_session_and_host_but_not_another_one`, `a_parent_cycle_does_not_hang_the_walk`.
3. **Stale state files.** A state file outlives its process, and a pid can be reused. **Rule:** liveness is the process
   table's: the recorded pid must be a live `claude` and its start time must equal `pid_start_secs`; a dead process
   reports no activity even if it died busy; a state file written before the field existed (`None`) falls back to pid
   alone. `launch_args` is "always fresh, never preserved" for the same reason (a stale argv is as wrong as an invented
   one). **Test:** `spec::tests::a_dead_process_reports_no_activity_even_if_it_died_busy` (and the status tests around it).
4. **The host's argv is fixed when the session starts.** `lane-restart host --role R -- <argv...>` runs inside the new
   terminal tab; its child argv is chosen at start, from the spec, and the running `claude`'s real argv is what
   `SessionStart` records in `launch_args`. So a later edit of the spec changes nothing for the running session; it takes
   effect at the next launch (which is also why failure 1 was invisible until the next restart). **Rule:** every `specs ...`
   command says "a running session is not touched", and `status` shows what the LIVE session actually carries (`channels=`).
   The PM's wording of this item ("the host argv baked in at start") matches; I did not read a separate incident for it.
5. **Dialogs awaiting a human.** A relaunched `claude` can stop at a startup dialog (the development-channels warning,
   trust). **Rule:** the host owns the ConPTY and, only during the startup window, checks the screen against every approval
   (`approvals`, loaded before the host starts `claude`; files live in the `ciresnave/ciresnave` repo under
   `.overmind/lane-restart/approvals/`) and injects the matching action; if the window ends on a dialog nothing answered it
   messages the notify role's lane. `start` exits `2` ("awaiting dialog") rather than `0`, and `lane-new` treats `0` and `2`
   as started. The launcher and `stop`/restart tools never answer a dialog themselves. **Tests:** the `lane_new_host`
   `StartAwaitingDialog` case, the `handlers` tests (an unchecked handler cannot be constructed: audit group 5), the
   duplicate tests above. UNVERIFIED: how often a lane lands on an unanswered dialog now; not measured.
6. **`lane-new`'s `start` refused by the caller check.** The PM's first real run, 2026-10-31 ~11:15 local
   (`lane-restart.exe lane-new surfaceant surfaceant --apply --plan 359152881bbbdf92`, Git Bash tool): steps 0-6 ok,
   then `step 7 start FAIL lane-restart start exited 1: lane-restart start: refused - only the PM session may start a
   lane, and the caller could not be verified: this command is not running under a claude session`. The same `start`
   typed by hand with the installed Oct 22 exe worked; `specs both-channels` (the new exe) passed its PM check in the same
   session. **Cause (read from the code; the unit chain reproduces it, a real `lane-new --apply` was not rerun by me):**
   `lane-new --apply` is PM-only **by process** (`caller_role`: the nearest `claude` ancestor, found by
   `claude_parent_pid`, whose state file says `pm`); step 7 runs `std::env::current_exe() start ...` (the NEW exe, so the
   installed exe was not involved) as a CHILD of the `lane-restart` running `lane-new`; `claude_parent_pid` skipped only
   known shells, so the child's first ancestor, `lane-restart.exe`, was refused as a stranger before the walk reached the
   `claude` above. **Rule:** the walk also passes through this tool's own image; the answer is still the nearest
   `claude`, so no identity is borrowed. **Test:** `claude_proc::tests::walks_past_the_tools_own_parent_process_to_the_claude_above_it`
   (the exact chain, a stranger, a `lane-restart` with no `claude` above it, and two `claude` above). **Merged: agentlife#206**
   (0.29.55). The end-to-end check, a real `lane-new --apply` on a throwaway role with an exe that has it, is the PM's and is
   UNVERIFIED here.
7. **A CI-only race in a test, 2026-10-31:** a fake host log written within milliseconds of `since = now()` read older
   than `since` on Linux (coarse file clock). **Rule:** a test stamps its file clearly after; nothing else in the repo has
   the zero-margin shape except `lane_new.rs` (production, seconds apart in real use). **PR:** merged as #201.
8. **Inconsistent push channels.** Measured 2026-10-31 from `claude.exe` command lines: 3 lanes loaded only Synapse, 10 only
   claude-peers, 14 both. **Rule (CireSnave):** every lane on Synapse; `specs both-channels` (merged, #202) as the
   transition, `check_synapse_only` and a Synapse-only `lane-new` spec as the end state (merged, #203). **Related:** the
   restore planner dropped `server:synapse` and `--mcp-config` from every plan (fixed in #203 with a test that restores a
   Synapse-only spec). The PM applies the end state to recorded specs with `specs synapse-only`, after every lane answers a
   PING on Synapse (UNVERIFIED here whether that has happened).
9. **Tests that read the machine's own state directory, 2026-10-31.** Since #205 the real spec store returns a recorded
   spec, so a `relaunch` test that named the real lane `overmind` and called the real `kill_and_relaunch` failed on any box
   that runs the lane (green on clean CI). **Rule:** such tests run through a temporary state directory and name a role no
   lane has; a guard test fails if one names the real restart or a role that has a spec or state in the real directory
   (covers `relaunch.rs` only; other modules use temporary directories). **Merged: #209.** The same PR fixes a race in
   `host::tests::a_folder_trust_answer_...` (a read of an outbox `.tmp` that was renamed a moment later). **Still open
   (UNVERIFIED cause):** `host::tests::no_helper_thread_outlives_a_host_run` failed about 2 of 12 full-suite runs on one
   box with the PTY reader still blocked after the host's fixed 3 s wait; it did not fail on CI in the runs seen.
10. **An approval the program built itself, 2026-10-31.** `user-request` 0.16 made `Approval` non-exhaustive (an approval can
    only come from its channel), which exposed that the restore-consent backend built one after the person answered.
    **Rule:** the only approval recorded is the channel's own, held as a non-`Clone` `Proof` that is moved into `resolve`;
    an `Approved` without one, or for another kind/subject/requester than the held request, releases the reservation and
    fails. 0.16 also changed the store's protector off Windows to a key file beside the store; the backend refuses off
    Windows instead (`refuse_off_windows`), the 0.11.1 behaviour, on the PM's ruling. **Merged: #210** (tests with controls,
    three `compile_fail` doctests, two mutations). The follow-up, `Answer::from_channel` taking the channel's outcome by
    value so no second copy exists, is **merged: #214**.
11. **Dependency bumps that change what "cannot happen" means.** `sysinfo` 0.40 made `System::new()` and
    `System::boot_time()` fallible; each of 19 call sites now answers as it would for a process it cannot find (liveness
    false, identity none, a kill refused, a shell check unsafe), never as "fine". **Merged: #208.**
12. **A restore that said "no session appeared" while both hooks had exited 0 (open defect, cause UNVERIFIED).** On main
    `5c62f44` (#213) the Windows leg failed attempt 1 and passed attempt 2 on the same commit: in
    `tests/launch_e2e.rs restore_now_asks_spends_and_starts_the_real_stand_ins_...` alpha was `Failed("no session appeared
    within 40 s")` and beta `AwaitingDialog`, although both stand-in hooks exited 0 (36 ms and 161 ms, stderr empty). Not
    reproduced locally (6 runs, then 3 rounds of 12 concurrent). Read from the code: `hook::session_start` returns a skip
    with no write and no stderr in three cases (not under a claude process, headless, another claude above), logging only
    to `hook.log`; one candidate is an empty process snapshot (`sysinfo` returns 0 processes, without an error, if the OS
    snapshot call fails). **State:** logged in `docs/ROADMAP.md`; **merged: #215** (the failing test prints `hook.log` and
    the agent records) and **#216** (a hook retries an empty snapshot; `restore` reports a visible hook skip at once). No
    repeat has been seen since, so the cause stays UNVERIFIED. **Input to the layer:** a supervisor must not infer "never started" from the absence of a
    record when the component that writes it can decline silently; the decline needs to be readable by the supervisor.

## 3. What a generic layer could take unchanged, and what stays Claude-specific

"Unchanged" means: the file or function does not name Claude Code in its data or logic. This is my reading of the code,
not a design.

**Could be taken as it is**
- The role identifier and its validity rule (`valid_identifier`).
- `Lifecycle` and `SpecStatus` as the lifecycle vocabulary, and the **derivation rule** in 1.2 with "live" supplied by the
  adapter (today: pid + start time from the process table).
- `StopKind` (soft/hard) and its rules, including the signed soft marker.
- The ledger format (`restart.log` JSON lines) and `ledger` filters.
- The per-role start lock (OS advisory lock).
- The atomic spec write (`write_atomic`: complete, temp file, `sync_all`, rename, read back and compare) and the ordering
  invariant "the spec is on disk and complete before the kill" (a lane may be stopped only if it can be started again).
- `status --json` (`schema: 1`) as an output contract, and the idea of listing unknown files under `skipped`.
- The disk-floor guard (`disk::guard`, 20 GB rule) and the "never Opus" model type (policy, but not about Claude Code's
  mechanics; the model names are).

**Must stay Claude-specific (the adapter)**
- Hooks and `<role>.json`; the `claude` argv allowlist and `launch_args`; the ConPTY host, the startup-window approvals and
  handlers; `HANDOFF.md`, `assert-idle`, the wrap-up protocol; `--name`/host detection; `permission_mode`,
  `remote_control`; the `wsl` launch mode; the `.claude-hooks` install and the Claude settings entries.

**Unclear without a decision from the new lane**
- Idle (hook-derived today). OverMind states its agents have no hooks and that the parent can derive idle as time since
  the last event.
- `activity` and the `busy`/`subagents_running` fields.
- The HANDOFF requirement on stop (a protocol between a lane and a person, not a mechanism).
- The launcher service's wake-on-message (see 4).
- Anything about **remote** agents: UNVERIFIED, nothing in this repo has run against one. The process-table liveness rule
  has no meaning for a process on another machine.

## 4. What depends on AgentLife's commands and files today

Do-not-break list. The state directory path and these file names are the contract; most are read by tools in other repos.
I can name the consumers I read in this repo; for other repos, the claim is UNVERIFIED unless a document here says it (I
may not search other repos).

| Interface | Used by (read in this repo) | Notes |
|---|---|---|
| `C:\Projects\.claude-hooks\lane-restart.exe` | every lane's Claude settings (hooks: `state <event>`, `assert-idle`) and every running `lane-restart host` | the Oct 22 build (2.4 MB) is what 27 running hosts hold open; Windows will not overwrite it while they run. New builds are run from a scratch path or installed beside it (`lane-restart install --from` is the PM's) |
| `lane-restart status [--json]` (`schema: 1`; the disk line on stderr) | the PM's sweeps (per the PM; UNVERIFIED here), `agentlife status`, humans | fields only get added; an unknown field must be ignorable |
| `lane-restart start --role R ... --json` (exit 0, 2 = awaiting dialog, other = failure; last JSON line) | the **launcher service** `agentlife-launcher` (`LaneRestartMechanism` runs it as a direct child; `map_start_json` reads the last JSON line); `lane-new` step 7 | the caller identity `launcher_gate` accepts is the service itself (`start::LAUNCHER_ROLE`); `overmind` was accepted as a start-only caller with a required `--signal-id` (0.15.2, 0.15.4) |
| `lane-restart stop`, `lane-restart --self`, `assert-idle` | the lanes themselves and the PM; `agentlife park`/`stop` (`src/down.rs`) read the spec with `lane_restart::spec::read` and write it back `suspended` themselves | the HANDOFF gate and the idle gate live here |
| `<role>.launch.json` (schema 1, 2) | `lane-restart` (all), `agentlife restore`'s planner (`src/spec_launch.rs`, via `plan::rebuild`'s allowlist), `agentlife down` | spec first, registry as a flagged fallback; a spec that records a stop (`suspended`/`stopping`) excludes the lane from restore even when incomplete |
| `<role>.json` (state) | `status`, the authorize gate (`authorize.rs`), `lane_state_writer`, `peers_join` | written only by the hooks |
| `<role>.mcp.json` | Claude Code via `--mcp-config`; `specs both-channels`/`synapse-only` read it to check it names `synapse` | |
| `restart.log` | `lane-restart ledger`, the PM | append-only |
| `roles-soak.txt`, `scripts/synapse-provision-weekly.ps1` | `lane-new` step 5/6 (Synapse identity), the weekly provisioning | |
| `lane_restart` and `lane-state` library crates | the `agentlife` binary (`src/*`: `disk`, `spec`, `notify`, `start`, `paths`, `relaunch`) | one version number for the whole workspace (CireSnave's rule); a breaking change is the second number pre-1.0 |
| Approvals in `ciresnave/ciresnave` `.overmind/lane-restart/approvals/*.json` | the host, at start | the embedded set is empty by design (RESTART-TOOL-DESIGN.md 12.2) |

Documents that already describe pieces of this: `RESTART-TOOL-DESIGN.md` (the restart tool and the spec, 14.x),
`docs/LANE-NEW.md`, `docs/LAUNCHER-SERVICE.md`, `docs/LANE-RESTART-WSL.md`, `docs/SYNAPSE-ONLY-CUTOVER.md`,
`docs/OVERMIND-EXTRACTION-NOTE.md`, `docs/GRACEFUL-STOP.md`.

## 5. Not measured, said once

- The per-call cost of the Claude Code state hook (`lane-restart state <event>` runs once per tool call). I have not
  measured it; running it on a live lane would write that lane's state file.
- Anything about remote or non-Claude agents in this code base: there is none.
- Whether `lane-restart`'s dialog handling is exercised in practice today (see failure 5).
- Failure 6 end to end: the PM's rerun of `lane-new --apply` on a throwaway role after #206 merges is the real check.
- Any other lane's lifecycle for an agent that is not Claude Code. The Synapse lane states (2026-10-31, from their code):
  roles authenticate by certificate chain and a signed claim, a session token is bound to one role, presence comes from
  heartbeat, Synapse stores no events (only counters and timestamps per role), the daemon is loopback-only with no remote
  transport yet. OverMind states (from its code, not measured live) that its non-Claude agents are one-shot tasks with no
  heartbeat and no hooks, and that its sandboxed child cannot open sockets but has a protocol channel to its parent.
