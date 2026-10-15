# Changelog

One version for the whole workspace (`agentlife`, `lane-restart`, `lane-state`).

## 0.15.4

- **The launcher caller is `agentlife-launcher`, not `overmind`** (PM ruling 2026-10-15). Synapse holds one
  session per role and a claim supersedes the other holder, so a service claiming `overmind` would knock the
  OverMind lane off its role. `lane-restart start` now accepts caller role `agentlife-launcher` (record
  `<state>/agentlife-launcher.launcher-caller`); the 0.15.2 name `overmind` and its record are **accepted for
  one more release** and removed after 0.16. A record must be filed under the name it carries. `stop` and a
  restart still refuse both. The caller is recorded as `launcher:<role>` (the `:` cannot appear in a lane's
  role), so a lane that shares the name, such as the OverMind lane, is not mistaken for the service (a
  hole in 0.15.2's gate, found in review of this change). `LAUNCHER_RECORD` stays as a deprecated alias.
  Threat note updated. No exit code or output changes; not run live (the service
  that writes the record does not exist yet).

## 0.15.3

- **`agentlife restore` no longer relaunches a lane that was stopped on purpose.** A `wanted` lane whose launch
  spec is `suspended` (written by `lane-restart stop` and `agentlife down`) is excluded from the plan with reason
  `SuspendedSpec` (routine, counted not listed); `lane-restart start` stays the resume path. Before, only
  `agentlife down` recorded `Closed` intent, so a `lane-restart stop`ped lane was `Wanted`, not running, and a
  restore candidate (found by the launcher design review, #92). A suspended spec that is not complete is also
  reported as suspended rather than falling back to the registry record, as is a `stopping` spec (the final
  `suspended` write failed) and a spec file that only partly reads but says `suspended`; another role's spec
  under a filename is not used. Not run against a real logon; the planner and loader are unit-tested, with the
  exclusion mutated out to see the test fail. **Residual, stated:** restore does not re-read the spec between
  building the plan and spawning, so a stop landing inside that window is still undone (not tested, not closed
  here); a pending restore frozen by 0.15.3 and read by an older exe would not parse the new `SuspendedSpec`
  reason (review finding, not run).

## Unreleased

- **Docs: `docs/LAUNCHER-SERVICE.md`, the design of the launch-on-message service** (`agentlife launcher`; PM task
  2026-10-15). Names what blocks it: `synapsed-client` is a `0.0.0` name reservation on crates.io (no code), the
  M5 grant store and board 216 are unbuilt, and `start-role` on the launcher path does not exist. Nothing built.

## 0.15.2

- **`lane-restart start` accepts a second caller, role `overmind`, for the launch-on-message service** (PM task
  2026-10-15; authority: CireSnave "Synapse->OverMind->AgentLife launch", relayed by the PM). Identified by a
  record `<state>/overmind.launcher-caller` plus the direct parent's pid and start time, not by a `claude`
  ancestor; `start` only (`stop` and a restart still require the PM, now tested with `overmind` refused); requires
  `--signal-id <role>:<id>` naming the role started, else `refused: no-signal`; each call ledgered with the signal
  id. Threat note: `docs/LAUNCH-ON-MESSAGE-THREAT.md`. No service writes the record yet (PR3); not run live.
  **Stated limit:** the record is a same-user-forgeable local mirror of the Synapse `launcher_roles=overmind`
  credential, which `lane-restart` cannot check; the gate bounds what that identity may do (start existing,
  specced lanes only), not who can claim it. The rate cap lives in the service, not here.

## 0.15.1

- **`lane-restart start --json`** prints one line on stdout, `{"role","outcome","reason"[,"launch_to_live_secs"]}`,
  for a caller that must decide whether to retry (Synapse's `/v1/launch-result`, spec C4). `outcome` is
  `started` (reason `started` or `awaiting-dialog`), `already-running`, `refused` (terminal: `not-pm`, `pm-role`,
  `invalid-role`, `reason`, `model-not-pinned`, `no-spec`, `spec-unusable`) or `failed` (retryable:
  `duplicate-risk`, `process-table-unreadable`, `disk-floor`, `spawn-failed`); `dry-run` for `--dry-run`. Exit codes
  are unchanged (0/1/2/3). A confirmed start also records `launch-to-live:<n>s` (spawn to confirmed progress) in
  the ledger line and the JSON, so a measured `launch_wait` can accrue from real starts. Not run against a real
  lane; the outcome mapping is unit-tested.

## 0.15.0

- **BREAKING (exit code): `lane-restart start` exits 3 when the lane is already running.** Before, that refusal
  exited 1 like every other (no PM caller, no spec, a duplicate-risk process, a failed launch), so a caller that
  only wants "make sure this lane is up" could not tell a harmless no-op from a failure. Now: `0` started with
  progress confirmed, `2` started and awaiting the dev-channels dialog, `3` already running (a live claude matches
  the recorded pid and start time; nothing is launched or written, safe to call again), `1` any other refusal or a
  failed launch. `stop` keeps `4` (stopped, `suspended` mark not written). A `duplicate-risk` refusal stays `1`: a
  stuck or unrecorded session is not a confirmed live lane. Also in `RESTART-TOOL-DESIGN.md` 14.5. Not run against
  a real lane (`start` is PM-only); the mapping is unit-tested (`StartRefusal::exit_code`).

## 0.14.35

- **`agentlife install-task` off Windows prints a `systemd --user` logon unit** (`docs/PORT-LINUX-PLAN.md`, P8).
  Before, it refused outright (Task Scheduler exists only on Windows). Now it prints
  `agentlife-logon.service` (`Type=oneshot`, `ExecStart=<exe> restore --from-logon`, `WantedBy=default.target`, no
  start timeout because a restore waits for a person; `%` and `$` doubled and the path quoted when needed) and the
  `systemctl --user daemon-reload` / `enable` commands. **There is no unlock unit**: systemd has no trigger for a
  session unlock (logind announces it as a D-Bus `Unlock` signal that a listener would have to catch), so on Linux
  `agentlife pending --prompt` is run by hand or from the desktop's own unlock hook; this is printed, not hidden.
  `--register`/`--remove` stay Windows-only: nothing is written into the user's systemd directory and no
  `systemctl` runs. **Not verified against a real systemd.** The Windows path is unchanged.

## 0.14.34

- **`lane-restart attach --role <r> --watch`: a read-only view of a running direct lane host** (`docs/PORT-LINUX-PLAN.md`,
  P6b; approved by the PM 2026-10-14 as the only attach to build). The host serves a unix-domain socket
  `<state>/watch/host-<role>.sock` (the directory is created `0700` before the socket is bound in it). A watcher is
  given the current screen (the host's `vt100` model, as of the chunks relayed so far) and then every chunk the child
  writes, in order. **Nothing flows the other way**: the server has no read path, so the host's own stdin stays the
  only way into the lane, and a watcher's bytes are ignored. A watcher that falls behind (256 chunks) is dropped; the
  relay never waits for one. `--watch` is required: a bare `attach` is refused, because interactive attach is not
  built and not approved (plan row P6c, open: a threat-model note on keeping a same-user agent off the socket is
  owed). Unix only; the host's behaviour with no watcher is what it was, and Windows is unchanged.

## 0.14.33

- **A lane starts as a direct process off Windows** (`docs/PORT-LINUX-PLAN.md`, P6a; CireSnave 2026-10-14, verbatim:
  "Direct process first with tmux support on our roadmap to build later."). No terminal program, no tmux, no
  emulator. Windows is unchanged (`wt.exe`, then `conhost.exe`).
  - `lane-restart` (`start`, restart, `--self`): `relaunch::spawn_launch` runs the host itself through the pure
    `relaunch::direct_command`: the lane's directory, `LANE_ROLE` set to the launched role, the session identity
    removed, stdin the null device, stdout and stderr appended to `host-<role>.out` in the state directory.
  - `agentlife restore`: new `launch::DirectSpawner` and `launch::platform_spawner()` (Windows: `RealSpawner`,
    unchanged); `SpawnVia::Direct` is now the real direct launch, not only a test double.
  - The host makes itself a session leader (`setsid`) when its stdin is not a tty, so a launcher's terminal closing
    does not take every lane it started. A host a person runs in a terminal is left alone.
  - Not in this change: **attach**. Ruled by the PM 2026-10-14: read-only `--watch` is P6b; interactive attach is
    not approved until a threat-model note says how a same-user agent is kept off the socket.

## 0.14.32

- **Unix stop is TERM, grace, KILL to the process group** (`docs/PORT-LINUX-PLAN.md`, P5). `sysinfo`'s `kill()` is
  `SIGKILL` on Linux: no flush, and a lane's shells and helpers were left running. New
  `lane_state::platform::signal::terminate` (unix): `SIGTERM` to the target's process group when it leads one (a
  session leader on a pty does), else to the process alone; wait up to 5 s (`facts::KILL_GRACE`); then `SIGKILL`
  **only if the same process is still there**, asked again immediately before the signal (a pid that changed
  hands during the grace is never signalled; a zombie counts as gone). Used by `SysinfoFacts::kill_verified`
  (`lane-restart` restart and stop, `lane_stop`) and by `agentlife down`. **Behaviour change off Windows:** a stop
  can now take up to 5 s longer when the process ignores `SIGTERM`. Windows is unchanged (one `TerminateProcess`).
  New unix-only dependency `rustix` features `process` in `lane-state`.
- Docs: P9 is recorded as **not verified against a real systemd journal** in the plan and the ROADMAP.

## 0.14.31

- **Shutdown marker on Linux** (`docs/PORT-LINUX-PLAN.md`, P9). The reboot-vs-crash classifier (`intent`) had a
  marker only on Windows (`wevtutil`, events 1074/6006); elsewhere every ambiguous case became a candidate. On Linux
  it now reads the previous boot's closing journal entries (`journalctl --boot=-1 --output=short-unix --reverse`) and
  takes the newest `Shutting down.`, `systemd-shutdown` or `Journal stopped` line at or before this boot began. A
  crash or power cut leaves none, so it reads as no marker, and so does a journal that is not persistent
  (`journalctl` exits non-zero) or a host without systemd: the safe default is unchanged. `pick_journal_shutdown`
  is pure and tested on both OSes. **Not verified against a real journal** (the only Linux available here has no
  systemd); the ubuntu CI leg runs the parser, not `journalctl`.

## 0.14.30

- docs only (PM-allocated). `docs/PORT-LINUX-PLAN.md`: updated for the Q1 (direct host first, tmux later, no
  emulator), Q2 and Q3 rulings; P3 replaced by P3' and P11 marked already built; **the key-file tier is corrected
  from "parity with DPAPI user scope" to weaker than it** (its key sits beside the store and is copied with it;
  from OverMind's audit in `consent` `docs/LINUX-PORT-DESIGN.md`), and a security-key touch is noted as presence,
  not reading. `docs/ROADMAP.md`: a Linux port section with the tmux-viewer row, marked later. No code change.

## 0.14.29

- **Identity-mismatch diagnostic, no change to any decision** (`docs/PORT-LINUX-PLAN.md`, P3'; issue #14 has no
  failing log). New `lane_state::platform::diag::start_readings(pid)`: on Linux, the `/proc/<pid>/stat` start tick
  (boot-relative, so a stepped clock cannot move it), the `btime` `/proc/stat` reports now, and the epoch start they
  give together; off Linux it says it is unavailable. It is appended to the message when a kill is refused because
  the pid's identity no longer matches: `agentlife down` (`DownError::IdentityChanged`), and `lane-restart` restart
  and `stop` (`action failed: ... kill refused: ... (start readings: ...)`). A later clock step then shows in the
  ledger as `proc_start_ticks` stable while `btime` moved. The token redesign (old P3) stays out unless a log shows
  that.

## 0.14.28

- **`lane-restart host` on unix** (`docs/PORT-LINUX-PLAN.md`, slice P4). No change on Windows.
  - A raw-mode guard on the host's stdin (`rustix::termios`): a real tty goes raw (no line buffering or echo, so
    the terminal's cursor-position reply reaches the child) and its original mode is restored on every exit path,
    panic unwind included. A pipe, a file or a test's fake stdin is left alone. This is the unix counterpart of the
    Windows `ENABLE_VIRTUAL_TERMINAL_INPUT` fix.
  - `current_console_size()` reads the tty size (`TIOCGWINSZ`) instead of returning `None`, so the PTY is no longer
    always 80x25 on Linux and resize forwarding fires (the existing 500 ms poll; no SIGWINCH handler yet).
  - The 11 real-PTY host tests that were `cfg(windows)` now run on both OSes with an `sh -c` child on unix; three new
    tests exercise the guard and the size against a real pty. New dependency: `rustix` 1.1.5 (`termios`, `stdio`),
    unix only.

## 0.14.27

- **Linux port, slices P0 to P2** (`docs/PORT-LINUX-PLAN.md`, merged in #78 without a bump; PM-allocated number).
  **No behaviour change on Windows**: every Windows value is the literal it was.
  - **P0, fixed directories per OS.** New `lane_state::platform` (`state_dir`, `agentlife_home`, `hooks_dir`,
    `portfolio_root`, with fallible `try_*` forms and a testable `Env`). Off Windows they resolve under
    `XDG_STATE_HOME` (else `~/.local/state`): `lane-state/`, `agentlife/`; hooks under `~/.local/lib/agentlife/hooks`;
    portfolio root `~/Projects`. Before, off Windows `C:/Projects/.lane-state` was a *relative* directory created under
    the cwd. `lane_restart::state_dir()` and the two other copies in `host.rs` now use it; `Config::resolve`
    re-resolves `portfolio_root` and `lane_state_dir` for this OS (`Config::default()` stays the Windows layout);
    `Home::resolve`, `doctor` (`doctor::hook_exe()`), `worktrees --stale` and `install` use it too. The agentlife
    home is a sibling of the state directory, not inside it (a small change from the plan's wording).
  - **P1, naming.** `windows-sys` moves under `[target.'cfg(windows)'.dependencies]`; `install` names the installed
    file `lane-restart` (and `.bak`) off Windows. User-facing `wt.exe` text is left as is: it is accurate until a
    Linux launcher exists (P6).
  - **P2, one set of process names.** New `lane_state::platform::names` (`image_base`, `is_claude`, `is_shell`,
    `SHELLS`) replaces five private copies. **Behaviour change off Windows only:** `zsh`, `fish`, `dash` and `ksh`
    are accepted as shells above `claude` (a zsh user had every hook refused with `UnexpectedAncestor`), and count as
    "a live shell" below it. What Claude Code's process is called on Linux is still unmeasured, so `is_claude`
    accepts exactly `claude` / `claude.exe`.
  - Not changed, on purpose: `tab_close` shell and terminal lists (destructive, Windows-only until P10), `hook.rs`
    `TERMINALS` (P10), `pin-plan --root` default (a standalone crate with no dependency on `lane-state`).

## 0.14.26

- Double-launch guard (PM task 2026-10-14; the UniSQL case: a lane did `--self` at 09:03, the PM ran `start --role
  unisql` at 09:11 while the first host and claude were alive but stuck at a startup dialog, and no state file named
  either). `lane-restart start` and the restart / `--self` path now REFUSE while the process table holds a
  `lane-restart host --role <r>` or a claude whose launch arguments carry `--name <r>` (or the spec's own name),
  even when the state file says crashed, stale or absent; the pids are printed. A restart ignores only its own
  session and that session's host. An unreadable process table also refuses. `status` marks such a role
  `duplicate-risk: host pid N, claude pid M` (`duplicate_risk` in `--json`, absent for a normal row) and clears its
  `startable`. There is no override flag: close the stuck session, then retry. `lane_state::facts::ProcEntry` gains
  `cmd` (the full command line). The normal restart path is unchanged.

## 0.14.25

- docs/CI only (PM-allocated, portfolio hygiene from the CodeRipper triage 2026-10-14): `.github/CODEOWNERS`
  (`* @ciresnave`, WSP-001) and `docs/GLOSSARY.md` (DOC-009). No code change.

## 0.14.24

- `agentlife worktrees --stale [--json] [--root <dir> | --repo <path>...]` (PM task 2026-10-14, ROADMAP slice 3
  row (a)4): which registered worktrees belong to a merged branch, by the `CLAUDE.md` section 3/4 predicate: a MERGED
  PR whose head is the worktree's own HEAD, no open PR, `git ls-remote --heads origin <branch>` empty, and a clean
  tree. It prints the `git worktree remove` lines for the stale ones and **never removes anything**. Uncommitted
  files are `stale-dirty` (ask first), a branch reused after its merge is `merged-then-diverged`, a failed lookup is
  `unknown`; none of those is ever listed as stale. Worktrees come from `git worktree list --porcelain` per repo
  (main worktrees excluded); repos are the `--repo` paths, else the children of `--root` (default `C:/Projects`)
  that have a `.git` directory. Needs `git` and `gh`.

## 0.14.23

- `lane-restart status --peers` (PM ruling 2026-10-14, ROADMAP slice 3 row (a)3; `docs/PROPOSAL-STATUS-PEERS-JOIN.md`):
  per role, the claude-peers sessions whose MCP helper's **parent process** is the role's live `claude` (never joined
  by cwd, never by peer id), then the peers no role's live `claude` owns, with the cwd. `--json` gains `peers` per
  role, `peers_unmatched` and `peers_error`, all absent without the flag. A broker that cannot be read is
  `"unknown"` for every role, never an empty list. A peer's `summary` is printed (one line, control characters
  dropped, cut at 100 characters) only under `--peers`. Read-only: it sends nothing. `notify::Peer` gains
  `pid`, `summary` and `last_seen` (all defaulted) and `notify::list_all`; the notify send path is unchanged.

## 0.14.22

- docs only: `docs/PROPOSAL-STATUS-PEERS-JOIN.md`, the proposal for `lane-restart status --peers` (ROADMAP slice 3
  (a)3). No code change.

## 0.14.21

- `lane-restart install --from <exe> [--dry-run]` (PM only, even as a dry run; PM ruling 2026-10-14, ROADMAP slice 3
  row (a)2): installing a new `lane-restart.exe` was a hand ritual that left `.claude-hooks` with 13 `lane-restart*`
  files and a `lane-restart.sha256` that no longer described the exe. The candidate must run `--version` (first line
  `lane-restart x.y.z`) and `status` (exit 0) before anything changes. The installed exe is then renamed to the one
  `lane-restart.exe.bak` (an earlier `.bak` is replaced, so there is only ever one), the candidate is copied in and
  read back byte for byte, and `lane-restart.sha256` is rewritten (`<hex> *lane-restart.exe`) and read back. A
  failure after the rename puts the old exe back and says whether that worked. A ledger row of kind `install` is
  written for every attempt, including a refused non-PM caller. The directory is fixed
  (`C:/Projects/.claude-hooks`), like the state directory. It does not delete the older `.prev`/`.old*` files: those
  are the PM's to clear.

## 0.14.20

- `lane-restart --self` needs no `--role` (PM ruling 2026-10-14): the documented bare form in `CLAUDE.md` and
  `CIRESNAVE-EXPECTATIONS.md` 2.3b exited 1 with `--role <name> is required`. The role is `LANE_ROLE`, else the one
  role whose lane state records the current directory (case and separator insensitive); several matches are
  narrowed by the caller's claude pid, and if that does not leave exactly one the command refuses and names the
  candidates. An explicit `--role` is used as given, and every existing check on `--self` (the state file's pid
  must be the caller's claude) still runs on the result. Without `--self` the role is still required.

## 0.14.19

- docs only: `docs/ROADMAP.md` slice 3 (what the PM and lanes still do by hand around restart/restore/status, and
  where OverMind-split steps 6 and 7 stand, measured at OverMind `ca9e208` and consent `62b1788`). No code change.

## 0.14.18

- `status --quiet-over <secs>` (opt-in; no default, nothing enforced): marks each **live** role whose last hook
  write is older than `<secs>` as `QUIET>Ns` in text, and sets `activity.quiet_over` (and the report's
  `quiet_over_secs`) in `--json`. Without the flag the output is byte-for-byte what 0.14.17 printed. The PM's
  recommended value is **120**, a recommendation and not a default: a lane inside one long tool call is also
  quiet, so the mark is a prompt to look, not a stall verdict.

## 0.14.17

- `status` and `specs import` label a role whose recorded cwd is inside another role's cwd: `status` text
  `nested-in=<role>` and `nested_in` in JSON; an import row says `nested in <role>`. Report only: it changes
  neither `startable` nor what import writes, because a worktree under its repo is a legitimate nested lane.
  Why: a session that `cd`-ed into a sub-directory records the leaf name as its role (nine such roles on
  2026-10-14). Comparison is case-insensitive, `/` and `\` alike; equal cwds are not nesting.

## 0.14.16

- `status`: a role that is not idle says why: `idle=false(busy+shells_never_asserted) quiet=NNNs` and
  `activity.idle_blockers` in JSON (PM ruling 2026-10-14: a lane that never asserted shell-free is not idle).
  `quiet_secs` is the age of the lane's last hook write; no threshold is applied, the number is the claim.
- `status`: HANDOFF size per role against its ceiling (`handoff`: path, bytes, ceiling, over; text
  `handoff=NB/CB [OVER]`, `handoff=none` when no cwd or no file). Read-only, one `stat` per role, the same file
  `assert-idle` lints.
- `ledger --by-lane`: acted and refused counts per target role over the rows the other filters keep.

## 0.14.15

- `status` and `status --json` carry each **live** role's recorded activity (`activity`: `busy`,
  `subagents_running`, `no_background_shells`, `updated_at`, `updated_by_event`, `idle`). A role whose
  process is not live has no `activity` key, so a lane killed mid-turn cannot read as busy. Additive: the
  schema number is unchanged.
- One named idle predicate, `LaneState::is_idle` (PM ruling 2026-10-14: not busy, no subagents, no background
  shells asserted). `status` prints it and the restart gate asserts it, so the two cannot disagree. The
  ruling's fourth term, "no unread TASK", is not recorded in any state file, so it is **not** part of
  `idle`; a caller that needs it must read it elsewhere.

## 0.14.14

- Docs only: `docs/ROADMAP.md` gains "Next slice 2" (what the PM's sweep still does by hand: activity fields in
  `status --json`, one named idle predicate, HANDOFF size per role, a staleness mark, a ledger summary) and the
  measured breakdown of the 30 roles with no launch spec (18 cwd missing, 9 sub-directory leaf names, 3
  worktree or repo roots) with proposals for retiring or importing them. No code.

## 0.14.13

- Docs only: `docs/ROADMAP.md` slice item 4 is now a measurement item with its first measurement (this repo at
  `2ca6368`: 118 tracked files, 79 `.rs`, no `.py`/`.sh`/`.ts`/`.js`/`.ps1`) and the PM's ruling (2026-10-14
  11:48Z) on the open question: `tools/*.py` and `probe/` stay in OverMind, nothing is assigned here. No code.

## 0.14.12

- **`lane-restart assert-idle` warns when the HANDOFF is over its ceiling** (ROADMAP slice item 3, PM ruling
  2026-10-14: warn only, because refusing could block a mandatory restart). The ceiling is
  `CIRESNAVE-EXPECTATIONS.md` section 2.3c step 5: **8 KB for a lane's HANDOFF, 20 KB for the PM's**, measured in
  bytes (`wc -c`), "KB" read as 1024 bytes. The warning names the file, its size and the ceiling, goes to stderr
  and leaves the exit code alone. A missing HANDOFF is silent here (`stop` already refuses that).


## 0.14.11

- **`lane-restart status` says which roles still have no complete launch spec, and why** (ROADMAP slice item 5).
  After the human rows: `launch specs: N of M role(s) have a complete one; without:` and one line per reason with
  the roles under it: `launch spec invalid: ...`, `no launch spec; importable with specs import`, `no launch
  spec; cwd missing (...recreate the directory, or specs retire)`, `no launch spec; the state records no
  launch_args (not importable)`, `no lane state and no launch spec`. In `--json` the same is `coverage:
  {total, complete, gaps: [{why, roles}]}` (additive; `schema` stays 1). Every incomplete role is in exactly one
  gap. The reasons mirror what `specs import` checks (launch_args, cwd), read without writing anything.


## 0.14.10

- **`lane-restart ledger`** (read-only; ROADMAP slice item 2): prints `restart.log` oldest first, filtered by
  `--role`, `--kind`, `--since <RFC 3339|30m|2h|1d>`, `--acted`/`--refused`, `--last N`; `--json` prints the rows
  as JSON lines. A line that is not an entry is counted and named in the footer, never skipped silently; a line
  written before `kind` existed reads as a restart; unknown keys are ignored. It writes nothing (a missing log is
  an empty ledger and is not created). Answers "who restarted whom, and what was refused" without reading the
  file by hand.

## 0.14.9

- **`lane-restart status --json` carries each role's `model` and the disk floor** (ROADMAP slice item 1). `model`
  is what the launch spec records (`null` with no readable spec; Opus shown as recorded). `disk` is
  `{mount, free_bytes, floor_bytes, below_floor, unknown}` for the state volume; it was a stderr line before, and
  the stderr line stays. Additive: `schema` stays 1 and every existing field is unchanged.
  (The ROADMAP's earlier claim that `status` was human text only was wrong; `--json` has existed since 0.12.10.)


## 0.14.8

- **`lane-restart specs retire --role R [--role R]... [--dry-run]`** (PM only; PM-approved 2026-10-14). For a role
  whose recorded `cwd` is not a directory and whose recorded process is not live, **moves** its state file (and
  launch spec, if any) to `<state dir>/retired/<file>.<UTC stamp>`: never deletes, never overwrites, reads the
  move back, writes a ledger row. Role `pm`, a role with a directory (resume it with `start`) and a role with a
  live process are refused with the reason. Additive subcommand: proposed 0.14.x, not breaking.

## 0.14.7

- **`lane-restart specs import` reports unmistakably and groups what it refuses** (PM-approved 2026-10-14, ROADMAP
  "specs import: cwd-missing and Crashed roles", #57). The first and last lines say whether anything was written
  (`DRY RUN - nothing written` / `WROTE n spec(s)`). Roles refused for the same reason, notably `cwd missing`,
  share one line with a count and their names instead of one row each; a group of one keeps its full row, and
  `--verbose` restores one row per role. No role is dropped from the report, and what is written is unchanged.

## 0.14.6

- Docs only: `docs/ROADMAP.md` gains a proposed next slice (status `--json`, a ledger reader, a HANDOFF lint,
  specs coverage, the non-Rust pieces of the OverMind split) and the PM-approved `specs import` rulings
  (group `cwd missing` rows, `specs retire` PM-only, resume Crashed roles with `start`, first/last-line banner).

## 0.14.5

- **`pin-plan --target current|latest`** (with `--emit-patch`; default `current`, unchanged). `latest` pins a
  **tag** ref to the commit of the action's latest release tag (comment = that tag) instead of the commit the old
  ref names, which is how a node20 action reaches its node24 release. A **branch** ref is kept at the SHA it
  names now in either mode: a branch such as `dtolnay/rust-toolchain@stable` selects behaviour (the toolchain)
  that a release tag does not carry, and so is every ref of `dtolnay/rust-toolchain` (`@1.85.0` is a toolchain
  selector too; the `v1` release installs none). `DIR/ACTIONS.md` lists every distinct action with its runtime now (per ref)
  and at the latest release (`runs.using` at that commit), marks a major jump, and puts the actions whose
  latest release still declares node20 or older ("no node24 release yet") and those with no release first.

## 0.14.4

- **`pin-plan --emit-patch DIR`**: one `git apply`-able unified diff per repo (`DIR/<repo>.patch`) that pins each
  mutable `uses:` to the commit SHA its ref names now, the old ref kept as a comment (`@<sha> # v4`; an existing
  comment follows, CRLF kept). It writes only into `DIR`, refuses a `DIR` inside any planned repo, needs the
  network, and opens no PR; unresolvable refs are counted, not patched.
- **Dogfood test** (`crates/pin-plan/tests/own_workflows.rs`): this repo's workflow files, read from the index,
  hold 0 mutable refs and every pin carries its tag comment. `ci.yml` was already pinned (0.12.4, #36) and each
  SHA still equals what its commented ref names (read 2026-10-14); no workflow file changes here.

## 0.14.3

- **`pin-plan`: the SUP-008 read-only pin planner** (new workspace crate `crates/pin-plan`, not published). For
  every repo under `--root` (default `C:/Projects`) it prints one table per repo of each `uses:` that names a
  mutable tag or branch, with two SHAs and the runtime: the **commit the current ref names today** (pinning to
  it changes no behaviour; for a branch such as `dtolnay/rust-toolchain@stable` it is the only faithful pin), the
  **latest release** (tag and commit SHA; the highest plain-semver tag when the repo has no release; annotated
  tags dereferenced to the commit), and the **Node runtime** the action declares at each (`node20 -> node24`),
  flagging a current `node20` or older. It **writes nothing**: no file, no ref, no PR.
- Files are enumerated with `git ls-files` (never a disk walk) and their text is read from `origin/main`
  (`HEAD` if there is none), the ref printed per repo; a file in the index but not at that ref is named, not
  read. A child whose `.git` is a file (a linked worktree) is listed as skipped, so a repo is not planned twice.
  `uses:` lines inside `run: |` / `script: >` blocks are not read as references.
- Each distinct action is looked up once with `gh api`, however many files use it. `--no-network` lists the
  mutable refs without release data; `--all` also lists already pinned references; `--repo NAME` limits it.
  Anything that could not be resolved is printed as `UNRESOLVED:` with the reason and counted in the totals,
  never dropped.
- **Run for real** (2026-10-14, repos at their `origin/main` as fetched by each lane, `gh` as `ciresnave-bot`,
  206 s): 32 repos read (27 linked worktrees skipped), 51 files, 455 `uses:` references: 59 pinned, **393
  mutable** (313 tag, 80 branch) in 22 repos across 32 action repos; 1 unresolved (`NVIDIA/blossom-action@main`
  has no tags); 213 on node20 or older today (100 are `actions/checkout@v4`). This is a different population
  from CodeRipper's 235 pins in 18 repos: this counts every mutable reference in every repo here.
  The pure parts (classification, scanning, runtime and tag choice) are unit-tested.
## 0.14.2

- **Disk floor: nothing is launched while `C:` has less than 20 GB free** (PM task, incident 2026-10-14 04:45Z:
  `C:` hit 4 GB and builds failed in six lanes). New `lane_restart::disk` (`FLOOR_BYTES` = 20 GiB; the volume is
  the one holding the lane state directory, picked as the longest mount that is a path prefix, case and slash
  insensitive). It refuses, with exit 1 and the volume, free and floor named, in: `lane-restart start`
  (also `--dry-run`, which reports what would be refused), `lane-restart --role R --self|--yes` (checked
  **before** the kill, so a refusal never leaves a lane dead and not relaunched), `agentlife restore` and
  `agentlife pending approve` (checked **before** anyone is asked, so an approval is not spent on a refused
  run; `restore::preflight` repeats it as a fatal `disk floor` line at execution time). `restore --dry-run` is
  unaffected. `lane-restart status` prints a `C:\ free: N GB (floor 20.0 GB)` line first (stderr under `--json`,
  so the JSON on stdout keeps its shape).
- **An unreadable probe is a warning, not a refusal**: if no mount contains the state directory the launch goes
  ahead and says it did not check, so a broken probe cannot stop every lane restarting. Say so if you disagree.
- **Debug builds only** honour `LANE_DISK_FLOOR_BYTES` (tests set `0`) and `LANE_DISK_PROBE_PATH` (the path whose
  volume is measured, so Linux CI, where `C:/Projects/.lane-state` is on no mount, still exercises the refusal);
  a release build ignores both, so a lane cannot lower the floor.
- Not exercised for real: the refusal on this machine's actual `C:` (it has 130 GB free), and `lane-restart`'s
  start and restart paths end to end; those are covered by the pure `check`/`volume_for` tests, the restore
  preflight test and the `agentlife restore` e2e.

## 0.14.1

- **`lane-restart host` relays the child's last output instead of leaving it in the channel** (the quirk noted
  in 0.12.12 / #44: "host.rs main loop breaks on child exit without draining rx"). After the main loop stops and
  the ConPTY is closed, the reader can still deliver a final chunk (a ConPTY may flush its last frame only on
  close); it was sent into a channel nobody read any more. `host::drain_tail` now relays everything the reader
  sends until it ends or `READER_END_TIMEOUT` passes, and replaces the old "wait for the reader to end" poll.
  A write error on the outer side stops the relay without hanging the teardown.
- **Not reproduced end to end:** a real `cmd /c "echo ... & <nul set /p =TAIL"` child never lost its tail on this
  box (15 runs before the change, 5 per run after), so the new real-child test
  (`the_childs_final_output_reaches_the_outer_side_before_the_host_returns`) is a regression guard, not the
  proof. The proof is `drain_tail_relays_what_the_reader_sent_after_the_loop_stopped` (queued chunks, a late chunk,
  and a reader that never ends), which fails when the relay is removed.

## 0.14.0

- **`restore` launches a lane from its launch spec when it has a complete one; the registry record is the flagged
  fallback** (the PM's ruling "A", 2026-10-14; RESTART-TOOL-DESIGN.md 14, ROADMAP Next 4). A lane's
  `<role>.launch.json` (written by `lane-restart`, read through `lane_restart::spec`, used only when
  `SpecFile::check` passes) is turned into a recorded-style argv and given to the **same** rebuild as a
  registry record: the same allowlist, the explicit Sonnet/Haiku model pin (Opus is replaced, flagged
  `model-pinned:`), the bypass-is-PM-only refusal, the `;`/control-character refusal, and the portfolio-root and
  exists checks on the spec's directory. So `restore` and `lane-restart start` describe one launch, and a lane
  is no longer "not startable" for want of recorded arguments when it has a spec.
- **`plan::Entry.source`** (`spec` or `registry`) says which one a launch was rebuilt from. It is part of the plan
  hash, printed as `launch from:` in the plan text, and the approval summary adds a section naming every lane
  started from the registry. The role is the record's `LANE_ROLE`, else its launch directory's leaf.
- **Breaking** (second number, pre-1.0): the plan hash changes for every plan, so a frozen or pending plan built
  by 0.13.x no longer verifies and is re-planned, never approved as is. `plan::Inputs` has a new `specs` field.
- No spec is written or changed by `restore`; no live start was run.

## 0.13.1

- **A restore's report names every wanted agent it did not put in a batch, with why** (RESTORE-GAP-ANALYSIS 5b).
  `Report.unstarted` (and a `NOT IN ANY BATCH (n)` section of the text) lists agents the plan held (`held:max_running`,
  `held:memory`) or excluded (`excluded:Running`, `Unverifiable`, `DuplicateAlive`, `NoLaunchArgs`, `CwdMissing`,
  `CwdOutsideRoot`, `BypassNotPm`, `UnsafeArgument`), so a restore can no longer finish clean with a lane missing
  because it looked alive. `NotSelected` and `NotWanted` are the person's own choices and are not listed. Older report
  files still read (`#[serde(default)]`). Additive.
- `twelve_real_stand_ins_are_paced_...` now prints `held` and `running_now` as well as `excluded` when its entry count
  is wrong, so the known flake names its cause.

## 0.13.0

- **`agentlife park|stop` of a RUNNING lane now keeps `lane-restart stop`'s invariant: a lane is stopped only
  if it can be started again** (the PM's task of 2026-10-13: lane-stop and lane-start must keep every lane
  resumable). The lane's `<role>.launch.json` must exist and pass `lane_restart::spec::SpecFile::check`
  (complete, an explicit non-Opus model) twice: before the lane is asked to wrap up, and again just before
  the intent is recorded (the wait can be long). Without one nothing is asked, recorded or ended, and the
  refusal names `lane-restart specs import --role <role>`. After the process is gone the spec is marked
  `suspended` (best effort: a failure is journalled in `stopped` as `spec_error` and leaves the earlier
  complete spec, so the lane stays startable). Dry runs refuse too, so they cannot promise a stop that
  would be refused. **Breaking**: a `park|stop` that used to proceed on a lane with no spec is refused.
  A person who wants the old behaviour has no flag for it, on purpose.
- The role is the lane's self-claimed `LANE_ROLE`, else the leaf of its launch directory (the fallback the
  state file uses); a record with neither is refused.
- `agentlife` now depends on `lane-restart` (published, 0.12.16 on crates.io when read; by version plus
  workspace path, as `lane-state` is). This is what 0.12.15 said was missing.
- `docs/ROADMAP.md`: the priority-ordered list of what is built, what is unblocked and what waits on whom.
- Nothing is wired to a logon task and no live start/stop was run; tests use stand-in processes only.

## 0.12.16

- **The approval prompt's fourth thing: why (board 173, CireSnave 2026-10-12: *"four things, all prompts"*).**
  A restore's `Request.reason` is now `pending::restore_why`: the requester's words (cleaned, at most 60
  characters), `N lanes: <titles>`, and `plan <hash12>`, built from the same frozen plan as the hash, capped at
  200 characters with control and bidi characters stripped. It changes whenever the plan changes. The Hello
  channel shows it as `Reason given:` once `user-request` ships that line (OverMind); the wire field is the
  existing `Request.reason`. The stored pending record keeps the requester's own text. Supersedes the "three
  things" of decisions 134/150 and the logon-task caveat in `docs/CONSENT.md`.
- Nothing is wired to a logon task and no live start/stop was run.

## 0.12.15

- **PR 4 of RESTART-TOOL-DESIGN.md 14.13, the PM's option C: documentation only.** `agentlife park|stop`'s help
  and `src/down.rs`'s module docs now say that it ends the process without writing the lane's launch spec, so it
  does not keep `lane-restart stop`'s rule that a lane is suspended only if it can be started again, and that
  the PM stops a running lane with `lane-restart stop`. No behaviour changed.
- **Not done, and why:** refusing `park|stop`'s kill when no complete spec exists needs `lane_restart::spec`; the
  root crate depends on `lane-state` only, and `restore` launches from the registry, not the spec. A person-
  caller path for `lane-restart stop|start` was not specified (14.8: the PM session's alone), so agentlife does
  not call them.
- `tools/launch-lane.ps1` (the PM's) is named only in prose here (CHANGELOG, RESTART-TOOL-DESIGN.md,
  docs/RESTORE-GAP-ANALYSIS.md); no code, script or build step in this repository invokes it.

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
