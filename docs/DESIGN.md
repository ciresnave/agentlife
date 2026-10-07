# agentlife — design

> **REVISED 2026-10-04: see `DESIGN-REVISION-1.md`.** It supersedes §1 (roster), §2.1–2.4 (what restore
> reads), §4.1/§4.3/§4.4 (trust model) and the roster-lost row of §7. The rest of this document stands.
> The sections it supersedes are left as written, as the dated record of the first proposal.


**Status: PROPOSAL for PM approval. No feature code until approved.** Written 2026-10-03 by the
`agentlife` lane against the PM's `[TASK] design-document`.

**Evidence base.** Code and spec: OverMind `origin/main` = `c6c8c1e` (read with `git show`, never its
working tree): `RESTART-TOOL-DESIGN.md`, `WITH-SECRET-DESIGN.md`, `crates/lane-restart/src/*`,
`crates/with-secret/src/*`. Runtime facts: `C:/Projects/.lane-state/*` and the live process table and
claude-peers broker, all measured **2026-10-03 between ~15:32Z and ~15:40Z** and so only true of that
minute. Where a claim is *not* measured it is marked **UNVERIFIED** and appears again in §9 as something
the real test must settle.

---

## 0. Findings that change the brief

Read these first. Each one contradicts something the brief, the README or a spec assumes.

1. **lane-restart has no way to start a lane that is not already running.** Its only action is
   *kill, then relaunch*: `authorize::decide` needs a live pid, an `updated_at` under 120 s old and a
   transcript, and `relaunch::kill_and_relaunch` calls `facts.kill_verified` before `spawn_relaunch`
   (`main.rs`, `mod relaunch`). After a Windows update every pid is dead, so *every* lane-restart
   precondition fails by design. `restore` is new code, not a loop around lane-restart.
2. **There is no usable roster today.** `.lane-state/` holds **43** files; **15** `claude` processes
   are live, one of them this lane. Only **14** files are named for a live lane. The other **29** are not
   lanes: `resolve_role` (`lane_state_writer.rs`) names a lane from the *current* cwd's leaf, so every
   time a lane `cd`s into a worktree or subdirectory its hook writes a new "role". One pid (57564, the
   `mlmf` lane) owns **21** files, its own plus 20 worktree names (`codacy-refactor`, `gguf-config`,
   `version-0.5.5`, …); the ThinkersJournal and Humboldt lanes' dead pids own `api`, `web`, `shared`,
   `backend`, `frontend`, `routes`, `worker`. Nothing deletes them: `SessionEnd` deletes the file, but a kill
   (`taskkill`, a Windows update) never fires it. (Counts: 43 files, 14 named for a live lane, 21 with
   pid 57564, measured 2026-10-03 ~15:40Z.)
3. **Two live lanes can share one state file.** `auth-framework` (pid 32524) and the lane started with
   `-n auth-framework-deps` (pid 27112) have the same cwd, so one role, one file, which records only
   32524. Role-by-cwd cannot tell them apart; the roster needs an explicit id.
4. **`name` is never recorded.** All 43 files have `"name": null`; `apply_event` copies `name` from the
   previous file and nothing ever sets it. Hand-started lanes use `-n <name>` (the short flag):
   `pm.json` has `-n PM` in `launch_args` and `name: null`. `extra_launch_args` neither carries `-n` nor
   lists it as known, so it is dropped as "unknown flag" and its value silently dropped as a bare
   positional. Relaunch then names the lane by `role`.
5. **"The mode it last ran in" is recorded as "the mode on its command line", not what it ran in.**
   `permission_mode` comes from `--permission-mode`/`--dangerously-skip-permissions` in `launch_args`
   (`parse_claude_cli_flags`). A lane started with no mode flag has `permission_mode: null`
   (7 of 43 files, including `coderipper`, `fuel`, `mlmf`, `baracuda`, `kiss`); a mode changed inside the
   session is invisible.
6. **Nothing enforces "only the PM may run without prompts".** `grep -i bypass` over `authorize.rs`,
   `main.rs`, `facts.rs`, `state.rs` finds nothing (control: the same grep finds 15 matching lines in
   `lane_state_writer.rs`, which only *parses* the flag). `claude_argv` passes whatever `permission_mode`
   is recorded. It holds today only because only `pm.json` says `bypassPermissions`. agentlife has to
   enforce this itself.
7. **The dev-channels approval is neither permanent nor universal.** CireSnave's belief ("already
   approved once and permanent") is half right; §4.5 has the verification. Short version: it expires
   **2027-03-19**, answers the dialog **only inside a tab started by `lane-restart host`** (never for a
   hand-started lane), and needs `gh` + network at the instant the host starts, failing closed to a
   dialog if either is missing. **That has already happened once:** 2026-10-01T23:13Z, `APPROVALS 0
   active - … gh timed out after 15s` (`host-overmind-42100.log`), so OverMind's relaunch sat at the
   dialog and the `[ASK]` also failed (`no live claude-peers session in
   C:\Projects\coderipper\.claude\worktrees\coderipper-reachability-m1 (the pm lane's cwd)`, because
   `pm.json`'s cwd had drifted into a worktree).
8. **Peer ids and peer pids are not lane pids.** A claude-peers peer is a `bun.exe` child of the lane's
   `claude.exe` (verified 3 of 3: peer pid 38504→parent 27996 = PM; 22636→32524 = auth-framework;
   4108→22196 = overmind). lane-restart's `notify` matches peers by **cwd**, which returns *two* peers for
   `C:\Projects\auth-framework`. Joining on **parent pid** is exact.
9. **The "fleet" is 15 lanes today, not 40.** 40 is a test target, not a measured population.
10. **`relaunch` lives inside `main.rs`** (a private `mod`), not in the `lane-restart` library
    (`lib.rs` exports `approvals authorize facts handlers host lane_state_writer log notify paths state
    tab_close`). agentlife cannot depend on it without an OverMind PR that moves it (§8).

---

## 1. The roster

### 1.1 What a lane records today (measured)

`C:/Projects/.lane-state/<role>.json`, written by the `lane-restart state <event>` hook from inside the
lane. Fields (`state.rs`): `role, session_id, pid, cwd, name, model, permission_mode, remote_control,
busy, subagents_running, no_background_shells, launch_args, updated_at, updated_by_event`.

| need | recorded? | verdict (measured 10-03) |
|---|---|---|
| lane identity | `role` = `LANE_ROLE` env, else lowercased cwd leaf | **wrong key** (§0.2, §0.3). Also the restart log has `Unpopped` and `unpopped` as different roles. |
| launch directory | `cwd` (launch dir after the 10-03 `recorded_cwd` fix) | usable, but a worktree-cwd lane gets a *new file* with that cwd |
| model | `model` from `SessionStart` | present in 17 of 43 files; full id (`claude-sonnet-5-5`) although the command line says `--model sonnet` |
| permission mode | `permission_mode` from flags | launch flag only; null when no flag (§0.5) |
| name | `name` | **never set** (§0.4) |
| Remote Control | `remote_control` from `--remote-control` | `false` everywhere; CireSnave's `remoteControlAtStartup: true` is invisible (known limit in the OverMind spec §10.1) |
| channels / extra flags | `launch_args` = full argv | **complete and good**; the one field restore can rebuild from |
| current peer id | not recorded | missing (§5) |
| handoff path | not recorded | missing; the relaunch prompt is the string `read <role> HANDOFF and continue` and the lane resolves the file |
| start order, group | not recorded | missing |
| "is this lane meant to run?" | not recorded | missing: nothing distinguishes a lane from a sub-session |

### 1.2 What survives a reboot

Files survive; **everything they say about liveness does not.** After a Windows update all 43 files
describe dead pids, `busy` may read `true` forever (a killed lane never fires `Stop`), and `SessionEnd`
never ran. What is trustworthy after a reboot: `cwd`, `launch_args`, `model`, and the last
`permission_mode`, as *claims the lane made about itself* (the lane-restart spec's own caveat). What is
not: `pid`, `busy`, `subagents_running`, `no_background_shells`.

Hook reliability is also imperfect: `.lane-state/hook-errors.log` (4.2 KB, 2026-10-02) is full of
`timed out waiting for the lane-state lock` for `PreToolUse`, `Stop`, `SubagentStop`. A lost `Stop`
leaves `busy: true`; a lost `PreToolUse` leaves `busy: false` while working, the unsafe direction. `down`
must therefore never trust `busy` alone (§3).

### 1.3 Proposal: a curated roster, separate from observed state

Two files, two owners, because they have opposite trust properties:

- **`C:/Projects/.agentlife/roster.json` — durable, declared, signed.** What *should* run. Written only
  by `agentlife roster …` commands, edited by a person or by an agent *proposing* a change that a person
  approves (§4.3). Kept out of every repo, like `.lane-state/`.
- **`C:/Projects/.lane-state/<role>.json` — observed, runtime, as today.** What *did* run. The roster
  never reads liveness from it without verifying against the process table.

```jsonc
{
  "schema": 1,
  "defaults": { "batch_size": 3, "batch_delay_secs": 45, "liveness_timeout_secs": 120,
                "stop_after_failed_batches": 2, "handoff_stale_after_secs": 600 },
  "lanes": [{
    "role": "pm",                         // explicit id; [A-Za-z0-9_-]{1,64} (lane-restart's valid_identifier), unique
    "order": 0,                           // 0 = first, alone. Ties start in one batch.
    "cwd": "C:/Projects",                 // launch directory
    "name": "PM",                         // the --name / -n value; unique per (cwd,name)
    "model": "sonnet",                    // passed through verbatim
    "mode": "bypassPermissions",          // APPROVED CEILING (§4). null = Claude Code's default.
    "remote_control": false,
    "channels": ["server:claude-peers"],  // → --dangerously-load-development-channels
    "extra_args": [],                     // allowlisted flags only (lane-restart's ALLOWED_LAUNCH_ARG_FLAGS)
    "handoff": "C:/Projects/PM-HANDOFF.md",
    "env": { "LANE_ROLE": "pm" },
    "enabled": true
  }]
}
```

`launch_args` stays the source for `channels`/`extra_args` *when seeding* a roster, never at restore
time. Restore uses only the signed roster, so a lane cannot change what restore launches by changing its
own state file (the escalation route §4.1 closes).

**Seeding.** `agentlife roster scan` joins live `claude` processes to state files by *pid and process
start time* (not role), proposes one roster entry per live lane (`name` from `-n`/`--name`, mode from
flags, channels from `launch_args`), and flags lanes it cannot name (the `-n`-less ones: `coderipper`,
`kiss`, `auth-framework` today). A person edits and approves. It must **never** turn the 30 phantom
files into lanes: a state file is a roster candidate only if its pid is a live `claude` whose start time
matches, or the file is the only one for a roster cwd.

**Hook change this implies (not made here).** `lane-restart state` resolves the role from cwd and
`LANE_ROLE`; once a roster exists it should resolve `cwd → role` through the roster (a worktree under a
roster cwd belongs to that lane), which ends the phantom files. That is an OverMind/agentlife
coordination item (§8), not a silent change to the installed hook.

---

## 2. `agentlife restore`

### 2.1 Trigger

- **Manual:** `agentlife restore [--dry-run] [--only role,…] [--batch N] [--delay S]`.
- **At logon:** a per-user Task Scheduler task, `ONLOGON`, "run only when user is logged on", running
  `agentlife restore --from-logon`. Created by `agentlife install-task` (a person runs it once;
  `schtasks.exe` exists at `C:\WINDOWS\system32\schtasks.exe`, measured). Interactive-session-only is
  required: `wt.exe` tabs need the desktop. **UNVERIFIED:** whether `wt.exe`
  (`%LOCALAPPDATA%\Microsoft\WindowsApps\wt.exe`, an App Execution Alias, measured present) resolves
  inside a Task Scheduler launch. The real test (§9.2) must cover it, and the fallback is a launch
  through `cmd /c start wt.exe`.
- `--from-logon` waits (bounded, default 120 s) for the preconditions that are otherwise silent failures:
  network (`gh api rate_limit` succeeds, because the approvals fetch needs it, §4.5), the claude-peers
  broker answering on `127.0.0.1:7899`, the installed `lane-restart.exe` hook binary present.

### 2.2 Algorithm

```
load roster; verify per-lane signature (§4.3)     → unsigned/invalid lanes are skipped and reported, never started
preflight (network, wt.exe, binaries, free disk)  → each failure named in the report; none is fatal unless it blocks all lanes
for each enabled lane: classify  ALIVE | ABSENT | AMBIGUOUS   (§2.4)
plan: sort by (order, role); PM alone as batch 0; then batches of batch_size
for batch in plan:
    launch every lane in the batch (§6)            → record launch time
    wait until every lane in the batch is SETTLED  (§2.3) or liveness_timeout_secs passes
    sleep batch_delay_secs
    if failed batches in a row ≥ stop_after_failed_batches: stop, report "halted", start nothing more
final report (§2.5)
```

**PM first, and gated.** Batch 0 is the PM alone and the run does not continue until the PM is `Working`
or has hit its timeout; a lane that starts before the PM has nothing to report to (§2.5) and the `[ASK]`
that lane-restart's host sends on an unanswered dialog (`host.rs` `handle_unhandled`) goes to the PM.
If the PM fails to start, the run continues with the remaining lanes (CireSnave's stated problem is that
*he* starts them by hand; a PM failure must not hold fourteen others) and the failure leads the report.

### 2.3 Liveness check — reuse lane-restart's method

`relaunch::wait_for_relaunch_liveness` (`main.rs` ~L1200) is the method. A lane is judged by its **own**
new state file, not by "a process exists":

- **`Working`**: the role's state file shows a `session_id` ≠ the pre-launch one **and**
  `updated_by_event` ≠ `SessionStart` (a lane stuck at a startup dialog still fires `SessionStart`).
- **`AwaitingDialog`**: no progress after 20 s, a live `claude` whose start time ≥ launch time − 5 s
  exists in the cwd (`find_claude_process_in`; the 5 s margin is the clock-source slack found on CI), and
  the argv carried the dev-channels flag.
- **`Failed`**: the process died, or never appeared, by the timeout; or died after appearing.

Differences from lane-restart, all because restore starts *many* lanes: (a) the **timeout is per
batch, default 120 s**, not 15 minutes: a human-gated dialog must not stall fourteen lanes, so it counts
as `AwaitingDialog` (reported, batch moves on); (b) the state file is keyed by **roster role** (§1.3);
(c) the pre-launch `session_id` comes from the last observed file; after a reboot that value is simply
"anything different".

**Settled** = `Working`, `AwaitingDialog`, `Failed` or timed out. The next batch starts only when the
previous one is settled *and* the delay has passed.

### 2.4 Idempotence — "never start a lane whose role+cwd is alive"

"Alive" must be decided without trusting a state file's pid (recycled) and without trusting cwd (live
`cwd` follows the session's `cd`, the OverMind spec §2 finding). Rule, strongest evidence first:

1. A live `claude` process whose `(pid, start_time)` equals the pair last recorded for this role →
   **ALIVE**.
2. Else a live `claude` whose command line carries this roster entry's `name` as `--name`/`-n` **and**
   whose launch cwd (from its state file) matches → **ALIVE**.
3. Else a live `claude` in this cwd that no roster entry claims → **AMBIGUOUS**: not started, reported
   ("something is already running in `C:/…`; start it anyway with `--force-role`"). Today `coderipper`,
   `kiss` and `auth-framework` are exactly this case until they are named in the roster (they carry no
   `-n`).
4. Else **ABSENT** → start.

**How exact "alive" is (added 2026-10-07).** The `(pid, start time)` pair is exact on Windows, where the start
time is the creation time. Off Windows it is best-effort (`sysinfo` derives it from a boot time that can drift
by a second; unproven, tracked in an issue), and in whole seconds on every platform, so a pid recycled within
the second of the process it replaced is indistinguishable. No tolerance is applied, because it would widen
that window. A green Linux CI leg checks the logic, not Linux identity.

The failure direction is **never start a duplicate**: two sessions in one cwd would both write
`.lane-state/<role>.json` and both claim the same peer role, which is §0.3.

### 2.5 Final report and notification

`C:/Projects/.agentlife/reports/restore-<UTC>.json`, one entry per lane: `role, classification,
launched_at, outcome (Working|AwaitingDialog|Failed|Skipped:<why>), seconds_to_working, handoff_age,
handoff_stale, errors`, plus a summary line and the preflight results. Also appended to the append-only
`agentlife.log` (same discipline as `restart.log`: every refusal is written, not just actions).

**Notification to CireSnave.** UNVERIFIED which channel works unattended. The channels that exist:

- *A `[NOTICE]` to the PM lane through the broker* (`POST /send-message`, the exact path `notify.rs`
  uses; verified in code). Works only if the PM is up, which batch 0 is meant to ensure; the PM then
  relays to CireSnave as it does today.
- *A direct local notification* (Windows toast) when the PM did not come up or the run halted.
  **UNVERIFIED** from an unpackaged Rust binary (toasts need an AppUserModelID); fallback is a plain
  `msg`-style dialog or a file in a place he looks.
- Claude Code's own `PushNotification` is a harness tool, not callable from a Rust binary.

This is an open question (§10, Q4) because the answer decides whether a halted restore at 3 a.m. is
heard at all.

---

## 3. `up`, `down`, `list`

- **`agentlife list`** — roster ⨝ observed ⨝ process table ⨝ peers (§5): role, classification, pid,
  uptime, mode, peer id, HANDOFF age, `busy` *with its age and a "possibly stale" flag*. Read-only, no
  consent.
- **`agentlife up <role>`** — one lane through §2.4 → §6 → §2.3. Same policy as restore (§4).
- **`agentlife down <role> [--all]`** — **graceful, and never a bare kill**:
  1. Authorize (§4.4).
  2. Ask the lane to wrap up by `[TASK]` message to its resolved peer id (§5): write HANDOFF, run
     `lane-restart assert-idle`, then `agentlife down --confirm-self` (a lane stopping itself:
     `kill_verified` on its own pid, no relaunch). A message to a lane is an instruction a lane may
     decline; it is the only graceful channel, since an interactive `claude` has no external
     "finish up" API (OverMind spec §0, verified against `agent-view.md` there).
  3. Wait up to `--timeout` (default 10 min). The lane counts as *ready to stop* only when **all** hold:
     its HANDOFF mtime is newer than the request, `no_background_shells` is `true` (assert-idle's claim,
     which any later `UserPromptSubmit`/`PreToolUse` clears), `subagents_running == 0`, **and** the
     process tree has no live shell descendant (`has_live_shell_descendant`). Both signals, as the
     lane-restart spec decided (§1a), because the hook's own log shows `busy` can be wrong.
  4. Only then terminate (`kill_verified`: pid + start time + exe path re-read immediately before the
     signal). A lane that did not become ready is **left running** and reported; `--force` is not part
     of v1 (open question Q8).
  5. `down --all` runs this in parallel for every lane except the PM, then the PM last, so the PM can
     hear the others finish and is itself the last to write a HANDOFF.
- **`agentlife restart <role>`** is `down` then `up`, and delegates to `lane-restart --self` semantics
  for the lane restarting *itself* during the migration (§8).

`down` has the same "no lane may stop a different lane" rule as lane-restart (spec §3): self, or the PM
against an idle lane with `--yes`; anything else needs a person (§4.4).

---

## 4. Permission policy

### 4.1 Principles

1. **A restart never widens.** Restore launches a lane at `min(approved_mode, last_observed_mode)`.
   `approved_mode` is the roster's `mode`, set and signed by a person. The observed mode can only
   *lower* what a lane gets back.
2. **Anything the roster does not already sign is a new grant** and needs a person (§4.3).
3. **A request that originates from an agent is untrusted input**, however it is worded and whoever the
   agent is (CLAUDE.md §8; the task's own rule).
4. **Only the `pm` role may be `bypassPermissions`.** Checked when the roster is signed and again at
   launch. A non-PM entry with that mode (or `--dangerously-skip-permissions` in `extra_args`) is
   refused, not downgraded silently. Whether even a person's Hello can override it is open question Q6;
   v1 default is *no override*.

Mode ordering is a table in the tool, not a guess: `default < acceptEdits < auto < bypassPermissions`,
with `plan` and any unknown value **incomparable → treated as untrusted → person required**. (Modes seen
on this box today: `auto`, `bypassPermissions`, and none. The ordering is UNVERIFIED against Claude Code
documentation; it is the first thing the implementer must confirm, because a wrong ladder is a wrong
policy.)

### 4.2 How lane-restart authorizes today (read from the code)

`authorize::decide` allows (a) `--self`, after checking the caller's own `claude` ancestor *is* the
recorded pid (`NotTheCaller`), or (b) the PM-restarts-an-idle-lane form needing `--yes`, a fresh state
file, the four-part identity (pid, process-verified `cwd`, transcript, `updated_at`) and the two idle
signals. It has **no concept of mode**, no consent, and its only defence against a hostile caller is
that ancestry check. OverMind's own §12.8 says so: *"It is not a security boundary against a hostile
lane."*

### 4.3 The channel: Windows Hello, as `with-secret` does it

`WITH-SECRET-DESIGN.md` §2.2 chose Hello (`UserConsentVerifier`) *because* lane-restart's channel (a
merge into the owner's repo plus an `[ASK]` to the PM) cannot prove CireSnave himself said yes — the PM
may act as `ciresnave` on GitHub. The prompt needs his PIN or biometric, so **no agent can answer it,
the PM included**; if he is away it times out and the command fails. I propose agentlife reuse that
channel for exactly the decisions where *who said yes* matters:

- `hello.rs` (`Owner::Foreground` default) and `consent.rs` (`ConsentRequest` / `prompt_text`) are the
  reusable pieces; `approval.rs`'s HMAC-over-DPAPI-key cache is the model for the signed roster.
- **Approve the roster, not each start.** `agentlife roster approve` shows the diff (lanes added,
  modes raised, flags added, cwd changed) in one Hello prompt and, on yes, HMAC-signs each *approved
  entry* with a DPAPI-protected key, as `with-secret`'s `ApprovalCache` does. Restore (§4.4) starts
  exactly the signed entries at ≤ their signed mode, unattended. Editing any field that could widen
  authority (add a lane, raise `mode`, add `channels`/`extra_args`, change `cwd` or `handoff`) breaks
  that entry's signature until re-approved; lowering a mode or disabling a lane does not.
- **Honest limit, same as with-secret §3:** any process running as this Windows user can read the DPAPI
  key and forge a signature, or send keystrokes to the desktop. Hello defeats an *accidentally or
  casually* misbehaving lane, not a hostile one. This is stated, not hidden.

### 4.4 The decision matrix

| request | originates from | consent |
|---|---|---|
| `list`, `status`, `restore --dry-run` | anyone | none |
| `restore` / `up` for a **signed** entry at ≤ signed mode | the logon task, or a person's own terminal (no `claude` ancestor) | none: the roster *is* the standing approval |
| same | a process with a `claude` ancestor (an agent) | **Hello**, naming the calling lane, target and mode; timeout → refuse |
| any launch of an **unsigned/modified** entry, or at a mode > signed | anyone | **Hello**; unknown mode or non-PM bypass → refuse |
| `down` self | the lane | none (self-stop, §3) |
| `down` another lane | the PM, lane idle, `--yes` | none beyond lane-restart's existing rule; any other caller → Hello |
| `roster approve`, `install-task` | a person | Hello (it *is* the grant) |

**The ancestry test is a convenience, not a boundary.** An agent can launch `schtasks /run`, or a
detached `wt.exe`, and arrive without a `claude` ancestor. What actually protects the grant is that a
signed roster cannot be *widened* without Hello and that a fresh launch of anything unsigned needs Hello
whoever asks. The only thing an agent can do by laundering through the logon task is start lanes the
person already signed, at modes the person already signed.

### 4.5 The dev-channels dialog — verified, not assumed

CireSnave's claim: approved once, permanent, and self-restarting lanes come back with channels active.
What the code and state files show:

- **Channels do come back.** Every one of the 15 live lanes' command lines carries
  `--dangerously-load-development-channels server:claude-peers`. The 9 started by `lane-restart host`
  (positional prompt `read <role> HANDOFF and continue` first) and the 6 hand-started ones (no prompt)
  all have it. `claude_argv` re-emits it from `launch_args` via `ALLOWED_LAUNCH_ARG_FLAGS`.
- **Approval is a file in `ciresnave/ciresnave`**, `.overmind/lane-restart/approvals/claude-peers-dev-channels.json`
  (fetched live with `gh api`, measured present): anchors `WARNING: Loading development channels` and
  `I am using this for local development`, field `Channels = server:claude-peers`, action `"1\r"`,
  `scope.roles = ["*"]`, **`expires_at: 2027-03-19T00:00:00Z`**. Not permanent. After that date every
  restart stops at the dialog.
- **It only fires inside `lane-restart host`.** The host owns a ConPTY and types the key during a 60 s
  window. A hand-started lane (`WindowsTerminal → pwsh → claude`) has no host; CireSnave answers by
  hand. `restore` must therefore launch every lane through a host (`wt … lane-restart host --role … --
  claude …`), as `spawn_relaunch` does. Otherwise restore re-creates the manual work it exists to
  remove.
- **It fails closed on a missing precondition.** The host fetches the approvals *before* spawning
  `claude`, through `gh` with a 15 s timeout, no cache. No network or no `gh` auth at logon → no
  approval loaded → the dialog sits there → after 60 s the host sends an `[ASK]` to the PM's peers; if
  the PM is not up (or *is* that lane, the known gap in OverMind spec §12.6) the dialog is simply on
  screen. Hence §2.1's network wait. This is measured, not hypothetical: see §0.7 (2026-10-01 23:13Z,
  `gh` timed out, approvals 0, `ASK FAILED`). Over the whole 111-line `restart.log`: 47 `relaunched a fresh
  session`, 3 `action failed`, 2 `awaiting human confirmation` (2026-09-19 before the approval existed,
  and the 2026-10-01 case above). The fetch worked on the relaunch before it: `APPROVALS 1 active, 0
  refused, from ciresnave/ciresnave:…@96ed61a33f54` (`host-overmind-9968.log`, 22:08Z).
- **A one-time claude-side bypass would not help.** The OverMind spec §5 cites `channels-reference`:
  there is no bypass during the research preview. The only mechanism is the keystroke approval above.

---

## 5. Peer identity

claude-peers ids rotate on every restart (brief; CLAUDE.md §8). What is stable and joinable:

- **Peer → lane by parent pid.** The broker's `/list-peers` returns `{id, pid, cwd, …}` where `pid` is
  the `bun.exe` MCP server and its **parent is the lane's `claude.exe`** (§0.8, 3 of 3). So
  `peer.pid → ppid → claude pid → (pid, start_time) → roster role` is exact. cwd matching is
  ambiguous (two peers share `C:\Projects\auth-framework`).
- **Roster records the join, never the id as identity.** `C:/Projects/.agentlife/peers.json`
  (runtime, not signed): `{role: {peer_id, claude_pid, claude_start_time, refreshed_at}}`. Refreshed at
  the end of every `restore`/`up`, by `agentlife peers refresh`, and lazily by `resolve`.
- **`agentlife resolve <role>` → current peer id**, re-joined at call time (the stored id is only a cache
  hint). Messaging by role is `agentlife send <role> <text>` (wraps broker `/send-message`, the same
  call `notify.rs` makes, so no claude-peers change is needed).
- **Dependency, not solved here:** the cleanest fix is for claude-peers-mcp to register a stable
  `role`/`name`, which would remove the join. That is a request to its owner (Q10). Until then the
  parent-pid join is the mechanism.

---

## 6. Windows specifics

Launch is `lane-restart`'s proven shape (OverMind spec §5, each point a past failure):

`wt.exe -w <window> new-tab --title <role> -d <cwd> <agentlife|lane-restart.exe> host --role <role> --
claude <prompt> --name <name> [--model m] [--permission-mode p] [--remote-control]
[--dangerously-load-development-channels spec …]`

- **Why a host, a real terminal, not `Command::spawn` of `claude`:** a child with inherited pipes makes
  `claude` run as one-shot print mode and exit after the prompt (spec §5, 2026-09-18 retest 2).
- **Environment:** strip the ten session-identity variables (`SESSION_IDENTITY_ENV_VARS`) or the new
  session believes it is a child of its launcher. From a Task Scheduler launch that list is mostly empty
  but must still run: restore may also be called from inside a lane.
- **The prompt goes first and flags after:** `--dangerously-load-development-channels` is *variadic* and
  swallows a trailing prompt (spec §5; `no_positional_ever_directly_follows_a_variadic_flags_values`).
- **Never `--resume`** (reloads the whole transcript; the cost the restart exists to cut). See Q5 for
  stale-HANDOFF lanes.
- **The `;` hazard.** `wt.exe` treats `;` as its own command separator on top of normal argv passing.
  Every element that reaches `wt` (cwd, name, model, mode, channel specs, **title**, **window**, and the
  prompt) is checked and the launch refused on `;`, *before anything starts* (`first_unsafe_argument`).
  agentlife issues **one `wt.exe` invocation per lane** and never chains lanes with `;`, so it never
  needs the separator itself.
- **Names:** roster `role`/`name` validated to `[A-Za-z0-9_-]{1,64}` (confirmed live in the OverMind
  spec: `a&calc` would have reached `cmd.exe`). Tab title = role. **UNVERIFIED:** that `new-tab --title`
  plus `-w <name>` addresses one shared "agentlife" window rather than opening fifteen. If it does not,
  fall back to `-w new` per lane (lane-restart's behaviour). One window or many is Q9.
- **Working directory:** `-d <cwd>`; `claude`'s own launch cwd is what the roster records.
- **`wt.exe` absent:** detected by a `NotFound` *spawn error*, never guessed from PATH, then
  `conhost.exe <host argv>` with `current_dir(cwd)` (lane-restart's fallback). The report names the
  fallback; the user loses tabs, not lanes. If `conhost.exe` also fails the lane is `Failed` and the run
  continues.
- **Leftover tabs:** unchanged from OverMind §5 (`tab_close.rs`): only meaningful for restart of a
  *hand-started* lane; tool-launched tabs close with their host.
- **Elevation / session:** a task running `whether logged on or not` has no desktop; the task is
  created "only when logged on" (§2.1).

---

## 7. Failure modes, and what is NOT solved

| failure | behaviour |
|---|---|
| **Windows update kills lanes mid-task** | Not preventable. Work since the last HANDOFF exists only in the dead session's transcript. Mitigation: HANDOFF freshness, below. **Not solved**: uncommitted file edits, in-flight background shells, an unanswered peer message. |
| stale HANDOFF | Detected and **reported**, never blocking (below). |
| no HANDOFF file | `Skipped:no-handoff` is *not* the outcome; the lane is started with a prompt that says there is none. Measured: KISS, coderipper, auth-framework, Humboldt have no `HANDOFF.md` at their repo root. |
| lane won't come up | `Failed`, report, next batch proceeds; N failed batches in a row halts the run. |
| stuck at a startup dialog | `AwaitingDialog`; reported; the host's own `[ASK]` goes to the PM. |
| network down at logon | bounded wait (§2.1); then lanes start and the dev-channels dialog is unanswered (§4.5). Reported as a *preflight failure*, not lane failures. |
| a different process holds a lane's pid/cwd | §2.4 identity rules; AMBIGUOUS → not started. |
| two `restore` runs at once (logon task + a person) | a lock file `C:/Projects/.agentlife/restore.lock` with the owner's pid + start time; a second run waits or exits, and reports which. |
| the machine is mid-update when the task fires | not detected. **Not solved**: restore can start lanes into a pending shutdown. |
| clock/process-start disagreement | the 5 s slack lane-restart found on CI is carried over. |
| `restore` itself crashes mid-run | the plan is written to the report as it goes; re-running is safe because §2.4 skips what is up. |
| roster file lost | restore has nothing; `roster scan` rebuilds from live lanes only. Open question Q11: put the roster in a (private) git repo? |
| token spend | Starting N lanes means N model turns on "read HANDOFF and continue". Batching bounds the *rate*, not the total. A `--max-lanes` cap and per-lane `enabled:false` exist; CireSnave's cost rule (CLAUDE.md §9) may want idle lanes left down (Q7). |

**HANDOFF staleness, concretely.** For each lane: `H` = mtime of the roster's `handoff` path;
`S` = last activity of the dead session (mtime of
`~/.claude/projects/<encoded cwd>/<session_id>.jsonl`, falling back to the state file's `updated_at`).
`stale` iff `S − H > handoff_stale_after_secs` (default 600). The `Written:` header is **advisory only**:
the format is free-text in practice (OverMind's HANDOFF says `Rewritten 2026-10-03 ~01:00Z`).

Measured 2026-10-03 ~15:43Z, both times in UTC (file mtimes converted from local, UTC−7; state
`updated_at` is already UTC), `S` taken as the last state-file event, against the live fleet (these are
working lanes, not casualties): `unpopped` 0.0 h behind, `synapse` 0.1 h, `lightbulb` 0.1 h,
`overmind` 1.2 h, `baracuda` 2.0 h, `pm` 16.4 h, `thinkersjournal-community` 31.3 h, `fuel` 79.8 h,
`mlmf` 80.8 h. So the fleet splits cleanly: lanes that just restarted via `lane-restart --self` (which
requires a HANDOFF) are fresh; **lanes that have been running a long time are days behind**, and those are
exactly the ones a Windows update catches mid-task. HANDOFF freshness therefore protects the lanes that
need it least. It is a mitigation for lanes that restart at task boundaries (CLAUDE.md §9), not a
guarantee, and the design says so instead of implying otherwise. (`S` from the transcript mtime may differ
from the state-file event; I measured the state file only.) The restore response is: report the age per lane, and append one factual sentence to that lane's
prompt (`HANDOFF is <age> older than the end of your last session; the old transcript is at <path> if
you need to see what happened after it`). The prompt text is built by agentlife from computed values
only, never from lane-written content. Whether to go further (resume the transcript for stale lanes) is
Q5.

---

## 8. Migration from lane-restart

**One tool, not two.** End state: `agentlife` owns the roster, `restore/up/down/list`, the state hooks,
the restart log and the host; `lane-restart` is a shim, then deleted. Until the cutover each concern has
**one** owner, listed here, so nothing is maintained twice.

| phase | agentlife owns | OverMind's `lane-restart` owns |
|---|---|---|
| **0 (now → approval)** | nothing built | everything; no change |
| **1 — read-only coexistence** | `roster`, `restore`, `up`, `list`, `peers`, `install-task`; reads `.lane-state/*.json` and `restart.log` **read-only**, writes only `.agentlife/` | `state` hooks, `assert-idle`, `host`, `approvals`, `restart --self`, `tab_close`. agentlife *invokes* the installed `lane-restart.exe host` to launch (§4.5) and parses the files above; it adds no dependency on OverMind's source. |
| **2 — library extraction** | the new `relaunch`/`liveness` code | an OverMind PR moves `mod relaunch` out of `main.rs` into the library (§0.10) so both binaries share it; `lane-restart` keeps working unchanged |
| **3 — hooks move** | `agentlife state <event>`, `assert-idle`, the role-via-roster resolution (ends phantom files, §0.2) | `lane-restart` execs `agentlife` for those verbs. **Hook change in `~/.claude/settings.json` is CireSnave's/the PM's** (OverMind spec §10.4 "settings are CireSnave's"), done with the rename-not-overwrite install the OverMind spec §11.2 documents, since the binary is in use by every hook. |
| **4 — retire** | everything | tag a last version; `lane-restart` becomes `agentlife restart`; remove the crate after one release with no callers |

**Coexistence rules.** (1) The `.lane-state` schema is **additive only** while both run (new fields,
never renamed or retyped); agentlife reads with `#[serde(default)]` and tolerates unknown fields.
(2) `restart.log` is append-only and shared; agentlife writes `requested_by` values from the same set
(`self`, `pm`) plus `agentlife-restore`. (3) A lane-restart bug fix goes to OverMind until phase 3;
agentlife never patches its behaviour around it. (4) The 3-way dependency (`approvals` repo,
`~/.overmind/lane-restart.json`, `with-secret` Hello) stays where it is; agentlife reads the config and
reuses `hello.rs` through a git dependency or an extracted crate, an OverMind-side decision (Q12).

---

## 9. Test plan

Principle taken from lane-restart: **a fake proves the logic, only a real process proves the contact
with the OS.** The OverMind spec records the proof: `sysinfo`'s default refresh left `cwd` and `cmd`
empty, and 79 passing fake-based tests missed it until two real-child tests existed (§10.1).

### 9.1 Unit and property tests (pure, no OS)

- **Planner:** (roster × fake process table × clock) → batches. Properties: PM is batch 0 and alone;
  no batch exceeds `batch_size`; no lane starts before its predecessor batch is settled *and* the delay
  elapsed; an ALIVE lane never appears in a plan; a lane in no state is `AMBIGUOUS`, not started.
  Injected `sleep` and clock, so the 15-minute paths run instantly (lane-restart's technique).
- **Policy matrix (§4.4)**, every cell, including: non-PM `bypassPermissions` refused; mode raised vs.
  signed → Hello required; tampered signature → entry skipped; an unknown mode is untrusted.
- **Identity (§2.4):** recycled pid with a different start time; same cwd, two roster entries
  distinguished by `name`; a state file whose pid is alive but a different `exe`.
- **Argv builder:** `;` in any element refuses the launch; the prompt is never directly after a variadic
  flag; `-n`, `--name`, `--resume`, `--permission-mode=x` handled; **mutation-verified** (flip each
  guard, a test must fail) with `assert count == 1` on every anchor, per CLAUDE.md §6.
- **Stale-HANDOFF calc** on the four measured shapes in §7.
- **Report shape:** predicted before running, then compared.

### 9.2 Real spawn/kill test (not a unit test)

`tests/real_spawn.rs`, `#[ignore]` by default, run on Windows CI **and** locally. It uses a small
in-repo stand-in binary `fake-claude` (not the real `claude`, which spends tokens) that:

1. is named/typed so `find_claude_process_in`'s image-name list accepts it (the function is
   parametrised on image names precisely so a stand-in works; lane-restart tests it with `ping`),
2. reads its own argv and writes a real `.lane-state/<role>.json` through the **real** hook JSON path,
   first `SessionStart`, then `UserPromptSubmit`,
3. optionally prints the dev-channels dialog text and blocks until it reads `1\r` on stdin (to exercise
   the host's approval keystroke, with the approval served from a local fixture as lane-restart's tests
   do),
4. can be told to crash after N seconds, hang at the dialog, or ignore `down`.

The test then runs the **real `agentlife restore`** against a temp roster and temp `AGENTLIFE_HOME` /
state dir (an env override, so the test can never touch `C:/Projects/.lane-state`), through the **real
launch path** (`wt.exe` where present, else `conhost.exe`, asserting which one ran), and checks: the
child exists with the right argv and cwd, the state file shows progress, the report says `Working`,
`down` terminates exactly that pid (identity-verified), and a second `restore` starts nothing
(idempotence). Cleanup kills only pids it spawned and records their start times first (CLAUDE.md §5).

**Windows CI caveat, UNVERIFIED:** GitHub's `windows-latest` may lack Windows Terminal, so CI may only
exercise the `conhost.exe` fallback. The real `wt.exe` + Task Scheduler path is therefore also a
**manual acceptance run** on this machine, logged in `docs/ACCEPTANCE.md` with the exact commands and
outputs, before the first release. Same for the live Hello prompt (run with CireSnave at the desktop,
as `with-secret` did).

### 9.3 A fleet of 40 without 40 agents

- `agentlife fleet-sim --n 40`: 40 `fake-claude` roster entries, generated, in a temp home. Launched via
  a `Launcher` trait with a **direct-spawn** implementation (CreateProcess, no `wt`) so 40 processes cost
  almost nothing, while the *planner, liveness polling, state-file reading and report* are the real code.
- **Real-time smoke:** `batch=10, delay=1 s`; assert peak "starting" ≤ 10, PM start time earliest, total
  wall-clock within a computed bound, the report lists 40.
- **Chaos set** mixed in: 5 never start, 3 stop at the fake dialog, 1 crashes after `Working`, 2
  already running (idempotence), 1 non-PM entry set to `bypassPermissions` (refused), 1 AMBIGUOUS cwd,
  1 with a bad signature, 1 stale HANDOFF. Assert the report classifies **each** by name, never by count
  (CLAUDE.md §5: "a number standing in for a set").
- **Halt rule:** with `stop_after_failed_batches=2` and 20 dead lanes, assert it stops, reports "halted",
  and started nothing after the halt.
- **Down fleet:** 40 fake lanes honouring the wrap-up protocol; assert PM last, none killed before its
  HANDOFF mtime moved, a lane ignoring the message is left alive and reported.

### 9.4 One real-claude canary

A `canary` roster entry (`C:/Projects/.agentlife-canary`, `--model` the cheapest, prompt "reply OK and
stop"), run once per release with CireSnave's knowledge. It is the only test that spends tokens and the
only one that exercises the *real* dialog + approval + `Working` chain end to end.

---

## 10. Open questions for CireSnave

Each is a decision that waits on him; recommendations are the lane's, not decisions.

- **Q1. Unattended restore.** Is "a signed roster, restored at logon with no further prompt" the
  right trust model (§4.3)? The alternative is a Hello prompt on every restore, which cannot work when
  he is away. *Recommend:* signed roster, no per-start prompt.
- **Q2. Auto-logon.** After an update, nothing starts until he logs in unless Windows auto-logs-on. Does
  he want `agentlife` to assume an interactive logon and say nothing about auto-logon, or should
  `install-task` also document/guide enabling it? (A security trade-off that is his.)
- **Q3. Which lanes are in the roster, and in what order?** The 43 state files are not the answer
  (§0.2). Proposed start: the 15 live lanes, with `pm` at 0, then Synapse/OverMind, then the rest.
  Does he want idle lanes (coderipper, KISS: "idle, holding for direction") restored at all?
- **Q4. How should he be told?** Phone push (through the PM), a Windows toast, both? And if the PM
  failed to start, who relays?
- **Q5. Stale HANDOFF lanes.** Report-and-warn only (v1), or allow `--resume` of the old transcript for
  lanes whose HANDOFF is stale, at the context-cost that `--resume` was banned for (OverMind §5)?
- **Q6. Can anything override "only the PM may bypass permissions"?** v1: no. Does a Hello yes count?
- **Q7. Concurrency and spend.** Default `batch_size`, `delay`, and whether a cap on lanes started per
  restore is wanted given the weekly-token concern in CLAUDE.md §9. Proposed: 3 / 45 s.
- **Q8. `down --force`.** Is killing a lane that never wrote its HANDOFF ever acceptable, e.g. a hung
  lane before a planned update? v1: no force.
- **Q9. One Windows Terminal window with tabs, or one window per lane?** (Today's hand-started lanes
  and lane-restart's `-w new` differ; I did not measure how he arranges them.)
- **Q10. claude-peers.** May we ask its owner for a stable `role`/`name` on registration so the
  parent-pid join (§5) becomes unnecessary?
- **Q11. Where does the roster live?** `C:/Projects/.agentlife/` (unversioned, lost with the disk) or a
  private repo (versioned, but a roster is authority-bearing config)?
- **Q12. Library reuse.** Reuse OverMind's `with-secret` Hello code and `lane-restart` library via git
  dependency, or extract a shared crate? (This is OverMind's call as much as his.)
- **Q13. The dev-channels approval expires 2027-03-19.** Who renews it, and should `agentlife list`
  warn 30 days ahead? (Cheap to add; he should know it expires.)
