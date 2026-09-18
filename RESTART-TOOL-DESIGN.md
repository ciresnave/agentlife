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

Verified against `sessions.md`:

- **Session ID, model, and permission mode are automatically restored** by `claude --resume
  <session-id>` from a terminal, with documented exceptions (a retired/disallowed model; explicit
  override flags) - the tool passes no flags for these unless the recorded state contradicts what a
  bare resume would restore.
- **`--mcp-config`, `--settings`, `--plugin-dir`, `--fallback-model`, and `--add-dir` are NOT
  auto-restored.** If a lane's original launch used any of these, the state file must record them
  (extending the schema in §1), and the tool re-passes them explicitly.
- **Remote Control reconnects automatically on resume**, per `remote-control.md`: *"When you resume a
  conversation with `claude --resume` or `claude --continue`, Claude Code reconnects to the Remote
  Control session recorded in that conversation."* One documented failure mode: *"Another Claude Code
  on this machine still has the session... Run `/remote-control` there to move it"* - the tool cannot
  resolve that itself; it launches, and if RC didn't come back, that surfaces the same way it would
  to a human doing this manually.
- **The launch command:** `claude --resume <session_id> --name <name> "read <role> HANDOFF and
  continue"`, run in the recorded `cwd`, in a new visible terminal window (platform-specific: this
  needs a real terminal-launch mechanism per OS - Windows first, since that's this machine).
- **No `--continue`**: the task specifies restarting a *specific* lane by its recorded session, not
  "whatever was most recent in this directory" - `--resume <session_id>` is the correct, unambiguous
  form.

## 6. Bulletproof requirements (from the task, restated as testable properties)

1. **Positive identification** — §2's four-part check, every time, no exceptions, including for
   self-restart (a lane still shouldn't be able to accidentally target a different process's PID
   through a bug in its own state file).
2. **Dry-run mode** — `--dry-run` performs every check, prints exactly what it would do (kill PID X,
   wait, launch `claude --resume ...` in cwd Y), and does neither. **DECIDED (PM, 2026-09-18):**
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
