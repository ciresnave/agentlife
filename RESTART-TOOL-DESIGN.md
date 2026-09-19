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
  "model": "claude-sonnet-5",            // absent (null) until a hook payload actually carries it - §10.3
  "permission_mode": "prompting",        // absent (null) until a hook payload actually carries it - §10.3
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

⚠️ **REVISED (PM finding, 2026-09-18, first real restart attempt): "written into HANDOFF by the lane
itself" above was never actually implemented as a command - no production code path ever wrote
`no_background_shells: Some(true)`.** Every real restart of another lane refused, and 79 passing tests
didn't catch it because every existing test's fixture hard-coded the field already set. **Fixed:
`lane-restart assert-idle [--role <name>]`** - a command the lane runs itself, from its own shell,
right after writing HANDOFF (not a Claude Code hook; nothing invokes this on the lane's behalf, since
no hook can honestly know whether a background shell is still running). It sets
`no_background_shells: Some(true)` on that lane's own state file. **A later `UserPromptSubmit` or
`PreToolUse` event clears it back to `None`**, so an assertion made once can't go on authorizing a
restart after the lane has done more work that could have started a new background shell - the claim
is about THIS moment, not a durable fact. An end-to-end test (hook JSON on stdin → a real state file on
disk → `authorize::decide()` reading that same file, no hand-built `LaneState` fixture anywhere in it)
now exists specifically so "never set in production" can't hide behind a fixture again.

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

⚠️ **REVISED (PM finding, 2026-09-18, real restart attempt, second retest): #2's `cwd` compare was a
raw string `==`, and refused on formatting, not on identity.** A real Windows process's own `cwd`
carries a trailing separator (`C:\Projects\.restart-test\`); the hook's recorded `cwd` never does
(`C:\Projects\.restart-test`) - the dry run correctly failed closed, but for a reason that had nothing
to do with the pid's actual identity. **Fixed:** a new `paths.rs` module's `paths_match` normalises
both sides before comparing - unifies `/`/`\`, strips one trailing separator (except a bare drive
root, where it is significant), and compares case-insensitively on Windows, where the filesystem
itself is - and is used for BOTH the `cwd` compare here and the `exe`-path compare in
`facts::kill_verified` (`§2`'s process-identity re-check), which had the identical gap. **Never a
prefix match**: `C:\a` and `C:\ab` stay distinct, so a state file for one lane can never pass the
identity check for a lane whose path happens to start the same. Unit-tested directly (separator,
trailing-separator, case, and the prefix-must-not-match property), plus a real-child-process test that
reads a genuine Windows `cwd` (trailing separator and all) and confirms it matches the same path
without one.

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
  live: `a&calc` as a name reaches `cmd.exe` and would execute `calc`).

⚠️ **REVISED (PM finding, 2026-09-18, second real-restart retest): `CREATE_NEW_CONSOLE` alone was NOT
enough - the first real end-to-end restart proved continuity via HANDOFF (a fresh session read it and
replied correctly), then exited 12 seconds later.** Root cause: Rust's `Command` always sets
`STARTF_USESTDHANDLES` and inherits the parent process's own std handles, even under
`CREATE_NEW_CONSOLE` - a hook-invoked lane runs from its own Bash tool, so the child got PIPES, not a
real console. Non-TTY stdin plus a prompt argument made `claude` behave like one-shot print mode: read
HANDOFF, reply, exit. **Fixed:** launch through Windows Terminal instead, which gives the child a REAL
ConPTY independent of this process's own handles - exactly how CireSnave's own lanes are launched
(WindowsTerminal → pwsh → claude): `wt.exe -w new -d <cwd> claude --name <name> [--model ...]
[--permission-mode ...] [--remote-control] "<prompt>"`, an explicit argv, no shell. Falls back to
`conhost.exe claude ...` if `wt.exe` isn't on `PATH` (detected via a `NotFound` spawn error, not
guessed). **`wt.exe` treats `;` as its OWN command separator** (`wt new-tab ; split-pane ...`), a
parsing layer on top of the normal, already-safe argv passing every element goes through regardless -
every argv element `wt.exe` receives (`cwd`, `model`, `permission_mode`, the prompt) is checked for
`;` and the whole relaunch refused if found, before anything is killed.

⚠️ **ALSO REVISED (PM finding, 2026-09-18, same retest): logging "relaunched a fresh session" from
`spawn()` returning `Ok` alone was dishonest** - exactly the graceful-exit case above would have been
logged as a success. **Fixed:** a post-launch liveness check polls (up to 30s, 1s intervals) for a
live `claude`/`claude.exe` process whose (normalised) `cwd` matches the target and whose `start_time`
is at or after the kill - the only way to observe "a new session actually came up" from outside it,
since its own `SessionStart` hasn't fired yet to record anything. Once found, a follow-up check ~10s
later confirms it is STILL alive before `acted: true`/"relaunched" is ever logged; otherwise the log
says exactly why ("relaunch FAILED - no live claude process was ever observed..." or "...came up but
exited again shortly after"). The polling protocol is unit-tested with an injectable `sleep` (no real
~40s wait in the test suite); the underlying process-matching (`find_process_in`, parametrised on the
image-name list so it can search for a stand-in like `ping` instead of `claude`) is proven against a
REAL spawned child, per this section's own "not unit tested without a real process" discipline for
anything that touches `sysinfo` directly.

⚠️ **The `start_time`-at-or-after-the-kill threshold carries a small safety margin (5s), found live
while adding this fix's own real-process CI coverage**: the kill timestamp is wall-clock time
(`facts.now()`), but the OS-reported `start_time` a fresh relaunch gets compared against comes from a
different clock source (on Linux, ticks-since-boot converted to a Unix timestamp) - the two can
disagree by a second or two without either being wrong. Without slack, `ubuntu-latest`'s CI runner
reproducibly excluded a genuinely fresh relaunch as "too old" by exactly this rounding gap. The margin
doesn't weaken the check meaningfully - a stale, truly unrelated process from before the kill is still
excluded - it only stops two clock sources' ordinary disagreement from being read as evidence of
staleness.

⚠️ **REVISED (PM finding, 2026-09-18, third real-restart retest): the mechanics are proven - kill,
`wt.exe` launch with the exact planned argv, `--remote-control` live, liveness confirmed - but the new
session came up believing it was a CHILD of the session that requested the restart.** `lane-restart`
always runs from a lane's own Bash tool - i.e. from INSIDE a running Claude Code session - and `wt.exe`
inherits that whole environment by default. The relaunched session showed "Transcript saving is off -
inherited `CLAUDE_CODE_CHILD_SESSION` marker," had no transcript, and its positional prompt never
auto-submitted. **Fixed:** `spawn_relaunch` strips the vars that actually NAME this session or its IPC
channel before launching, via `Command::env_remove` on both the `wt.exe` and `conhost.exe` fallback -
confirmed by dumping `env` from inside a real session first, not guessed:
`CLAUDECODE`, `CLAUDE_CODE_CHILD_SESSION`, `CLAUDE_CODE_ENTRYPOINT`, `CLAUDE_CODE_SESSION_ID`,
`CLAUDE_CODE_SESSION_ATTENDED`, `CLAUDE_CODE_BRIDGE_SESSION_ID`, `CLAUDE_CODE_MESSAGING_SOCKET`,
`CLAUDE_CODE_MESSAGING_TOKEN`, `CLAUDE_CODE_SSE_PORT`, `CLAUDE_PID`. A user's own persistent config
vars (`CLAUDE_EFFORT`, `CLAUDE_CODE_USE_POWERSHELL_TOOL`, `CLAUDE_CODE_EXECPATH`, anything unrelated
like `CLOUDFLARE_*`) are deliberately left alone - this is a targeted strip of session identity, not a
blanket environment wipe.

**Also noted (PM finding, same retest, cosmetic - not fixed in code):** killing `claude` leaves its
hosting terminal tab behind, in raw mouse-tracking mode (the shell's own escape codes never got
cleaned up). Not worth a terminal-reset-sequence fix - the actual answer is that a lane's tab should
close on its own once its `claude` process exits, which is already true when a lane is LAUNCHED the
same way this tool relaunches it (`wt … claude …`, one `claude` per tab, no intermediate shell prompt
sitting underneath it).

⚠️ **REVISED (CireSnave, via the PM, 2026-09-18): a relaunch that only reconstructs `--name`/`--model`/
`--permission-mode`/`--remote-control` silently drops every OTHER real launch flag.** CireSnave launches
every lane with `--dangerously-load-development-channels server:claude-peers` - the flag that makes
`claude-peers` PUSH incoming messages into a session. Without it, a relaunched lane can still SEND
messages but never RECEIVE notifications. **Fixed, two parts:**

1. **Record it.** `LaneState.launch_args` (via `ClaudeCliFlags.launch_args`, set inside
   `parse_claude_cli_flags`) now carries the real, full argv `claude.exe` was launched with, read at
   `SessionStart` from the same OS process-list lookup `remote_control`/`permission_mode` already use.
   Same "always fresh, never preserved" rule `remote_control` follows, for the same reason: a stale
   argv from a prior launch would be exactly as wrong as inventing one.
2. **Rebuild from an ALLOWLIST, never a blind pass-through** (`main.rs`'s `extra_launch_args`).
   Carried over verbatim: `--dangerously-load-development-channels <spec>…` and `--add-dir <dir>…`
   (variadic - consumed until the next `-`-prefixed token), `--mcp-config <file>` and `--settings
   <file>` (exactly one value each). `--model`/`--permission-mode`/`--remote-control`/
   `--dangerously-skip-permissions` are recognised-and-intentionally-NOT-duplicated here - they're
   already carried over by `claude_argv` itself, from their own dedicated state fields with proper
   fallback rules richer than a blind re-parse would give (and `--dangerously-skip-permissions`
   specifically is already represented as `permission_mode: Some("bypassPermissions")`, so re-emitting
   the literal original flag would just restate the same fact). **Dropped, with their own value
   correctly consumed so it's never misread as a stray positional or another flag's value:**
   `--resume`/`-r` (optional value - CireSnave's own real command line has it bare, as the LAST token),
   `--continue`/`-c`, `-p`/`--print`, `--session-id` (optional value), `--fork-session`, `--name`
   (dropping the ORIGINAL is what keeps `claude_argv`'s own fresh `--name` from being emitted twice),
   and any leftover positional (the original prompt, if one was captured). **Any other, unrecognised
   flag is dropped and reported** (never passed through blindly) - its presumed arity is unknown, so
   only the flag token itself is dropped, never a guessed value alongside it. The `;` gate (§5, `wt.exe`'s
   own command separator) scans `claude_argv`'s full output, so it already covers these new elements
   too - proven by a test that injects a `;` into a `--dangerously-load-development-channels` value and
   confirms the whole relaunch is refused, the same as for `cwd`/`model`/`permission_mode`.

⚠️ **REVISED TWICE, same day (PM findings, 2026-09-18, fourth then fifth real-restart retests) - the
second revision corrects the first's own mistake, kept here rather than silently overwritten.**

**Fourth retest**: a live `claude` process is not the same claim as a WORKING session -
`--dangerously-load-development-channels` shows a security confirmation dialog on every start, and the
liveness check up to that point (§5, "process alive, then still alive a follow-up interval later") would
have logged a lane stuck at that dialog as a success. First fix: require the target role's OWN state file
to show a `session_id` that differs from the one that was killed, whose `updated_by_event` is something
AFTER `SessionStart` (a real `UserPromptSubmit` or later event) - `SessionStart` alone doesn't prove it,
since a session stuck at the dialog still fires that event and then never gets past it.

**⚠️ That first fix's OWN mistake, caught by CireSnave within the same day**: an earlier draft of THIS
section (§5 above) had reasoned its way to also taking `--dangerously-load-development-channels` OUT of
the carry-over allowlist entirely, on the theory that a flag which only produces a dialog is better
dropped than carried. **That reasoning was wrong** - `channels-reference`'s own docs confirm there is NO
bypass during the research preview, and `--channels` (the no-dialog alternative) only accepts plugins on
Anthropic's own allowlist, so it can never substitute for a local MCP server like `claude-peers`. Dropping
the flag would have silently cut every relaunched lane off from Synapse, FAM, and every other non-Claude
agent - the exact fabric the flag exists to keep reachable. **Retracted the same day it was proposed,
before ever merging**: the flag stays in `ALLOWED_LAUNCH_ARG_FLAGS`, carried over verbatim, exactly as the
lane was originally launched.

**Fifth retest, the actual fix**: liveness now has THREE outcomes, not a pass/fail pair -
`RelaunchOutcome::Relaunched` (real progress, as the fourth retest established), `AwaitingConfirmation`
(the process IS alive, past `PROGRESS_TIMEOUT` (~20s) with no progress, AND its own launch carried the
dev-channels flag - exactly the dialog's shape), and `Err(SessionNeverProcessedPrompt)` (the process died,
or never came up, or is stuck with nothing to explain why). `AwaitingConfirmation` is real, ACTED work -
the kill and the relaunch both genuinely happened - never an error: `log_outcome` records `acted: true`
with "awaiting human confirmation at the dev-channels dialog." The moment `AwaitingConfirmation` is FIRST
detected (not only once the full ~15 minute total timeout elapses), a callback fires exactly once and
`lane-restart` prints a machine-readable JSON status line
(`{"event":"lane-restart-status","role":...,"outcome":"awaiting_confirmation","at":...}`) - so whatever is
watching this process's stdout (the PM lane, today) can notify CireSnave promptly that a human needs to
go confirm the dialog. Polling then CONTINUES (never re-notifying) up to the ~15 minute total: a human
confirming flips the outcome to `Relaunched`; reaching the full timeout still stuck returns
`AwaitingConfirmation` as the final outcome instead.

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
- ⚠️ **REVISED (PM finding, 2026-09-18, fourth interactive retest): `model` IS present in a real
  `SessionStart` payload after all** - the key list logged from a live interactive Remote Control
  session was `[cwd, hook_event_name, model, scratchpad_dir, session_id, source, transcript_path]`,
  with `permission_mode` absent (consistent with §10.3's third-retest finding) but `model` present.
  Recorded here exactly as measured, at `C:/Projects/.lane-state/diag-parent.log`,
  18:28:21Z. **Only the key names were logged, not the value's shape** - `lane_state_writer.rs`'s
  `ModelField` enum accepts either a plain string or `{id: string}` defensively, marked NOT CONFIRMED
  in its own doc comment until a real capture pins the shape down.
- ⚠️ **Same retest: no hook field carries `remote_control` at all**, at `SessionStart` or any other
  event - a session launched with `--remote-control` still recorded `remote_control: false` under the
  original design. Fixed by deriving it (and, as a fallback source, `permission_mode`) from the
  `claude` process's own launch command line, read via `sysinfo::Process::cmd()` against the pid
  `claude_parent_pid` already verified (§10.3) - never against an unverified process. The pure parser
  (`parse_claude_cli_flags`) recognizes `--remote-control`, `--permission-mode <value>` and
  `--permission-mode=<value>` both, and `--dangerously-skip-permissions` (mapped to
  `bypassPermissions`, `sessions.md`'s own name for that mode). Unlike `model`/`permission_mode`,
  `remote_control` is **never preserved from a prior session's state** - it is read fresh from every
  launch's own command line, since a stale carried-forward value would be exactly as wrong as
  inventing one.
- ⚠️ **KNOWN LIMIT, low priority (PM finding, 2026-09-18, post-merge gate read of PR #61):**
  CireSnave's own `settings.json` carries `remoteControlAtStartup: true`, so a lane can have Remote
  Control with no `--remote-control` flag on its launch command line at all - this field then reads
  `remote_control: false` even though the session genuinely has it. Not wrong *in practice*: a
  relaunch picks Remote Control back up from that same setting regardless of what this field says.
  But the state file itself doesn't prove which is true. **Also fixed in the same gate read: the
  equals form (`--permission-mode=value`) wasn't being parsed at all** - only the space-separated
  form was; both now parse identically.
- ⚠️ **ROOT CAUSE FOUND (PM finding, 2026-09-18, first real restart attempt): `remote_control` reading
  `false` on a real `--remote-control` session, and `cwd_of` failing with "could not read the cwd of
  pid ..." on every real identity check, were the SAME bug.** `System::refresh_processes` (no
  `_specifics`) uses sysinfo 0.39.6's own default `ProcessRefreshKind`, which leaves `cwd` and `cmd` at
  `UpdateKind::Never` - confirmed by reading sysinfo's own source, not by guessing. `exe` happened to
  still work by luck (the same default explicitly overrides it to `OnlyIfNotSet`, which fetches it on
  a fresh, never-yet-set `System` - the pattern every call site here already used). `cwd` and `cmd`
  have no such override, so `cwd_of` (§2's identity check) and `cmdline_of` (`ClaudeCliFlags`
  derivation, above) were both reading an always-empty field in production, on every real invocation,
  while every existing unit test used `FakeFacts`/`FakeAncestry` and so never touched the real sysinfo
  default at all. **Fixed** in both `facts.rs` and `lane_state_writer.rs`: `refresh_processes_specifics`
  with an explicit `ProcessRefreshKind` that sets `cwd`, `cmd`, and `exe` to `UpdateKind::Always`,
  so none of the three depend on an unstated default again. Proven with two new REAL-child-process
  integration tests (`cwd_of_reads_a_real_spawned_childs_actual_working_directory`,
  `real_parent_process_cmdline_of_reads_a_real_spawned_childs_actual_argv`) that spawn a genuine child
  with a known cwd/argv and read it back through the real `SysinfoFacts`/`RealParentProcess` - the one
  category of test this crate's existing fakes structurally cannot catch.

### 10.2 Role: `LANE_ROLE` override, falling back to the `cwd` leaf

**REVISED (PM finding, 2026-09-18):** the cwd-leaf-only version of this proposal gave the PM's own
lane `"projects"` (its cwd is `C:/Projects` itself, not a per-lane subdirectory) - a real bug, not a
hypothetical one. Fixed: `LANE_ROLE`, an environment variable set once per lane wherever that lane's
own launch environment already lives, always wins when present; the lowercased leaf directory name
of `cwd` (`C:/Projects/OverMind` → `overmind`) remains the fallback for every lane whose directory
name already IS its role - which is most of them, so most lanes need no new setting at all.

### 10.3 The hook command: `lane-restart state <event>`

A subcommand of this same crate (`crates/lane-restart/src/lane_state_writer.rs`), not a separate
script. Common hook input JSON comes on stdin, exactly as `hooks.md` documents; the event name is
passed on the command line (§10.4 - as a shell-form string, not `args`, per that section's own
finding).

⚠️ **REVISED (PM finding, 2026-09-18, third interactive retest): only `hook_event_name`,
`session_id`, and `cwd` are required from that JSON - everything else is `Option<T>`, absent rather
than guessed when missing.** A real `SessionStart` payload does not include `permission_mode` -
parsing it as required failed against live input, not a hypothetical (`missing field
'permission_mode'`). "In the documented common-fields table" was never the same claim as "present on
every event, always"; `hooks.md`'s own table doesn't promise that, and this design's first draft
read it as if it did. `permission_mode` now follows the same rule `model` already did: `None` until a
real payload carries it, preserved across a later `SessionStart` that doesn't repeat it, never
invented. The relaunch command (§5) omits `--permission-mode` when it's `None`, the same way it
already omits `--model`.

⚠️ **REVISED (PM finding, 2026-09-18): the acceptance check for this whole design is a real state
WRITE, not just the parent-chain diagnostic.** §11.1's first retest confirmed the parent chain
matched what was recorded, and only THEN discovered that no `.lane-state/*.json` file existed at
all - the diagnostic checked the ancestry, never checked that the intended side effect actually
happened. Any future retest of this hook (this one, or a different one) must confirm the state file
is written and updated correctly, not stop at confirming the process ancestry looks right.

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
for the same reason: portfolio-wide runtime tooling, kept out of every git repo.

⚠️ **REVISED (PM finding, 2026-09-18): exec form's `args` are not passed - confirmed live, not
assumed.** The exec-form block below (previously `"args": ["state", "SessionStart"]`) wrote NO state
file in the real interactive retest. Reproduced directly: running `lane-restart.exe` with zero
arguments gives the identical failure - it falls into the restart-CLI path, prints `--role <name> is
required`, exits 1 - matching exactly what the hook produced. **Conclusion: Claude Code does not
deliver `args` to this hook's command process.** A non-zero hook exit is non-blocking, so this failed
completely silently; no state file, no error visible anywhere short of checking for the file's
existence. **Fixed: use SHELL form - one command string, no `args` key** - the shell splits it into
argv itself, which is what actually reaches the binary. This is safe here specifically because
nothing lane-controlled ever goes into this string: every token is a literal this design authored,
never data from a hook's own JSON input or a lane's state file - the metacharacter-injection risk
`main.rs`'s relaunch command guards against (§5) doesn't apply to a string with no runtime content at
all.

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
        "command": "C:/Projects/.claude-hooks/lane-restart.exe state SessionStart" }] }
    ],
    "UserPromptSubmit": [{ "hooks": [{ "type": "command",
      "command": "C:/Projects/.claude-hooks/lane-restart.exe state UserPromptSubmit" }] }],
    "PreToolUse": [{ "hooks": [{ "type": "command",
      "command": "C:/Projects/.claude-hooks/lane-restart.exe state PreToolUse" }] }],
    "Stop": [{ "hooks": [{ "type": "command",
      "command": "C:/Projects/.claude-hooks/lane-restart.exe state Stop" }] }],
    "SubagentStart": [{ "hooks": [{ "type": "command",
      "command": "C:/Projects/.claude-hooks/lane-restart.exe state SubagentStart" }] }],
    "SubagentStop": [{ "hooks": [{ "type": "command",
      "command": "C:/Projects/.claude-hooks/lane-restart.exe state SubagentStop" }] }],
    "PostModelSwitch": [{ "hooks": [{ "type": "command",
      "command": "C:/Projects/.claude-hooks/lane-restart.exe state PostModelSwitch" }] }],
    "SessionEnd": [{ "hooks": [{ "type": "command",
      "command": "C:/Projects/.claude-hooks/lane-restart.exe state SessionEnd" }] }]
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
single direct hop.

**REVISED again (PM finding, 2026-09-18, second retest): the exec-form command §10.4 originally
proposed doesn't work at all** - Claude Code does not deliver `args` to a command hook, confirmed
live (the exec-form hook wrote no state file; running the binary with zero arguments reproduces the
identical failure). §10.4 now uses shell form (one command string, no `args`) instead. **Also found
by this same retest**: the earlier "pass" only checked the parent chain, never checked that a state
file actually got written - §10.3 now records that the real acceptance check is a state WRITE, not
the diagnostic alone. Both fixes need their own re-run before this step is complete.

1. Pick the first lane to receive the hooks (recommend: whichever of OverMind/Synapse/the PM is
   least busy at the time).
2. Install *only* the diagnostic parent-chain check (§10.3's verification script, not the real
   `lane-restart state` hooks yet) on that ONE lane's own settings, for a single event
   (`SessionStart` is enough) - using the same SHELL-form command string §10.4 now installs (not the
   exec-form array that was already shown not to work).
3. Restart that lane normally (however it's normally started/reconnected, Remote Control included)
   and confirm BOTH: `claude_parent_pid`'s fixed logic (walk-past-shells, refuse-on-stranger)
   resolves to the real `claude.exe` pid, AND a real `.lane-state/<role>.json` file is actually
   written with that pid in it - the chain alone is not sufficient evidence, per §10.3.
4. Remove the diagnostic hook. Only if step 3 confirms both does step 11.2 proceed on that lane.

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
