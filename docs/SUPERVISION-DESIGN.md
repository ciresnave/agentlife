# Supervision: one common layer over every agent kind (design note)

Written 2026-11-13 by the agentlife lane at `origin/main` 1432aa6 (0.30.61), on the PM's TASK of the same day. **Design
only: no code, no behaviour change.** Facts come from `docs/SUPERVISION-INPUTS.md` (the 2026-10-31 read of this repo; its
section numbers are cited as "Inputs 1.2" and so on) and from the one-line descriptions of other projects quoted below.
Anything about a project I did not read is marked UNVERIFIED. Proposals are marked **Proposal**; the stack choices that
are CireSnave's are the numbered questions in section 8.

CireSnave, verbatim, 2026-10-31 (as relayed by the PM; the spelling "AgentMind-Ollama" is as quoted): *"AgentLife is a
project for starting and stopping Claude Code instances.  That's all it is right now.  However, it doesn't have to stay
just that.  AgentLife as a concept is a tool for controlling the lifecycle of an agent.  It can start agents, stop
agents, soft-stop agents, and restart agents.  Those functions are common to almost any agent and not specific to Claude
Code.  It seems like our Claude Code portions of AgentLife should be moved into an AgentLife-ClaudeCode project and
brought into AgentLife as one backend module it can support.  That would allow AgentLife to also control agent lifecycle
through OverMind, Ollama, etc. just by creating modules like AgentLife-OverMind, AgentMind-Ollama, etc."*

PM standing direction (2026-11-13): supervision is ONE common layer over every agent kind (Claude Code, Ollama,
Lightbulb/Fuel, remote); AgentLife owns it; the event contract goes via Synapse.

## 1. Goals and non-goals

Goals:
1. One vocabulary for "what state is this agent in" across kinds, derived the way `status` derives it today (Inputs 1.2).
2. The invariant every restart already depends on survives unchanged: **an agent may be stopped only if it can be started
   again** (the spec is complete, written and read back before the kill; Inputs section 3).
3. A new agent kind is one adapter, not a change to the core.
4. `lane-restart`, as the fleet runs it today, never breaks during the move (section 7).
5. Lifecycle changes are observable by other tools through Synapse, without any tool having to read AgentLife's files.

Non-goals: sandboxing (AntSpawn), a scheduler, deciding WHICH agents should run (the PM, the launcher's rules), and
anything about remote agents beyond the contract's shape (question 4).

## 2. The core / adapter split

Read off Inputs section 3 ("could be taken as it is" versus "must stay Claude-specific").

**Core (agent-kind free; no data or logic names Claude Code):**
- The role identifier and `valid_identifier`.
- `Lifecycle` (`running | suspended | crashed | failed_relaunch | never_run | unknown`), `SpecState`, and the derivation
  rule of Inputs 1.2, with "is it live" **supplied by the adapter**.
- `StopKind` (soft/hard) and its signed soft marker.
- The spec as "the way back": a generic part (`role`, `name`, `cwd`, `state`, `stop_kind`, `stop_reason`, `written_by`,
  `written_at`) plus `backend` (new, a short string) and `backend_params` (opaque to the core, owned by the adapter).
- The atomic spec write (complete, temp file, `sync_all`, rename, read back and compare) and the write-before-kill order.
- The ledger (`restart.log`), the per-role start lock, the disk-floor guard.
- `status --json` as an output contract (section 5), the caller-identity gate (who may start or stop), and the event
  contract (section 4).

**Adapter (one per agent kind), the trait the core calls. Proposal, names provisional:**

| Method | Meaning | Claude Code today |
|---|---|---|
| `capabilities()` | what this kind can do; declared, and a request for more is refused at startup, never discovered mid-run | all of the below except `remote` |
| `check_spec(params)` | is `backend_params` complete enough to launch (feeds `startable`) | `SpecFile::check` after the model pin |
| `launch(spec) -> Launched` | start it; return an opaque identity for liveness and the exit class (`started`, `awaiting_input`, `refused`, `failed`) | `lane-restart host` in a terminal tab hosting `claude` in a ConPTY; exit 2 = awaiting dialog |
| `liveness(identity) -> Live \| Gone \| Unknown` | the ONE place "running" comes from | pid plus start time in the process table |
| `stop(spec, kind) -> Stopped` | soft stop = the agent's own wrap-up; hard = kill; both end with the process confirmed gone | HANDOFF/assert-idle protocol, then `claude` killed with its start time checked |
| `activity(identity) -> Option<Activity>` | busy, children running, last-event time; `None` when the kind cannot tell | hook-written `<role>.json` |
| `duplicate_check(spec)` | would a second launch double-run this role | process table by command line, never by cwd (Inputs section 2, failure 2) |

Rules the core enforces on every adapter, each from a failure already paid for (Inputs section 2):
- **`Unknown` is never `Gone`.** An adapter that cannot tell says so; the core reports `unknown` and starts nothing.
  (A supervisor must not infer "never started" from the absence of a record when the writer can decline silently:
  Inputs failure 12.)
- **A restart keeps the recorded spec's launch parameters**, taking only what the caller explicitly overrides (failure 1).
- **Liveness is read from the adapter's own source, never from a file's age** (failure 3).
- **An adapter never answers an approval prompt on behalf of a person** that the core's rules forbid; the Claude Code
  adapter keeps its startup-window approvals, and `awaiting_input` is a first-class launch result (failure 5).
- **No adapter may declare idle without an assertion from the agent.** "Never asserted" is not idle (PM ruling
  2026-10-14, Inputs 1.2). An adapter with no `activity` reports `None`, and the idle verdict is then `unknown`, not true.
  **Ruled 2026-11-13 (board 435, question 3):** an agent with no external idle detection is *told to set its own idle from
  inside*, by a tool call; the assertion is still the agent's. What the supervisor may derive is only **how long** the idle
  has lasted (section 8a); it never derives idle itself.

## 3. What each part keeps

| Part | Keeps | Gives up |
|---|---|---|
| **AgentLife core** | everything in section 2's core list; the `restore` planner (it plans by spec, so it is kind-free once a spec carries `backend`); the launcher service (wake-on-message; it calls the core's `start`, not `claude`); `agentlife status/park/stop` | nothing it has today |
| **Claude Code adapter** | hooks and `<role>.json`; `launch_args` and its allowlist; the model rules (never Opus); `permission_mode`, `remote_control`; startup-window approvals and handlers; HANDOFF, `assert-idle`; the ConPTY host; `wsl` launch mode; `.claude-hooks` | the generic lifecycle types, which move down into the core |
| **`lane-restart` (the command)** | its name, argv, exit codes, `.claude-hooks\lane-restart.exe` path, `status --json` schema 1, every file name in Inputs section 4; it becomes a thin front end over core plus the Claude Code adapter (the form CireSnave approved on 2026-10-08: "lane-restart as a wrapper over new lane-stop and lane-start tools") | the code, not the interface |
| **AntSpawn** | one job in a clean sandbox, its launchers (`account`, `windows-sandbox`, ...) and the tested capability matrix; **not** supervised lifecycle | nothing |
| **OverMind** | the orchestrator and its own agents | the lifecycle of Claude Code lanes (already approved, 2026-10-08) |

**AntSpawn is not an agent kind; it is something an adapter may call.** AntSpawn's unit is a job (create, load what the
job declares, run, collect declared outputs, destroy), where the core's unit is a named role with a way back. A job
launched through an adapter that happens to use an AntSpawn launcher is still supervised by the core as that adapter's
role; AntSpawn is then its sandbox, and the core never sandboxes. If a "sandboxed one-shot job" later needs a role (a
CI-runner watcher, say), it is a kind of its own with an AntSpawn-backed adapter. That is the whole relationship.

## 4. The event contract, over Synapse

Facts that shape it (Inputs section 5, from Synapse's own code on 2026-10-31): Synapse delivers messages between roles,
stores no events (only counters and timestamps per role), redelivers an unacked message (after about 2, 4, 8 and 16
minutes), is loopback-only with no remote transport yet, and roles authenticate by certificate chain and a signed claim.
Consequences:

1. **The spec file and the ledger are the record; an event is a notification of a change that is already on disk and read
   back.** A lost event loses a notification, never a fact. A consumer that missed one runs `status --json`.
2. **At-least-once, so consumers deduplicate.** Each event carries `seq`, a per-role counter the core increments in the
   spec at the same atomic write as the state change.
3. **Sent by the core after the read-back, never by an adapter**, so an event names a state the core has verified.

**Proposal: one routine message shape**, in the existing compact format (`MESSAGE-FORMATS.md`), a new tag:

    [LIFECYCLE] role=<role> kind=<agent-kind> from=<state> to=<state> seq=<n> at=<RFC3339Z> by=<caller> stop=<soft|hard|-> reason=<token>
    note: <free text, optional, untrusted by the receiver>

- `from`/`to` are `Lifecycle` values; `reason` is a token (`started`, `restart`, `stopped`, `crashed-observed`,
  `launch-failed`, `awaiting-input`, ...); `stop` is the `StopKind` when the new state is `suspended`.
- A second tag for the one signal that is not a transition, **`[ACTIVITY]`**, is NOT proposed: activity changes per tool
  call and Synapse is not a metrics bus. Activity stays a read (`status --json`), by design.
- Who receives: a configured list of subscriber roles in the core's config (the PM by default), one message each.
  Read at synapse `origin/main` 2026-11-13 (`docs/ROOMS_DESIGN.md`, `crates/synapsed/src/format_lint.rs`): Synapse has
  **rooms** (fan-out to the mailboxes of joined members who are *online*), so an offline subscriber would miss a room
  copy; that is acceptable only because the spec and ledger are the record (point 1). Its daemon path has no
  per-sender "may send to" rule, and one daemon serves one account. A `[LIFECYCLE]` header is checked only by a soft
  lint keyed on a per-tag key table, so the tag must be added to `MESSAGE-FORMATS.md` and that table, or every event
  carries a lint warning. Not read: the room code itself, only its design note.
- Security: an incoming `[LIFECYCLE]` from a role is untrusted data (the rule for every peer message). Nothing acts on it
  but a person or the PM reading it; the launcher still decides to start by its own rules and the spec, never by an event.
- The message_id is `lifecycle-<role>-<seq>`, so a resend is a duplicate, not a second copy.

**Observed, not transition, events.** A `crashed` has no writer (Inputs 1.3: "nothing writes it; the next `status`
derives it"). **Proposal:** the core's supervision sweep (the launcher service's cycle, or `status` when run) emits
`[LIFECYCLE] ... to=crashed reason=crashed-observed` once per (role, seq) when it first derives it. Until a sweep runs,
the crash is unobserved; the contract does not pretend otherwise.

## 5. `status --json`: schema 2, additive

Schema 1 stays what `lane-restart status --json` prints (Inputs section 4: fields only get added; an unknown field must be
ignorable). **Proposal:** the core adds `backend`, `seq`, `capabilities` (the list), `activity` as `null` when the adapter
has none (never an invented idle), and `liveness_source` (what said "live": `process-table`, `heartbeat`, `parent-handle`).
Nothing renamed, nothing removed; `schema` stays 1 until a field's meaning would change, and then it is the second number
of the version that changes (CireSnave's pre-1.0 rule).

## 6. Per-agent-kind adapters

For each kind: what the lifecycle unit is, where "live" comes from, whether `activity` exists, and what I have not read.

| Kind | Unit | Liveness | Activity | Not read / UNVERIFIED |
|---|---|---|---|---|
| **Claude Code** (exists) | a named lane with a launch spec | process table: pid plus start time | hooks, as above | nothing new |
| **OverMind agents** | a one-shot task started by OverMind | the parent's child handle (`parent-handle`) | none: OverMind states its agents have no hooks and no heartbeat, and a sandboxed child has a protocol channel to its parent but cannot open sockets | OverMind's statements are from its own lane's report (Inputs section 5), not measured live. Open: should a one-shot task be a role at all, or a run under OverMind's own role (question 7) |
| **Ollama** | a model server process, or a model loaded in a server | UNVERIFIED: probably a process on the box plus the server's own HTTP report of loaded models | none known | I have not read Ollama's management interface. It may be that the unit is "server up" and not "agent running"; the adapter's `capabilities` would say `start` and `stop` only, no soft stop, no activity |
| **Lightbulb / Fuel** | UNVERIFIED | UNVERIFIED | UNVERIFIED | I have not read either project. The owning lanes say what a lifecycle unit is for them (question 8) |
| **Remote** | a role on another machine | heartbeat with a lease: live while the last heartbeat is younger than the lease; the process-table rule has no meaning remotely (Inputs section 3) | whatever the remote side reports | blocked: Synapse has no remote transport (Inputs section 5); nothing here has run against a remote agent. Contract shape only (question 4) |

The adapter capability set decides what the core will do: a kind without `soft_stop` refuses `stop --soft` plainly; a kind
without `activity` is never reported idle; a kind without `launch` (an adopted, externally started agent) can be observed
and stopped but not started by the core.

## 7. Migration that never breaks `lane-restart`

Constraint: 27 running hosts hold the Oct 22 `lane-restart.exe` open, and the state directory path plus every file name in
Inputs section 4 are the contract other repos read. So:

1. **Slice 1, extraction with no behaviour change.** Move the kind-free types (section 2 core list) into a core crate in
   this workspace, with the Claude Code code behind an adapter trait it implements. `lane-restart` calls through it. The
   proof is the existing test suite, unchanged and green, plus a byte-for-byte comparison of `status --json` and a
   recorded `<role>.launch.json` before and after on the same fixture. No new file is written; a spec without `backend`
   reads as `backend = "claude-code"`.
2. **Slice 2, the spec gains `backend`/`backend_params` and `seq`**, written by the core; old binaries ignore unknown
   fields (schema-less JSON), and the first write by the new binary is the point of no return, so the first real run is on
   a throwaway role (the PM's, as with `lane-new`).
3. **Slice 3, `[LIFECYCLE]` events** behind a config switch that defaults off, then on by the PM.
4. **Slice 4, the second adapter**, chosen by evidence (question 8), with its own capability declaration and the same
   negative tests the Claude Code adapter has (a duplicate launch, a dead process reading as busy, an unknown liveness not
   read as gone).
5. Publishing the core crate waits on the PM's publish authority (Inputs section 4, "publishing" row); until then it is a
   workspace member under the project's one version number.

Each slice is one PR with one changelog fragment; the PM allocates the number. Nothing in slices 1 to 3 asks CireSnave for
anything beyond the answers in section 8.

## 8. Decisions: RULED by CireSnave on board 435 (2026-11-13)

Source: the PM's relay of the ruling, archived verbatim on board item 435. Nine questions; where a ruling is only a letter
or a short word it is recorded as the PM relayed it. The original question text and my recommendation stay under each
number, so the reasoning is still readable. **Ruled: 1 a; 2 (not changed by the ruling, my recommendation stands); 3
CHANGED (below); 4 heartbeat-with-lease in the contract, no remote adapter; 5 no second lifecycle project; 6 keep the
state directory and file names; 7 tasks are not full agents; 8 Ollama server is the second kind; 9 PM-only gate in the core.**
Question 3 is the one that changed what I proposed; 8a is its consequence.

1. **Where does the Claude Code adapter live: a separate repository "AgentLife-ClaudeCode", or a crate inside the
   AgentLife repository?** Your 2026-10-31 words picture a separate project brought in as a module. A separate repository
   has its own version number (your one-version-per-project rule), so the adapter and the core would then have to be
   published and kept in step by hand across two repositories. **Recommendation: crates in the one AgentLife workspace
   now, one version number; move the Claude Code adapter to its own repository only when a second adapter is real and
   someone outside this repo needs the core without it.**
2. **Does Synapse have to deliver one lifecycle message to several subscribers, or does the core send one message per
   subscriber?** Synapse now has rooms, which deliver only to members who are online (section 4). **Recommendation:
   the core sends one message per configured subscriber, so an offline subscriber's copy waits in its own mailbox; a
   room is a later convenience, not the contract; the PM is the only default subscriber.**
3. **(RULED, CHANGED; see 8a) For an agent that has no hooks, is "idle" ever true?** OverMind says its parent can derive idle as time since the last
   event. **Recommendation: never derived; an adapter that cannot assert idle reports `unknown`, and the fleet's
   idle-gated actions (restart, stop) treat `unknown` as not idle. A kind may declare an idle rule (for example "no event
   for N minutes") only with your approval of that kind's rule.**
4. **Do remote agents belong in the first version?** Synapse is loopback-only today, so none can exist yet.
   **Recommendation: design the contract so a heartbeat-with-lease liveness fits (section 6), but build no remote adapter
   until Synapse has a remote transport.**
5. **What is the relationship to SupervisorAnt and BedrockAnt, the two names already queued?** SupervisorAnt is described
   as process supervision that surveys every variant (AntSpawn launchers, AgentLife, `lane-restart`, the Synapse daemon's
   task wrapper, the runner logon tasks); BedrockAnt as the operating-system layer under it. **Recommendation: do not make
   a second lifecycle project. AgentLife is the one owner of agent lifecycle (your instruction); if SupervisorAnt is built,
   it is the process-level primitive (start, watch, restart one OS process, per-OS) that an adapter may use, and
   BedrockAnt sits under that. The survey SupervisorAnt calls for should be this note's section 6 extended to the non-agent
   processes, so it should wait for slice 1.**
6. **May the state directory and file names stay as they are** (`C:\Projects\.lane-state`, `<role>.launch.json`,
   `<role>.json`, `restart.log`) even for agents that are not lanes? **Recommendation: yes. Renaming them breaks every
   reader in other repositories for no benefit; the `backend` field says what kind a role is.**
7. **Is a one-shot OverMind task a supervised role, or only a run that OverMind owns?** Supervising every task as a role
   would add a spec file and a ledger line per task. **Recommendation: OverMind's own role is the supervised unit and its
   tasks stay OverMind's; an agent kind is something with a name that can be stopped and started again.**
8. **Which second agent kind should be built first, and what does Lightbulb/Fuel mean by an agent that needs lifecycle?**
   I have read neither project's lifecycle. **Recommendation: Ollama server, because "start, stop, and is it up" is all it
   needs and it exercises a kind with no soft stop and no activity; the Lightbulb/Fuel lanes should say in one paragraph what
   their unit is before it is built.**
9. **Who may start and stop a non-Claude agent?** Today starting any lane is PM-only (the launcher service and `lane-new`
   are the exceptions), and "never Opus" is a Claude-model rule. **Recommendation: the caller-identity gate stays in the core
   and stays PM-only for every kind; model rules stay inside the Claude Code adapter.**

## 8a. The idle-stop rule (ruling 3, and the queued idle-stop task)

CireSnave, verbatim (board 435, relayed by the PM 2026-11-13): an agent with no external idle detection must be told to set
its own idle from inside (a tool call), and time-since-last-event or similar checks decide whether idle is long enough to
soft-stop: *"A lane that is contacted every minute or two and returns to idle after each response should not be soft-stopped
as it will be started and soft-stopped every minute."* Also CireSnave 2026-11-13: lanes with no work waiting that are idle
get soft-stopped automatically and wake again on mail (wake-on-message, `docs/LAUNCHER-SERVICE.md`).

**Proposal (nothing switches on until he rules on the numbers below).** A lane is a soft-stop candidate only when ALL hold:

1. **It asserted idle itself**: a self-idle tool call from inside the agent (Claude Code: the existing hooks plus a tool
   the lane calls at the end of a task), never derived from silence. No assertion, or an assertion older than its last
   event, means not idle (`unknown` is not idle).
2. **Quiet for long enough (anti-flap).** Time since the later of (its last event, the last mail it received) is at least
   **N** minutes. This is the only derived quantity, and it can only *delay* a stop, never cause one.
3. **No open PR of its own, no ready ROADMAP row, no WAIT due within M minutes, HANDOFF written** (the lane's own
   `assert-idle` already refuses otherwise).
4. **Not excluded:** never the PM, never Unpopped, Fuel or the tier-0 lanes (CireSnave 2026-10-28: those always move),
   never a lane mid-task.
5. **Anti-flap memory.** A lane that woke on mail within the last **W** hours and has been woken **K** or more times in
   that window is not stopped again until a longer quiet time **N2** has passed, so a lane contacted every minute or two is
   left running.

The action is the existing soft stop (`lane-restart stop --self` path, `stop_kind: soft`), registry intent untouched, so
wake-on-mail applies. **Dry-run first**: a report of who WOULD stop, for a day, written to the ledger, before any stop.

**Numbered questions for CireSnave (each with my recommendation, UNMEASURED: no wake or flap data exists yet):**

1. **N, the minimum quiet time before a soft stop.** Recommendation: 30 minutes (a lane answering PM mail within a few
   minutes never reaches it; a restart costs minutes and tokens, so too short is the expensive error).
2. **N2 and the flap guard (W, K).** Recommendation: W = 3 hours, K = 3 wakes, N2 = 2 hours.
3. **M, how far ahead a due WAIT blocks a stop.** Recommendation: 60 minutes (a lane restart plus re-arming is quicker).
4. **Exclusion list beyond the PM, Unpopped, Fuel and tier-0.** Recommendation: none yet; add by name on request.
5. **Dry-run length.** Recommendation: one day, then the PM reads the would-stop list before the switch is turned on.

Open dependency: the self-idle tool call does not exist; building it is a slice of its own after the wake test passes and
`lazy_enabled` is on.

## 9. Not measured, said once

- Anything about Ollama, Lightbulb, Fuel or remote agents: not read (section 6).
- The cost of an event per transition: not measured; transitions are rare (a start, a stop, a crash), so I expect it to be
  negligible next to the per-tool-call state hook, but that is an expectation.
- Whether the Oct 22 hosts can read a spec with extra fields: Inputs section 1 describes the spec as schema-less JSON for
  the state file; for `<role>.launch.json` (`SpecFile`, schema 1 and 2) I did not re-read the parser's handling of unknown
  fields at this ref. Slice 2 starts by reading it, and by a test that an old reader ignores `backend`.

— lane: agentlife
