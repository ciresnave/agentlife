# Session restart tool — design spec

**Status: SPEC, settled — not yet built.** Written per the task from CireSnave via the PM,
2026-09-18: pulled ahead of the throttle ("it is another project that will help us avoid the
condition that required the throttle in the first place"). Location decided: a Cargo workspace crate
in this repo, sharing OverMind's version number, with a Rust CI job (fmt, clippy, test) branch
protection requires, and SPDX `MIT OR Apache-2.0` headers.

**Every `DECISION NEEDED` point from the first draft is now answered by the PM (2026-09-18,
CireSnave sees and can override)** - marked `DECIDED` in place below. Everything else is grounded in
either the task's own stated requirements or Claude Code's documented behaviour, verified directly
against `code.claude.com/docs` in this session — not assumed, and in one case correcting an initial
wrong assumption (below).

## 0. The one correction that reshaped this design

The natural first idea — poll `claude agents --json` for each lane's `status` (`busy`/`waiting`/
`idle`) — **does not work**. Verified directly against `agent-view.md`:

> *"Interactive sessions you have open in other terminals don't appear until you background them."*

Every lane in this portfolio (OverMind, Synapse, the PM) runs **interactive**, not backgrounded. That
command's entire field table (`id`, `state`, `pid`, `status`, `waitingFor`) only populates for
sessions started as background sessions. There is no external, polling-based way to ask an
interactive Claude Code process "are you busy right now" at all.

**So the design below does not rely on polling for state.** It relies on each lane maintaining its
own state, event-driven, via documented hooks - and an external reader (this tool, or the PM) only
ever reads what a lane already wrote about itself.

## 1. What each lane self-reports: `C:/Projects/.lane-state/<role>.json` (location per §7)

A small JSON file, one per lane, written by a hook running **inside** that lane's own Claude Code
process - so it is always describing that process's own, current-moment truth, never a stale
snapshot fetched from outside.

```jsonc
{
  "role": "overmind",                    // this lane's role name - matches its HANDOFF file
  "session_id": "3f9a...-uuid",          // from transcript_path's filename, read in the hook
  "pid": 48213,
  "cwd": "C:/Projects/OverMind",
  "name": "overmind",                    // session name, if set (--name / /rename)
  "model": "claude-sonnet-5",
  "permission_mode": "prompting",
  "remote_control": true,
  "busy": false,                         // see the event mapping below
  "subagents_running": 0,                // incremented on SubagentStart, decremented on SubagentStop
  "background_bash_unknown": true,       // see §1a - this is a KNOWN GAP, always true today
  "updated_at": "2026-09-18T09:14:03Z",
  "updated_by_event": "Stop"
}
```

**Hook wiring** (all documented, verified against `hooks.md`):

| hook event | effect on the state file |
|---|---|
| `SessionStart` | write the file fresh: `pid`, `cwd`, `session_id` (parsed from `transcript_path`), `name`, `model`, `permission_mode`, `remote_control`, `busy: false` |
| `UserPromptSubmit`, `PreToolUse` | `busy: true` |
| `Stop` | `busy: false` |
| `SubagentStart` | `subagents_running += 1` |
| `SubagentStop` | `subagents_running -= 1` (floor at 0 - see §1a) |
| `PostModelSwitch` | update `model` |
| `SessionEnd` | delete the file, or mark `"exited": true` - **not** leave a stale "idle" record lying around after the process is gone |

### 1a. Known, unresolved gap: background Bash commands

Verified against `hooks.md`: **there is no documented hook or query for whether a `run_in_background`
Bash command is still running.** `PostToolUse` fires when the tool CALL resolves, which for a
backgrounded command is immediately (it returns a task handle), not when the background process
itself finishes. `SubagentStart`/`SubagentStop` give a reliable *count* for subagents; there is no
equivalent pair of events for background shells.

`background_bash_unknown: true` is always set, honestly, rather than pretending this is tracked.

**DECIDED (PM, 2026-09-18):** use both signals, and refuse if EITHER shows activity.

1. The self-asserted `no_background_shells: true` claim, written into HANDOFF by the lane itself -
   the honesty-burden mechanism proposed above.
2. **Independently**, the tool walks the target `claude` process's own child-process tree (via its
   PID, using the OS process list, no hook or Claude Code cooperation needed) looking for live shells
   - `bash`, `pwsh`, `cmd`, and their own children. A backgrounded Bash command is a real OS child
   process even though no Claude Code hook fires for it, so the process table can see it regardless.

**Unknown counts as unsafe** in both directions: an absent `no_background_shells` claim refuses the
restart, and a child-process walk that can't enumerate cleanly (permissions, a transient OS error)
refuses rather than proceeding on the claim alone. Neither signal is trusted by itself - matches this
portfolio's own "no absence without a positive control" discipline: the claim alone could be wrong,
and the process walk alone could miss a shell spawned through something that doesn't appear as a
direct child (a detached process, a service) - together they cover more than either does alone.

## 2. Process identification — "never kill by name alone"

Verified: Claude Code does not document a PID-reuse guard, and `pid` alone is not sufficient (OS PIDs
recycle). The positive-identification tuple this tool checks, all four, right before sending any
signal:

1. **PID** is running, and is a `claude` process (checked via the OS process list's own recorded
   command/image name for that PID - not assumed from the state file).
2. **`cwd`** of that live PID matches the state file's `cwd`.
3. **`session_id`** recorded in the state file corresponds to a transcript file that exists on disk
   under that `cwd`'s project directory (`~/.claude/projects/<project>/<session-id>.jsonl`,
   documented in `sessions.md`) and whose mtime is recent enough to be plausibly this run, not a
   leftover from a much older process that reused the PID.
4. **`updated_at`** on the state file is recent (a tool-configurable staleness bound, e.g. 2 minutes) -
   a stale file is refused, not trusted, because the lane may have crashed without a `SessionEnd`
   hook ever firing.

Any mismatch on any of the four: refuse, log why, do not kill.

## 3. Authorization: who may restart whom

- **A lane may request its own restart at any time it chooses** (it just wrote its own HANDOFF, so it
  is, by construction, at a point it considers safe to leave). No `busy`/`subagents_running` check is
  needed for a *self*-restart - the lane is the same process making the request, so there is no
  staleness window between "I checked" and "I act."
- **The PM may request a restart of another lane only when that lane's own state file says
  `busy: false`, `subagents_running: 0`, and (per §1a's resolution) `no_background_shells: true`
  was set when its HANDOFF was written**, AND the four-part identification in §2 passes at the moment
  of the kill, not just at the moment the PM decided. **The PM's own belief that a lane is idle
  (from `ListAgents`, or anywhere else) is never sufficient by itself** - the tool re-checks the
  state file itself, fresh, immediately before acting.
- **No lane may restart a DIFFERENT lane.** Only self-restart, or PM-restart-of-idle-lane. Written
  down explicitly because the task asked for exactly this to be written down.

## 4. HANDOFF file format — one per role

`<role>/HANDOFF.md`, written by the lane itself immediately before requesting its own restart (or, on
the PM's side, before a PM-initiated restart is even considered eligible - see §3, `no_background_shells`).

```markdown
# HANDOFF — overmind
Written: 2026-09-18T09:14:03Z
Session ending: 3f9a...-uuid

## Where things stand
<free text - what's in flight, what's queued, what's blocked and on what>

## Open PRs / branches
<list>

## Board items waiting on someone else
<list, with who>

## Anything the next session must NOT re-derive from scratch
<the load-bearing facts that took real work to establish this session>

## Role-specific
<optional - free content a role's own CLAUDE.md/role doc may require>
```

**DECIDED (PM, 2026-09-18):** the shared skeleton above, plus an optional `## Role-specific` section
with free content. The PM's own section is just a pointer to `C:/Projects/PM-HANDOFF.md`, which stays
the PM's real, full handoff document - this file doesn't replace it. Nothing else is mandated per
role for now.

## 5. Relaunch mechanics

**REVISED (PM finding, 2026-09-18): NEVER `--resume`.** The first draft of this section proposed
`claude --resume <session_id>`, verified against `sessions.md` as automatically restoring the model
and permission mode. That's true, but it restores something else along with them: the WHOLE prior
transcript, reloaded into context. That is exactly the per-turn cost a restart tool exists to cut -
the whole point of restarting is a small, fresh context, not the old one reloaded under a new PID.

**The actual design: a genuinely fresh session, with continuity through HANDOFF alone.**

- **`state.session_id` is used ONLY by `authorize::decide`'s identity check (§2)** - proving the pid
  being killed really is the recorded lane, via a matching, recent transcript. It plays no part in
  the relaunch command at all.
- **Model and permission mode are NOT auto-restored on a fresh session** (that restoration is a
  property of `--resume`/`--continue`, which this design no longer uses) - so the tool passes them
  explicitly: `--model <state.model> --permission-mode <state.permission_mode>`, both recorded in the
  state file by the lane's own hooks (§1).
- **Remote Control does not "reconnect" either** - a fresh session has no prior RC record to
  reconnect to; `remote-control.md`'s auto-reconnect-on-resume behaviour doesn't apply here, since
  there is no resume. Instead: `--remote-control` is passed at launch when `state.remote_control` was
  true, which starts a NEW RC session (documented as a real launch flag) - the property "RC was on"
  carries over; the specific prior RC session's identity does not, and cannot, for a fresh session.
- **`--mcp-config`, `--settings`, `--plugin-dir`, `--fallback-model`, `--add-dir`** are not carried
  over by this tool at all (still not recorded in §1's schema) - documented here as NOT carried over,
  rather than silently dropped without saying so, per this file's own §0 discipline.
- **The launch command:** `claude --name <name> --model <state.model> --permission-mode
  <state.permission_mode> [--remote-control] "read <role> HANDOFF and continue"`, run in the recorded
  `cwd`. **No shell in the loop** - PM finding, 2026-09-18: an earlier draft's `cmd /C start "" claude
  ...` put `role`/`name` (state-file content, not this host's own choice) through cmd.exe's own
  parser, which a Rust `Command`'s argv-quoting does not protect against. Fixed two ways, not one:
  `role` and `name` are validated against `^[A-Za-z0-9_-]{1,64}$` and refused otherwise (confirmed
  live: `a&calc` as a name reaches `cmd.exe` and would execute `calc`), AND the launch now spawns
  `claude.exe` directly with `CREATE_NEW_CONSOLE` (Windows) - a real argv array Windows'
  `CreateProcess` never hands to a shell for re-parsing, giving the same visible new window without
  cmd.exe touching any of it.

## 6. Bulletproof requirements (from the task, restated as testable properties)

1. **Positive identification** — §2's four-part check, every time, no exceptions, including for
   self-restart (a lane still shouldn't be able to accidentally target a different process's PID
   through a bug in its own state file). **Extended (PM finding, 2026-09-18):** identification at
   `decide()` time is not enough on its own - the pid could be recycled in the window between
   deciding and the actual kill. The process's start time (and exe path, when both sides have one)
   is recorded at `decide()` time and RE-READ, fresh, immediately before the real kill signal;
   any mismatch refuses rather than kills.
2. **Dry-run mode** — `--dry-run` performs every check, prints exactly what it would do (kill PID X,
   wait, launch a fresh session in cwd Y), and does neither. **DECIDED (PM, 2026-09-18):**
   self-restart is real by default (the agent chose that boundary itself, restarting only its own
   process). Restarting a DIFFERENT lane is dry-run unless `--yes` is passed - and even with `--yes`,
   it still refuses unless that lane is idle (§1a's two-signal check) and its state file is fresh
   (§2's four-part identification).
3. **A log of every kill and launch** — append-only, at `C:/Projects/.lane-state/restart.log` (§7),
   every entry: timestamp, who requested it (self or PM), the full identification tuple checked, the
   verdict (killed+launched / refused+why), and the exact command line used to relaunch. Never
   overwritten, never rotated silently - this is exactly the kind of artifact this portfolio's own
   evidence culture depends on existing.

## 7. Where lane state and the log live

**DECIDED (PM, 2026-09-18):** fixed, portfolio-wide, not per-lane:

- `C:/Projects/.lane-state/<role>.json` - one file per lane, matching §1's schema.
- `C:/Projects/.lane-state/restart.log` - the one append-only log for every lane's kills and
  launches (§6.3), not split per role, so a single read shows the whole portfolio's restart history.

**Kept out of every git repo**, including this one's own `.portfolio-history.git` (the PM is adding
`.lane-state/` to its excludes) - this is live, per-process runtime state, not a durable record
anyone should be committing.

## 8. What this spec still doesn't decide

- **Exact Windows process-kill mechanism** (`taskkill`, `TerminateProcess` via a crate, sending
  Ctrl-C then escalating) - an implementation detail, not a design question, deferred to the build.

## 9. What's needed before building starts

All four `DECISION NEEDED` points from the earlier draft are now answered (§1a, §4, §6.2, §7). Next:
fold these into the crate skeleton and CI (already in progress), then build the kill/launch logic
against this now-settled spec.

**Update, 2026-09-18: the crate is built (OverMind#53).** What's left, per the PM: *"Before the tool
restarts any real lane, the hooks that write `.lane-state` must exist on every lane... don't install
them yourself: settings are CireSnave's."* §10 is that proposal - not installed anywhere, and not
authored to be installed by this lane.

## 10. PROPOSAL, not installed — the hooks that write `.lane-state/<role>.json`

**CireSnave's or the PM's to install, in `settings.json`.** Nothing in this section has been applied
anywhere. Verified against `hooks.md`'s documented common input fields and settings shape before
writing this, not assumed - and that check surfaced two real gaps, honestly flagged below rather than
worked around with a guess.

**REVISED (PM review, 2026-09-18): the first draft's PowerShell script is replaced by
`lane-restart state <event>`, a subcommand in this same crate.** Four real issues in the PowerShell
version, none of them fixed by tweaking that script:

1. **Cost.** `PreToolUse` fires on every tool call, in every lane - spawning `powershell.exe` plus
   `Get-CimInstance` each time cost ~0.3-1s, a portfolio-wide latency tax. A native binary starts in
   single-digit milliseconds.
2. **Races.** Parallel tool calls fire concurrent hooks; an unguarded read-modify-write on one JSON
   file loses updates - a lost `SubagentStart` makes `subagents_running` too LOW, the unsafe
   direction (a busy lane could look idle). Fixed with a create-new-file lock (atomic at the OS level)
   around each read-modify-write cycle, and a stale-lock reclamation so a crashed holder can't wedge
   every future hook forever. Mutation- and concurrency-tested: 20 real OS threads racing
   `SubagentStart` against the same file, none lost.
3. **`Get-Date -AsUTC`** is PowerShell 7 only - moot now; there's no PowerShell in the design at all.
4. **Role from the cwd leaf gives the PM's own lane `"projects"`** (its cwd is `C:/Projects` itself).
   Fixed: `LANE_ROLE`, when set, always wins over the cwd-leaf fallback.

**On dropping `PreToolUse` in favour of `UserPromptSubmit` + `Stop` alone (raised as worth
considering):** not done. `UserPromptSubmit` fires specifically for an interactively-typed prompt;
whether it also fires for every other way a turn can start - a delivered peer/cross-session message,
a `/loop`-scheduled prompt, a notification-driven response - isn't confirmed by the documented common
fields, and getting that wrong means `busy` silently staying `false` during real work. That's the
unsafe direction for the one thing this check exists to prevent (the PM restarting a lane it wrongly
believes is idle). `PreToolUse` fires before literally any tool call regardless of what triggered the
turn, so it's kept - and moving to a native binary is what makes keeping it cheap again.

### 10.1 Two gaps in what a hook can know, found while designing this

- ⚠️ **No hook receives the running Claude Code process's own PID.** The common input fields
  (`session_id`, `transcript_path`, `cwd`, `permission_mode`, `hook_event_name`, ...) do not include
  one, and `lane-restart` needs it (§2's identity check signs against `pid`). §10.3's subcommand
  derives it itself: a hook runs as a CHILD process of the `claude` process, so a parent-process
  lookup (via `sysinfo`) gets it, with a sanity check that the parent's own image name really is
  `claude`/`claude.exe`.
- ⚠️ **No hook receives the current model name either**, at `SessionStart` or otherwise, except
  `PostModelSwitch`'s own event (whose payload isn't in the DOCUMENTED common-fields table, so this
  proposal doesn't assume its shape without checking that separately). Consequence: `model` in the
  state file starts **unset** for a session that never explicitly switches models, until proven
  otherwise. **Flagged, not worked around** - a wrong guess here would feed a wrong `--model` into a
  future relaunch.

### 10.2 Role: `LANE_ROLE` override, falling back to the `cwd` leaf

**REVISED (PM finding, 2026-09-18):** the cwd-leaf-only version of this proposal gave the PM's own
lane `"projects"` (its cwd is `C:/Projects` itself, not a per-lane subdirectory) - a real bug, not a
hypothetical one. Fixed: `LANE_ROLE`, an environment variable set once per lane wherever that lane's
own launch environment already lives, always wins when present; the lowercased leaf directory name
of `cwd` (`C:/Projects/OverMind` → `overmind`) remains the fallback for every lane whose directory
name already IS its role - which is most of them, so most lanes need no new setting at all.

### 10.3 The hook command: `lane-restart state <event>`

A subcommand of this same crate (`crates/lane-restart/src/lane_state_writer.rs`), not a separate
script. Exec form (`args`, no shell): the event name is a literal argument, common hook input JSON
comes on stdin, exactly as `hooks.md` documents.

- **Role**: `LANE_ROLE` env var if set, else `cwd`'s lowercased leaf directory name (§10.2, now with
  the PM's override).
- **PID**: this hook process's own parent, looked up via `sysinfo` (the same crate `facts.rs`
  already depends on) - refused, not guessed, if that parent isn't named `claude`/`claude.exe`.
  ⚠️ **SUPERSEDED (PM finding, 2026-09-18, from a REAL interactive session) - the headless result
  below does NOT hold for a real lane.** A first pass verified this against an ephemeral, headless
  `claude -p --settings <scratch file>` session and found the hook's DIRECT parent was `claude.exe`
  in both hook forms, no shell in between. **That result doesn't generalise.** Running the same
  diagnostic in a real interactive session (`claude --remote-control`, cwd `OverMind`, per §11.1's
  own plan) found: `powershell.exe(hook) <- bash.exe <- bash.exe <- claude.exe <- pwsh.exe <-
  WindowsTerminal.exe`. Claude Code ran the hook through Git Bash, two layers deep - the direct
  parent was `bash.exe`, not `claude.exe`. A single-hop check refuses every real interactive write.
  **Fixed:** `claude_parent_pid` now walks up the ancestry, skipping ONLY known shell images (`bash`,
  `sh`, `cmd`, `powershell`, `pwsh`; capped at 4 hops) and returns the first `claude` found - refusing
  outright if a non-shell, non-`claude` image appears first, or if the hop limit is hit. Unit-tested
  against the exact observed chain (`bash <- bash <- claude`), a single-shell chain, a refused
  stranger (`node <- claude`, and a stranger behind a real shell hop), and the hop-limit case; all
  mutation-verified. **Still needs:** the interactive diagnostic re-run with the EXACT command string
  §10.4 installs (exec form), to confirm this fix actually resolves the real case, not just the
  chain shape recorded from it - held per §11.1 until that's done.
- **Concurrency**: a create-new-file lock (`<role>.lock`, atomic at the OS level) held for the whole
  read-modify-write cycle; a lock older than 5s is treated as abandoned and reclaimed rather than
  wedging every future hook forever.
- **Write**: temp file + rename (atomic on the same volume), never a direct overwrite.
- **Events**: identical mapping to the first draft - `SessionStart` writes fresh (preserving a
  previously-learned `model`/`remote_control` across a resume, never resetting them to unknown);
  `UserPromptSubmit`/`PreToolUse` set `busy: true`; `Stop` clears it; `SubagentStart`/`SubagentStop`
  increment/decrement (floored at 0, `saturating_sub`); `PostModelSwitch` updates `model` when its
  payload includes one; `SessionEnd` deletes the file. `no_background_shells` is never set `true` by
  any hook event - per §1a, that claim is the lane's own manual assertion when writing HANDOFF.
- **Tests**: every branch above is a pure function (`apply_event`) tested without any file I/O, plus
  a test driving 20 real OS threads at the real locked write path to prove `SubagentStart` is never
  lost under real concurrency - not just asserted safe in isolation.

### 10.4 The `settings.json` block (not applied)

**Recommended scope: user-level `~/.claude/settings.json`**, not per-project - every lane already runs
under this one user account, and a user-level entry covers every project directory without editing
each lane's own repo. Per-project would need the identical block added to every lane's project
settings separately, for no benefit this design needs. Still CireSnave's call.

The binary itself needs to exist at one fixed, absolute path every lane can reach (built once, not
per-project) - proposed as `C:/Projects/.claude-hooks/lane-restart.exe`, a sibling of `.lane-state/`
for the same reason: portfolio-wide runtime tooling, kept out of every git repo. Exec form (`args`)
is used throughout, so no shell ever parses anything.

⚠️ **REVISED (PM finding, 2026-09-18): `~/.claude/settings.json` already has a `hooks` key.**
Confirmed by reading the file directly, not assumed: it holds a `SessionStart` entry running
`run_wrap_hidden.vbs` - CireSnave's own, unrelated to this proposal. **This MERGES into that file,
appending to each event's array - it never replaces the `hooks` key, and `SessionStart`'s existing
entry stays exactly where it is.** The block below shows the merged result for the one event that
already had something (`SessionStart`) and the new entries alone for the rest:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          { "type": "command", "command": "wscript.exe \"C:\\Users\\cires\\.claude\\scripts\\run_wrap_hidden.vbs\"" }
        ]
      },
      { "hooks": [{ "type": "command",
        "command": "C:/Projects/.claude-hooks/lane-restart.exe", "args": ["state", "SessionStart"] }] }
    ],
    "UserPromptSubmit": [{ "hooks": [{ "type": "command",
      "command": "C:/Projects/.claude-hooks/lane-restart.exe", "args": ["state", "UserPromptSubmit"] }] }],
    "PreToolUse": [{ "hooks": [{ "type": "command",
      "command": "C:/Projects/.claude-hooks/lane-restart.exe", "args": ["state", "PreToolUse"] }] }],
    "Stop": [{ "hooks": [{ "type": "command",
      "command": "C:/Projects/.claude-hooks/lane-restart.exe", "args": ["state", "Stop"] }] }],
    "SubagentStart": [{ "hooks": [{ "type": "command",
      "command": "C:/Projects/.claude-hooks/lane-restart.exe", "args": ["state", "SubagentStart"] }] }],
    "SubagentStop": [{ "hooks": [{ "type": "command",
      "command": "C:/Projects/.claude-hooks/lane-restart.exe", "args": ["state", "SubagentStop"] }] }],
    "PostModelSwitch": [{ "hooks": [{ "type": "command",
      "command": "C:/Projects/.claude-hooks/lane-restart.exe", "args": ["state", "PostModelSwitch"] }] }],
    "SessionEnd": [{ "hooks": [{ "type": "command",
      "command": "C:/Projects/.claude-hooks/lane-restart.exe", "args": ["state", "SessionEnd"] }] }]
  }
}
```

Every OTHER top-level key in that file (`env`, `permissions`, `model`, `deniedMcpServers`, `worktree`,
`enabledPlugins`, ...) is untouched by this - only `hooks` is merged into, and only by appending.

`LANE_ROLE` (§10.2) is set once per lane, wherever that lane's own launch environment is configured -
not part of this settings.json block, which is identical across every lane.

### 10.5 Not proposed here

- Installing any of the above. This section is the design; CireSnave/the PM decide whether, when, and
  exactly how.
- A fix for the two gaps in §10.1 - they're recorded as known, current limits of what a hook can
  report, not solved by guessing at data hooks don't document providing.

## 11. Install proposal, for CireSnave

**Still nothing installed.** This is the concrete plan the PM asked for, to put on his board -
sequenced so nothing acts on a real lane before that lane's own behaviour is checked, and reversible
at every step.

### 11.1 Step 1 (before anything else): verify interactively, not just headless

**RESULT (PM, 2026-09-18): done once, and it found a real bug, now fixed - re-run needed before
this step is complete.** §10.3's parent-process check was first verified only against a **headless**
`claude -p` session; every real lane runs **interactive**, several reached over **Remote Control** -
a materially different launch path. The PM ran this step for real, on OverMind's own lane
(`claude --remote-control`, `SessionStart`), and found the headless result does NOT hold: the direct
parent was `bash.exe`, not `claude.exe` - Claude Code ran the hook through Git Bash, two layers deep.
Per this section's own step 4, the install was stopped and the diagnostic hook removed before going
any further.

**Fixed in §10.3**: `claude_parent_pid` now walks past known shell layers instead of requiring a
single direct hop. **Still open**: the interactive diagnostic needs re-running with the exact command
string §10.4 installs (exec form) to confirm the fix, not just the recorded chain shape, is right -
this step isn't complete until that re-run passes.

1. Pick the first lane to receive the hooks (recommend: whichever of OverMind/Synapse/the PM is
   least busy at the time).
2. Install *only* the diagnostic parent-chain check (§10.3's verification script, not the real
   `lane-restart state` hooks yet) on that ONE lane's own settings, for a single event
   (`SessionStart` is enough) - using the same exec-form command shape §10.4 installs, not a
   shell-form stand-in, since the two may not behave identically.
3. Restart that lane normally (however it's normally started/reconnected, Remote Control included)
   and confirm `claude_parent_pid`'s fixed logic (walk-past-shells, refuse-on-stranger) actually
   resolves to the real `claude.exe` pid for this exact chain, not just that the chain matches what
   was recorded from the earlier run.
4. Remove the diagnostic hook. Only if step 3 confirms the fix works for real does step 11.2 proceed
   on that lane.

### 11.2 Building and placing the binary

```
cd C:/Projects/OverMind
cargo build --release -p lane-restart
```

Copy `target/release/lane-restart.exe` to `C:/Projects/.claude-hooks/lane-restart.exe` - **the
release binary, not `target/debug`** (PM finding, 2026-09-18: debug builds are slower and not what a
latency-sensitive hook, firing on every tool call, should run). Re-run this copy step after any
future change to the crate; nothing here auto-updates it.

### 11.3 `LANE_ROLE` per lane

Set once, wherever each lane's own launch environment already lives (its own shortcut, launch script,
or terminal profile - not part of the shared `settings.json` block, which stays identical across
every lane):

| Lane | `cwd` | `LANE_ROLE` needed? | Value |
|---|---|---|---|
| OverMind | `C:/Projects/OverMind` | No - cwd leaf already gives `overmind` | (unset is fine) |
| Synapse | `C:/Projects/synapse` | No - cwd leaf already gives `synapse` | (unset is fine) |
| the PM | `C:/Projects` | **Yes** - cwd leaf gives `projects`, the bug this override exists for | `pm` |

Any future lane whose `cwd` doesn't already match its intended role name needs the same explicit
override; every lane whose directory name already IS its role needs nothing.

### 11.4 The `settings.json` block

Exactly §10.4's block, user-level (`~/.claude/settings.json`), unchanged from that section - not
repeated here to avoid two copies drifting.

### 11.5 Rollback

**REVISED (PM finding, 2026-09-18): `~/.claude/settings.json` already has a `hooks` key** - a
`SessionStart` entry running `run_wrap_hidden.vbs`, CireSnave's own, unrelated to this proposal.
Confirmed directly by reading the file, not assumed. Removing the whole `hooks` key would delete
that too. **Rollback is: remove only the entries whose `command` is `lane-restart.exe`, from each
event's array, leaving every other hook (including that one) untouched.** Once removed, hooks simply
stop firing; `.lane-state/*.json` files stop updating and, per §2's four-part identification, quickly
read as stale and get refused by `lane-restart` rather than trusted - the tool fails closed on its
own, not because rollback does anything special beyond removing its own entries. No lane-side change
is needed; the binary and `.lane-state/` directory can be left in place inert, or deleted, either is
safe.

### 11.6 Sequencing

§11.1 (one lane, diagnostic only) → §11.2 (build once) → §11.3 (`LANE_ROLE` for that one lane, if it
needs one) → §11.4 (install the real hooks for that one lane only, e.g. via that lane's own
project-level settings first if a narrower rollout than user-level is wanted) → observe `.lane-state`
populate correctly across a few real turns → only then widen to every lane, and only then does anyone
attempt a real `--self` or PM-initiated restart for the first time.
