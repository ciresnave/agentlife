# agentlife — design revision 1: a dynamic registry, consent at restore time

**Status: PROPOSAL for PM approval. Docs only, no code.** Written 2026-10-04 against the PM's
`[TASK] design-revision`. **Supersedes**, where they conflict: `DESIGN.md` §1 (roster), §2.1–2.4 (what
restore reads), §4.1/§4.3/§4.4 (trust), §7 (roster-lost row), and in full `ROSTER-SCHEMA.md` and
`V0.1.md`. Everything else in `DESIGN.md` stands (liveness method, launch mechanics, peer join, test
discipline, failure modes).

**CireSnave's ruling, verbatim (relayed by the PM, 2026-10-04):** *"I'm fine with me having to be at the
PC to do a Windows Hello prompt if necessary. I just don't want to have to manually open terminals and
launch every agent in a 15+ agent group that will likely grow toward hundreds of agents as we continue
our work. The solution will need to keep track of every agent that was running, not hard code specific
agents. We already do something similar with our lane restart that tracks command line options so there
is prior work we may be able to leverage and expand."*

**Evidence base.** OverMind `origin/main` `3bf513f` (the `lane-restart` crate is byte-identical to
`c6c8c1e`: `git diff c6c8c1e 3bf513f -- crates/lane-restart` is empty; only `with-secret` changed). File:line
citations below are to that ref. Runtime numbers measured 2026-10-04 on this machine (63.2 GB RAM, 32
logical CPUs, 15 live `claude` processes) and are true of that moment only. **UNVERIFIED** items are
carried into §10 as things the real test must settle.

---

## 1. What changes in one paragraph

The source of truth stops being a file a person edits and becomes **a registry that agents write about
themselves when they start**, from the same kind of hook `lane-restart` already runs. "What to restore"
is *everything that was running when the machine went down*, minus anything a person deliberately closed
or parked. Authority moves from a signed standing roster to **one Windows Hello consent at restore time
that covers the exact plan shown**. The registry therefore stores no authority, only facts; its integrity
needs shrink to "accurate and auditable", not "unforgeable".

---

## 2. The dynamic registry

### 2.1 Reusing lane-restart's mechanism: what is already captured

`lane-restart state <event>` runs from every lane's hooks and already records, per launch, almost
everything a restore needs. Cited at `3bf513f`:

| captured today | where |
|---|---|
| full launch argv, verbatim (`launch_args`) | `lane_state_writer.rs:162` (field), `:173` (set in `parse_claude_cli_flags`), written at `:311`, `:368`, refreshed at `:407–409` |
| permission mode from `--permission-mode` / `--dangerously-skip-permissions` | `parse_claude_cli_flags`, `:171–196` |
| `--remote-control` | `:183` |
| the `claude` pid, by walking up through shell layers | `claude_parent_pid`, `:222` (`SHELL_ANCESTOR_NAMES` `:205`, `MAX_ANCESTRY_HOPS` `:210`) |
| the real command line of a pid | `ParentProcess::cmdline_of`, `:131`; `RealParentProcess` `:672`, impl `:688` |
| the **launch** directory (not the drifting current one), proven from the transcript path | `recorded_cwd`, `:258`; `transcript_project_dir` `:239` |
| model (from the `SessionStart` payload) | `HookInput.model`, `:60–70`, `ModelField` `:32` |
| session id, `updated_at`, `updated_by_event` | `state.rs:16` (`LaneState`) |
| race-safe read-modify-write | `StateLock` `:475` (create-new-file lock, stale reclaim `LOCK_STALE_AFTER` `:467`), `write_atomic` `:533` (temp + rename), `run` `:542` |

All of this is `pub mod lane_state_writer` in the library (`lib.rs`), so agentlife can call it without an
OverMind change, except two private items it would need (`recorded_cwd`, `write_atomic`), which is a
one-line visibility change in the OverMind PR already described in `OVERMIND-EXTRACTION-NOTE.md`.

### 2.2 What that mechanism does *not* capture, and the five defects it has for this job

1. **Keyed by the wrong thing.** `resolve_role` (`:76`) names the file from the *current* hook cwd's
   leaf. Measured 2026-10-03: 43 state files for 14 live-lane names, 21 owned by one pid (`DESIGN.md`
   §0.2). A registry must key on the **process launch** (session id, pid + start time), grouped into an
   agent by a stable id (§2.3), never on cwd.
2. **`name` is never recorded.** `parse_claude_cli_flags` (`:171–196`) handles only `--remote-control`,
   `--dangerously-skip-permissions`, `--permission-mode`; it does not read `-n`/`--name`, and
   `apply_event` copies `name` from the previous file (`:338`). All 43 files have `name: null`. The name
   is how a person recognises an agent and how `restore` tells two lanes in one cwd apart (e.g.
   `auth-framework` and `-n auth-framework-deps`, both in `C:\Projects\auth-framework`).
3. **`SessionEnd` deletes the record** (`:376`; the caller `remove_file` at `:581`). For "what was
   running" that destroys the answer exactly when it matters: if Windows closes sessions gracefully at
   shutdown and `SessionEnd` fires, every file vanishes and nothing is left to restore. Whether it fires
   is **UNVERIFIED** (the hook input struct, `:60–70`, has no `reason` field, and I found no recorded
   shutdown with hooks present: the 2026-09-16 update restart predates the hooks, the 2026-09-18 12:34
   shutdown was unclean). The registry must **never delete**; it marks `ended` with a time.
4. **`launch_args` is dropped from a *successor* record and the model is the full id** (aliases like
   `sonnet` become `claude-sonnet-5-5`). Fine for restore (the full id is valid) but the registry should
   keep the argv exactly as launched and restore from it through the existing allowlist
   (`ALLOWED_LAUNCH_ARG_FLAGS`, `main.rs:856`) rather than from derived fields.
5. **No terminal placement, no origin.** Nothing records which window/tab a lane was in, or whether a
   person or another agent started it. §2.5 and §4.3.

### 2.3 The registry design

Two stores, both outside every repo, under `C:/Projects/.agentlife/` (open question Q-D6 for the path):

- **`registry/agents/<agent_id>.json`**: one record per *agent* (a durable identity that survives
  restarts). Fields: `agent_id`; `name`; `launch_cwd`; `role` (self-claimed, advisory); `first_seen`;
  `sessions[]` (last N: `session_id`, `pid`, `process_start_time`, `started_at`, `ended_at`, `end_kind`);
  `launch_args` (verbatim argv of the latest session); `model`; `permission_mode`; `remote_control`;
  `placement` (§2.5); `origin` (§4.3); `state` (`running`|`ended`|`parked`|`superseded`); `park`
  (who/when/why). **Facts only**: no field grants anything.
- **`registry/journal.jsonl`**: append-only, one line per registration/end/park/restore decision, same
  discipline as `restart.log` (`log.rs`). The audit trail the Hello review (§4) diffs against.

**Agent identity, `agent_id`.** Assigned the first time an agent is seen and then *carried*, not
re-inferred:

1. **Launched by agentlife or by `lane-restart host`**: the launcher sets `AGENTLIFE_AGENT_ID=<id>` in the
   child's environment (the same channel `LANE_ROLE` already uses, `resolve_role` `:76`). The hook reads
   it, so the id is exact across any number of restarts, including two agents in one cwd.
2. **Hand-started** (`WindowsTerminal → pwsh → claude`, 6 of the 15 live lanes today): no env, so the
   hook looks up `(launch_cwd, name)`; no match → mint a new id. Two *unnamed* sessions in one cwd cannot
   be told apart this way and are recorded as one agent with `instances: 2` (restored as two) rather than
   guessed apart.
3. **Environment propagation through `wt.exe` is UNVERIFIED for a new variable.** Evidence it propagates
   at all: `lane-restart`'s relaunch inherited the launcher's session variables and had to strip ten of
   them (`SESSION_IDENTITY_ENV_VARS`, `main.rs:1081–1092`, `strip_session_identity_env` `:1094`, OverMind
   spec §5, third retest). If a new variable does not survive, the fallback is putting the id in the
   `--name` value (`<name>~<id>`). The real test (§10) settles it.

**Only interactive agents register.** Headless/child sessions must not become "agents to restore", or a
fan-out of subagents turns into a restore storm at hundreds. A session registers only if its command
line has no `-p/--print`, **and** the ancestry walk (`claude_parent_pid`'s own machinery, `:222`) finds no
other `claude.exe` above it. Measured: all 15 live `claude` processes have chains ending
`… <- pwsh|lane-restart <- WindowsTerminal <- svchost`, none under another `claude`.

### 2.4 How registration hooks in

Two ways to get a registration, with different costs. I recommend **R2**.

- **R1: extend the installed `lane-restart state` hook.** Zero settings change (same command string in
  `~/.claude/settings.json`), but it is a behaviour change to the hook every tool call in every lane runs
  (`PreToolUse` fires per call), it needs the OverMind lane to take it, and `SessionEnd`'s delete (`:376`)
  must change from delete to mark.
- **R2: a separate `agentlife hook SessionStart|SessionEnd`, two events only.** Low frequency (twice per
  session, not per tool call), independent release, reuses the library functions above, leaves the
  existing hook untouched. Cost: two entries appended to `~/.claude/settings.json` `hooks`, which is
  CireSnave's/the PM's to install (OverMind spec §10.4: "settings are CireSnave's"), done by appending
  to each event's array, never replacing `hooks` (the rule in spec §10.4 and §11.5; rollback removes only
  entries whose command is `agentlife`).

R2 is also the phase-3 end state of `DESIGN.md` §8 (agentlife owns the hooks), reached without a flag day.
Heartbeat ("when was it last alive") needs **no extra hook**: at restore time the registry joins each
session to the existing `.lane-state` file **by `session_id`** (not by role, which is the phantom-file
problem), and to the transcript mtime, taking the later of the two as `last_seen`.

### 2.5 Terminal placement

What is observable: nothing about the window. Measured: all 15 live lanes sit under **one**
`WindowsTerminal.exe` (pid 2812); a second `WindowsTerminal.exe` exists but hosts none of them. A
`WindowsTerminal.exe` process hosts all of its windows and tabs, so process ancestry says nothing about
which window or tab a lane is in. So placement **cannot be read back**; it can only be *assigned at
launch and recorded*:

- agentlife launches into a window named `agentlife-<k>` with the tab titled by the agent's `name`, and
  writes `placement: {window, title}` into the record. `k = index / tabs_per_window` (§6.3).
- For hand-started agents `placement` is `null`; restore *assigns* one.
- **UNVERIFIED** that `wt -w <name> new-tab --title …` addresses one window per name from separate
  `wt.exe` invocations (`DESIGN.md` §6 already flags this). Tested in §10.

### 2.6 "What was running at shutdown", decided without trusting `SessionEnd`

A registry record is a restore **candidate** iff all hold:

1. its last session's process (pid + start time) is **not running now**, and its start time is before
   this boot (`LastBootUpTime`, measured 2026-09-18 12:49 on this machine);
2. its state is not `parked` and not `superseded` (a later session of the same `agent_id` exists, as with
   a `lane-restart --self` relaunch, which kills the old pid without firing `SessionEnd`);
3. it was **not deliberately closed**: either no `SessionEnd` was recorded (killed), **or** its `ended_at`
   falls *after* the shutdown began. "Deliberately closed" is `SessionEnd` recorded **before** the
   shutdown started, i.e. a person typed `/exit` earlier.

The shutdown-start marker comes from the System event log, which I confirmed is readable without
elevation on this machine: Event 1074 (*"process … has initiated the restart"*, 2026-09-16 01:48:37, by
`TrustedInstaller.exe`, i.e. a Windows update, the case that motivates the tool), 6006 (log stopped), and
for an unclean stop 41/6008 (2026-09-18 12:49:33/12:49:45, "previous shutdown at 12:34:11 was
unexpected"). Rule: `shutdown_start = latest 1074 before this boot`, else the 6008 time, else
`max(last_seen)` across records. This makes rule 3 correct *whether or not* `SessionEnd` fires at
shutdown, which is the point.

**What it cannot do:** a person who closed a window five seconds before a Windows update started looks
the same as one the update closed. The cost is one extra agent offered in the review (§4.2), not a
silent loss.

---

## 3. Restore, revised

`DESIGN.md` §2.2's algorithm stands with the roster replaced:

```
read registry; compute candidates (§2.6); apply parked/excluded (§6.2); order (§3.1)
build the PLAN: ordered batches, each agent's exact argv, cwd, mode, placement
FREEZE it: write plans/<UTC>.json, hash it
Hello consent (§4.2) on the frozen plan's summary + hash
execute exactly the frozen plan (§2.2/§2.3 of DESIGN.md: batches, liveness, halt, idempotence, report)
```

### 3.1 Order, without a hand-kept list

PM first and alone (`DESIGN.md` §2.2): the agent whose `launch_cwd` is the portfolio root `C:/Projects`
and whose role/name is `pm` (both self-claimed; the review shows it, §4). Then by **most recently active
first** (`last_seen`), on the argument that the agent working most recently is the one most likely wanted.
Ties and a per-agent `priority` nudge live in `overrides.json` (preferences, §4.4), not authority.
Whether recency is the right default is Q-D5.

### 3.2 Idempotence, unchanged in rule, changed in key

`DESIGN.md` §2.4's pid+start-time test, keyed by `agent_id` and session rather than role. Two agents in one
cwd are two records, so "something is running in this cwd" no longer blocks a start; only "this agent's
own process is alive" does.

### 3.3 The control tab

At logon the task opens one **control tab** (`agentlife restore` in a normal terminal): it shows the plan,
asks for the Hello consent, then shows batch progress and the final report. This is where §4.2's consent
is requested and where §5's default notification lands, because he said he will be at the PC.

---

## 4. Trust model

### 4.1 What changed

`DESIGN.md` §4.3 signed each roster entry and let restore run unattended. Revised: **nothing is signed
and no standing authority is stored.** The registry holds facts; **authority is a Hello consent at
restore time over the exact plan.**

### 4.2 Batch consent, not per-agent prompts

One `UserConsentVerifier` prompt for the whole fleet, reusing `with-secret`'s `hello.rs`
(`Owner::Foreground`, the default) and `consent.rs` (`ConsentRequest`/`prompt_text`) as in
`DESIGN.md` §4.3. A Hello message is a short string and hundreds of agents cannot be listed in it, so:

- The prompt carries a **summary and the plan hash**: *N agents; M new since the last approved restore;
  K at a non-default mode; bypass: pm only; plan abc123*.
- The **detail is in the control tab** (§3.3), with exceptions at the top: agents **new** since the last
  approved restore, agents whose **mode or flags are higher** than at their previous restore, anything
  refused by a hard rule, anything whose `origin` is not a person (§4.3).
- Before consenting he can **deselect** agents in the control tab (this writes `park`/exclude entries,
  §6.2); the plan is **re-frozen and re-hashed** and the prompt carries the *new* hash.
- **Execute only the frozen plan.** Restore refuses if the registry changed between consent and start
  for any listed agent's launch fields, closing the window where an agent edits the registry after the
  prompt (TOCTOU). Agents registered *after* consent are simply not in the plan.
- A prompt nobody answers times out (`with-secret` behaviour, design §2.2) and restore starts nothing and
  says so. It does not fall back to starting unconfirmed.

### 4.3 What the registry's integrity needs now

**Accuracy and auditability, not unforgeability.** Concretely:

1. **Atomic, locked writes** (`StateLock` `:475`, temp+rename `:533`), so a crash or a concurrent hook
   cannot truncate a record.
2. **Append-only journal** so "new since last approved restore" and "mode higher than before" are
   computed from history rather than from the record being judged.
3. **`origin` recorded at registration** from the process ancestry the hook already walks:
   `hand` (`WindowsTerminal → shell → claude`), `host` (`lane-restart host` or `agentlife host` parent),
   `other`. It is a *signal for the review*, not a gate: an agent can start a `host`-parented lane.
4. **Hard rules in the plan builder, independent of the registry's contents**, as in `DESIGN.md` §4.1:
   `bypassPermissions` only for the portfolio-root PM agent, otherwise **refused, not downgraded**; mode
   ordering from the same table (unknown/`plan` incomparable → flagged); `channels` allowlist; no
   `;` in any argv element; `cwd` must exist and be under `C:/Projects`.
5. **Stated limit (unchanged, same as `with-secret` §3 and `lane-restart` §12.8):** any process running as
   this Windows user can write the registry, read DPAPI, or send keystrokes. A forged record can make the
   review *show* a benign-looking entry. What stops it being silent is the review, the journal diff, and
   Hello; none of it stops a hostile same-user process. The honest claim is "a casually misbehaving lane
   cannot get a launch the person has not seen", not more.

### 4.4 `overrides.json`: preferences, never authority

A small person-edited file: `priority` nudges, `tabs_per_window`, batch size/delay, memory floor (§6),
and `park` flags that agentlife also writes. Rule: **an entry may only narrow** (park, exclude, lower a
mode, delay); anything that would widen (raise a mode, allow bypass for another agent, add a channel) is
ignored here and can only come from the Hello review of a specific plan. That keeps the file from
becoming the signed roster by the back door.

### 4.5 Agent-originated requests

Unchanged from `DESIGN.md` §4.4: `agentlife up/down/restore` from a process with a `claude` ancestor
needs Hello naming the caller and target, whoever it is. The ancestry test remains a convenience, not a
boundary (`schtasks /run` or a detached `wt` escapes it); the boundary is that every launch of anything
needs a consent that shows it.

---

## 5. How he is told (Q4): proposed default

Default, in order, all cheap and all verifiable with what exists:

1. **The control tab is the primary channel** (§3.3): plan, consent, live batch progress, final report, in
   a terminal he is already looking at, since he said he will be at the PC.
2. **`reports/restore-<UTC>.json` and the journal**, always.
3. **A `[NOTICE]` to the PM lane over the broker** (`POST /send-message`, the path `notify.rs` uses) once
   the PM is up, so the PM can relay by its own means (the phone push the OverMind spec mentions is a PM
   capability, not agentlife's).
4. **A Windows toast only when something needs him and the control tab may not be in view**: waiting on
   Hello, a halted run, or a failed PM. **UNVERIFIED** from an unpackaged Rust binary (toasts need an
   AppUserModelID); if it does not work the fallback is a console beep + the control tab title change.

A Hello prompt itself is a notification: Windows raises it on the desktop. If he is away it times out
(§4.2) and the control tab and toast say "restore is waiting for you".

---

## 6. Scale to hundreds

### 6.1 Measured numbers (2026-10-03/04, this machine)

| quantity | measured | how |
|---|---|---|
| resident memory per lane | `claude` ≈ **518 MB** (7,767 MB / 15), **2 `bun` per lane** ≈ 63 MB each (1,940 MB / 31), `lane-restart` host ≈ 6 MB, `OpenConsole` ≈ 7 MB, plus a `pwsh` for hand-started ones: **≈ 0.65 GB per idle lane** | `Get-Process` working sets, 15 live lanes |
| machine | 63.2 GB RAM, **22.2 GB free** with the 15 lanes (and whatever else is running), 32 logical CPUs | `Win32_ComputerSystem`/`OS` |
| time from host start to "Working" | min **4 s**, median **12 s**, max **123 s** (n = 46) | first host-log timestamp vs the matching `restart.log` "relaunched" line, same role, <5 min apart. These were one-at-a-time restarts, **not concurrent**; contention is unmeasured. |
| tokens a restored session spends before settling | first-turn context **51 k – 126 k** (median ≈ 53 k); first 10 assistant records total **0.57 M – 1.32 M** input-side tokens (cache reads and creation included) and **2 k – 5 k** output (n = 48 restored sessions found in `~/.claude/projects`) | first-user-message matches `read <role> HANDOFF and continue`. **Caveat:** counted per transcript assistant record, which may count one API call more than once. Order of magnitude, not a price. |

### 6.2 What those numbers mean

- **Hundreds running at once cannot fit this machine.** At ≈ 0.65 GB idle each, 22 GB free is ≈ 34 more
  lanes of idle footprint, and 100 lanes ≈ 65 GB, more than the 63 GB installed, before a single build.
  So *tracking* hundreds in the registry is fine and *running* hundreds is a hardware limit. Parking is
  therefore not an optimisation, it is a requirement at that scale.
- **Cost is per restored lane**: ≈ 0.6 M input-side tokens (mostly cache reads) in the first ten turns for
  the typical lane, ≈ 1.3 M for `fuel`. 100 restored lanes is on the order of 60–130 M such tokens,
  mostly cheap cached reads but not zero, and CLAUDE.md §9's cost rule is a standing constraint.

### 6.3 Mechanisms

- **Batching and caps.** `batch_size`, `batch_delay`, and a **hard `--max-running`** (never start more
  than N concurrent agents, counting already-running ones). Plus a **memory floor**: stop starting when
  free RAM falls below a configurable GB (default proposed: 8), reported as "held for memory", not as
  failure.
- **Start-time budget** from the measured median, ignoring contention: wall time ≈
  `ceil(N / batch) × (delay + t_settle)` with `t_settle ≈ 12 s`:

  | agents | batch / delay | ≈ wall time |
  |---|---|---|
  | 15 | 3 / 45 s | ≈ 5 min |
  | 100 | 5 / 30 s | ≈ 14 min |
  | 300 | 5 / 30 s | ≈ 42 min (memory forbids running it, see above) |
  The max observed settle was 123 s, so treat these as lower bounds.
- **Windows and tabs.** One tab per agent in a window named `agentlife-<k>` with at most
  `tabs_per_window` (proposed default **10**, a human-usable number, not a technical limit; WT's own
  tab ceiling is **UNVERIFIED**). So 100 agents ≈ 10 windows. `placement` (§2.5) is recorded so a
  restore reproduces the layout. Q-D2.
- **The registry itself must scale.** One file per agent plus an append-only journal means a write never
  rewrites the fleet. Ended records are **pruned after a retention window** (proposed 14 days, Q-D6) but
  the journal keeps the audit line.

### 6.4 The exclusion mechanism: options, and the decision is CireSnave's

This is the cost and memory valve. I do **not** choose between these; each is buildable and they
combine.

| option | what it does | for | against |
|---|---|---|---|
| **E0 none** | restore everything that was running | simplest; matches "restore every agent that was running" literally | at hundreds, hits the memory ceiling and the token cost; only `--max-running` stops it |
| **E1 explicit park** | `agentlife park <name>` sets `parked` (sticky until `unpark`); restore lists parked agents and skips them | clear, auditable, a person's decision, shown in the review | someone must park; a parked agent is simply not there until someone un-parks it |
| **E2 idle rule** | agents whose `last_seen` was more than T before shutdown are *proposed* for skipping in the review (not silently skipped) | no per-agent chore; matches "idle/parked" wording | T is a guess; an agent mid-thought that paused for T is skipped; recency is the only signal available (busy flags are unreliable: `DESIGN.md` §1.2) |
| **E3 lazy start** | parked agents start on demand when a message addressed to them arrives | keeps hundreds *reachable* at near-zero cost | needs a resident listener on the broker and a "wake" protocol, a different component; not v0.x |
| **E4 per-restore cap** | `--max-agents N`, highest priority/recency first, rest listed as "not started: over cap" | a hard bound on spend | arbitrary cut line |

Defaults until he decides: **ship E0 + the E4/`--max-running`/memory-floor caps as mechanisms with
conservative defaults, and ship E1's `park` command as the manual escape**, and state plainly in the
control tab how many agents were over the cap. Which of E1/E2/E3 should become *policy* is **Q-D1**.

---

## 7. Reused / new / extracted

| piece | status | where / note |
|---|---|---|
| hook machinery: lock, atomic write, role/pid/cmdline/launch-cwd/flag parsing | **reused**, library, already `pub` | `lane_state_writer.rs` (§2.1). Two items need `pub`. |
| `LaneState`, `.lane-state` files, `restart.log` | **reused read-only**, joined by `session_id` | `state.rs:16`, `log.rs` |
| ConPTY host, approvals fetch, startup-dialog keystroke | **reused** (launch every agent through `lane-restart host`) | `host.rs`, `approvals.rs` (`DESIGN.md` §4.5) |
| process identity, `kill_verified`, `find_claude_process_in`, `process_table` | **reused** | `facts.rs` (`SystemFacts`) |
| broker notify | **reused** | `notify.rs` |
| Hello consent | **reused** (git dependency or extracted crate, Q12 of `DESIGN.md`) | `with-secret` `hello.rs`, `consent.rs` |
| `valid_identifier`, argv allowlist, `;` guard, env strip, `spawn_relaunch`, liveness protocol | **extracted from OverMind** | `main.rs` `mod relaunch` L723–2200, per `OVERMIND-EXTRACTION-NOTE.md`; v0.x may carry a drift-tested copy |
| `-n`/`--name` parsing | **new** (small), belongs next to `parse_claude_cli_flags` `:171` | OverMind PR or agentlife-side parse of `launch_args` |
| registry store + journal, `agentlife hook SessionStart/SessionEnd`, `agent_id` env carry | **new** | §2 |
| shutdown-start inference from the System log, candidate rule | **new** | §2.6 |
| plan builder: ordering, caps, memory floor, placement, freeze + hash | **new** | §3, §6 |
| control tab UI, batch Hello review, deselect → park | **new** | §3.3, §4.2 |
| `park`/`unpark`, `list`, `overrides.json` | **new** | §4.4, §6.4 |
| reports, notices, toast | **new** (notice path reused) | §5 |

---

## 8. Effect on the first slice (replaces `V0.1.md`)

The smallest slice that ends hand-launching, now registry-based:

1. `agentlife hook SessionStart|SessionEnd` (R2) writing the registry and journal, with the filters of
   §2.3 (interactive only, no deletes).
2. `agentlife restore` with candidate rule §2.6, frozen plan, control tab, **one Hello consent**,
   PM-first, batches, per-batch liveness, idempotence, `--max-running`, memory floor, report, PM notice.
3. `agentlife list` and `park`/`unpark`.
4. Launch every agent through `lane-restart host` (so the dev-channels approval still fires,
   `DESIGN.md` §4.5), into `agentlife-<k>` windows.
5. A documented `schtasks` logon command; `--from-logon` waits for network (the measured
   2026-10-01 approvals failure).

Not yet: lazy start (E3), toast if unverified, `down`/graceful wrap-up, peers/resolve, hook migration
beyond the two new events. Hello is **in** the first slice now (§4), which makes the OverMind-side reuse
decision (Q12) a prerequisite rather than a v0.2 item.

---

## 9. What this revision does *not* solve

- A lane's work since its last HANDOFF (`DESIGN.md` §7): unchanged.
- Hostile same-user processes (§4.3 item 5).
- A forged-but-plausible registry entry that a person approves without reading the review.
- Whether `SessionEnd` fires at shutdown (§2.2 item 3): the design is *robust* to either answer; it is
  not *informed* by it until the test in §10 runs.
- Hundreds *running* on this hardware (§6.2).

---

## 10. Test additions (on top of `DESIGN.md` §9)

- **Shutdown experiment (UNVERIFIED items 2.2-3, 2.6):** with CireSnave's knowledge, a canary agent
  with an `agentlife hook SessionEnd` logging to a file; end it three ways (`/exit`, killing the
  terminal window, an actual `shutdown /r` or a Windows-update restart) and record whether `SessionEnd`
  fired, with what payload, and the 1074/6006 timestamps. This is the only way to learn the real
  behaviour; I did not do it.
- **Env propagation (§2.3-3):** launch a stand-in through `wt.exe -w agentlife-0 new-tab …` and assert the
  child sees `AGENTLIFE_AGENT_ID`.
- **Registry property tests:** concurrent `SessionStart` hooks never lose a record (the 20-thread test
  pattern, `lane_state_writer.rs` tests); a `lane-restart --self` successor supersedes, not duplicates;
  headless/child sessions never register (stand-in with `-p`, stand-in with a `claude` ancestor).
- **Candidate rule table:** killed / `/exit` before shutdown / `/exit` after shutdown began / superseded /
  parked / clock skew, each against all three shutdown-marker sources.
- **Plan freeze:** change a registry record between consent and execution; assert restore refuses that
  agent and starts the rest.
- **Fleet sim at 300 records** with a stand-in launcher, asserting `--max-running`, the memory floor
  (injected free-RAM function), window assignment (`tabs_per_window`), and that the report names each
  held/over-cap agent.

---

## 11. Open questions for CireSnave (new; `DESIGN.md` §10 Q1, Q3, Q4 are answered)

- **Q-D1. The exclusion policy (§6.4).** E0 only? E1 explicit park? E2 idle rule (and what T)? E3 lazy
  start later? E4 a cap? **Not decided here.** Also: should a parked agent that receives a message be
  woken, or stay parked until a person unparks it?
- **Q-D2. Windows layout.** `tabs_per_window` (proposed 10), and one window per N agents vs one window
  for all. And is it acceptable that restore re-creates the layout it assigned, not the exact layout he
  had (which cannot be read back, §2.5)?
- **Q-D3. Memory and spend ceilings.** The free-RAM floor (proposed 8 GB) and a default
  `--max-running`. This machine cannot hold hundreds (§6.2); is the plan a bigger machine, parking, or
  both?
- **Q-D4. "Closed on purpose".** Is "`/exit` before the shutdown started means do not restore" the right
  rule, including for an agent he closed a minute before an update?
- **Q-D5. Order.** PM first, then most-recently-active first: is recency the right default, or should some
  agents (Synapse, OverMind) always come early?
- **Q-D6. Retention and location.** How long to keep ended records (proposed 14 days), and is
  `C:/Projects/.agentlife/` the right home?
- **Q-D7. Hook ownership.** R2 (two new hook events appended to `~/.claude/settings.json`) or R1 (extend
  the installed hook)? R2 touches his settings; R1 touches the hook every tool call runs.
- **Q-D8. Hello timeout.** How long should restore wait for him at the PC before giving up and starting
  nothing (proposed 10 min), and should the toast repeat?
- **Q-D9. Hello in the first slice.** It is now a prerequisite, and `with-secret`'s Hello code lives in
  OverMind (`DESIGN.md` Q12). Git dependency, or extract a shared crate?
