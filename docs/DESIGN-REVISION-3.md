# agentlife — design revision 3: lazy start, idle-to-lazy, and agents that must not be shut down

**Status: PROPOSAL for PM approval. Docs only, no code.** Written 2026-10-06/07 against the PM's
`[TASK] design-revision-2 follow-through`. Builds on `DESIGN-REVISION-2.md` (merged, #4). Where this
document conflicts with revision 2, this one wins; §10 lists exactly what it changes. Revision 2's durable
pending restore, timed approvals and `user-request` crate are **not re-designed here**; §8 only says how the
new mechanisms plug into them.

**CireSnave's ruling, verbatim** (`C:/Projects/CIRESNAVE-DECIDED-ARCHIVE.md`, entry "121 (ruled part 2)"):

> *"On the remaining questions for decision 121: I like the idea of a lazy start (would require integration
> with Synapse). If we have lazy start on-demand, then any agent that is idle when they died should be
> started lazily. Having agents automatically enter that lazy launch on demand state when they've been idle
> for X minutes would make sense. However, agents should also be markable as waiting on the user so they
> don't get shut down while directly interacting with the user. For example, our project manager (you) is
> always there for me to contact. Even if every other agent were shut down into a load on demand state, the
> project manager should never be. The same holds for any agent that is actively waiting for an answer from
> the user in their own chat. I'm not sure how we'll arrange that."*

**Still his, not decided here:** the restore **order** (board item 121); §8 gives the options and a
recommendation only.

**Evidence base.** Synapse `origin/main` `264ceb6` (read with `git show`); OverMind `3bf513f`; Claude Code
hooks docs fetched 2026-10-06; live transcripts and `.lane-state` read 2026-10-07 ~01:28Z. **UNVERIFIED**
items are collected in §12.

---

## 1. What Synapse is today, and what lazy start needs from it

Everything lazy start rests on is a Synapse property that is planned or built; one thing it needs does not
exist.

| fact | where (`264ceb6`) | status |
|---|---|---|
| a lane has a **stable `role@account` identity** that a restarted session takes over; ids no longer rotate | plan `2026-09-29-claude-peers-replacement.md` L37, L98–107 (claim, epoch, `Superseded`) | M3 roles: built (core) |
| **the mailbox belongs to the role, not the session**; mail to a restarting lane is kept and served to the newest epoch | same plan L39; spec `2026-10-02-m4-mailbox-design.md` | M4: built (redb store) |
| **offline roles stay listed and addressable**; `list` returns `online`, `last_seen`, `summary` for every known role | plan L41, L116–117; `m5-synapsed-design.md` L43, L74–75 | M5a/M5b: built (`synapsed`, `synapsectl`) |
| presence is a **90 s heartbeat lease, in memory**; after a daemon restart everyone reads offline until their next call | `m5-synapsed-design.md` L70–76 | built |
| `POST /v1/send` to an offline role returns `queued`; fetch/ack/lease/redelivery on expiry | `m5-synapsed-design.md` L39–41; plan L144 | built |
| channel push into a Claude Code session | plan L147 (M7), spike passed L195 | **M7 not built** |
| **side-by-side then cutover; claude-peers retired** | plan L149 (M9) | **not done** |
| **a hook or query that says "mail is waiting for an offline role"** | grep of the plan and the M3/M4/M5 specs for `subscribe|notify|webhook|on enqueue|wake|watch|event` finds nothing relevant | **does not exist** |

(Control for that last row: the same grep over the same files does find the `offline`, presence and `list`
lines cited above, so the query works; it finds nothing about waking.)

Three consequences, each a design constraint:

1. **Lazy start is gated on the lanes actually using Synapse for messages.** Today lanes talk through
   claude-peers, whose broker evicts a dead peer (plan L41), so a message to a stopped lane *fails*: there
   is no mailbox to wait in. Until M7 and the M9 cutover, a lazy agent would be simply unreachable. Revision
   3 therefore defines lazy start fully but ships it **off** (`lazy_enabled = false`) and makes restore fall
   back to eager start (§6) until the Synapse path is verified.
2. **agentlife needs one small read-only addition to the daemon**: a per-role *pending* count (queued,
   leased, oldest-enqueued time). `list` today carries no mailbox depth (L43). Spelled out in
   `SYNAPSE-REQUIREMENTS.md`.
3. **agentlife is itself a Synapse role** (`agentlife@<account>`): `/v1/list` needs a session (L43), so the
   watcher claims a role. Its sends carry a verified `from` (L47), which is what makes the audit in §3.4
   meaningful.

---

## 2. The agent model, extended

Revision 2 had `intent ∈ {wanted, closed{parked|exited}}`. Revision 3 adds one value:

| intent | meaning | restore starts it? | wakes on mail? |
|---|---|---|---|
| **`wanted`** | should be running while the machine is up | yes | n/a |
| **`lazy`** | known, not running, **starts when mail arrives for it** | no | **yes** (§3) |
| **`closed{parked}`** | a person or the PM stopped it on purpose | no | **no** by default (R3-Q1) |
| **`closed{exited}`** | the person ended the session | no | no |

`lazy` is a *resting* state, not a failure: no HANDOFF is stale-flagged for it, no restore warns about it,
and it counts as zero against the memory floor. Transitions:

```
wanted --(idle ≥ X min, not exempt, HANDOFF gate passes, stop succeeds)--> lazy      (§4)
lazy   --(mail pending, grant valid, caps allow, launch settles)---------> wanted    (§3)
wanted --(machine went down while it was idle)--------------------------> lazy       (§7, at restore)
wanted --(park/stop)--> closed         lazy --(park)--> closed        closed --(unpark)--> wanted
```

Exempt agents (§5) never take the first and third arrows.

---

## 3. Lazy start via Synapse

### 3.1 The wake path

1. A sender calls `/v1/send` to `R`, an offline role. Synapse queues it durably (built).
2. **`agentlife watch`**, a small resident process started at logon beside the restore task, calls
   `/v1/list` every `wake_poll_secs` (default **10**) and reads the new `pending` counts.
3. For each registry agent with `intent = lazy` whose Synapse role has `pending.queued > 0` and `online =
   false`: **wake** it.
4. A wake is **a plan of one** (revision 2 §4, §6): the agent's stored argv, launched through `lane-restart
   host`, into the next window slot. It is subject to the same checks as any launch: hard rules, RAM floor,
   `max_running`, **and consent** (§3.3).
5. On launch the new session claims `R`; Synapse serves the mailbox to the newest epoch; the pushed message
   is the lane's first input after its `read <role> HANDOFF` prompt. Redelivery on lease expiry (plan L144)
   means a message the session fetches and dies on is **delivered again**, not lost.
6. `agentlife watch` marks the agent `wanted` once liveness settles (revision 1/2 §2.3) and journals
   `woke: {agent, sender_role, message_ids, plan_hash}`.

**Latency**, from measured parts: ≤ `wake_poll_secs` + host-start-to-working, median **12 s**, max **123 s**
(revision 1 §6.1, n = 46, single starts, not concurrent). Expect roughly 10–25 s typical; unmeasured under
contention.

### 3.2 Why polling first

A push (`/v1/events`, SSE) would drop the poll latency and the idle CPU. I recommend **poll first**: the
daemon change is a read-only field, there is no new connection state to get wrong on Windows (where the
daemon lifecycle already "spent several fixes", plan L204), and a poller that dies is restarted by the
logon/unlock task rather than leaving a half-open stream. Push is an optimisation to add once the poll
version is proven. If the Synapse lane would rather build the stream once, agentlife consumes it.

### 3.3 Waking is agent-originated, so it is untrusted by default

A wake is a launch caused by **another agent's message**. The portfolio rule (and `DESIGN.md` §4.4) is that
a start requested by an agent needs a person's consent. So:

- A wake needs a **valid standing grant that covers `wake`** for that agent (revision 2 §7: named agents,
  max mode, flags, duration chosen by the approver, revocable, audited). Granting is the person's one-time
  "this agent may be started on demand, at this mode, until this time".
- **No grant ⇒ the wake becomes a durable pending request** (revision 2 §6): the agent stays `lazy`, the mail
  stays queued, and the control tab / `agentlife pending` shows *"mail from `<sender>` is waiting for
  `<agent>`; start it?"*. No timeout; the mail waits.
- **Hard rules still apply**: a non-PM `bypassPermissions` agent is refused regardless of grant (revision 2
  §7.2, R2-Q2 unanswered).
- **Whether certain senders may wake without a grant** (the PM's mail waking a lane) is **R3-Q2**.

### 3.4 Storm and abuse limits

A message from any lane can start any lazy agent it has been granted, so loops matter (A mails B mails A;
a script mails 200 agents). Controls, all configurable defaults (§9), none a ceiling on fleet size:
`max_wakes_per_agent_per_hour` (default 6), a global `max_wakes_per_minute` (default 10), the RAM floor and
`max_running` (revision 2 §5), and **debounce**: many messages to one lazy agent produce one wake. Every wake
is journaled with the sender's **Synapse-verified role** (L47), so "who started this" is answerable. A
wake that a limit blocks stays pending, shown to the person as such, and the mail is never dropped.

### 3.5 Binding an agent to a Synapse role

The registry needs `agent ↔ synapse role`. Proposal: **the role is the agent's `name`** (both are already
unique, charset-restricted identifiers: `DESIGN.md` §6), with an optional `synapse_role` override recorded at
registration, and the account part fixed by config. A bound role missing from `/v1/list` is reported at
restore ("lazy agent X has no Synapse role; started eagerly instead", §6), never silently skipped.

---

## 4. Idle → lazy after X minutes

### 4.1 What "idle" means, and the evidence it cannot come from the role file

A lane is **idle** only if *all* of: no turn in progress, no running subagent, no live shell descendant (the
existing `assert-idle` signals, `facts::has_live_shell_descendant`, `subagents_running`), **and** no session
activity for `lazy_after_idle_minutes` (**X**, default **120**).

"No session activity" must be read from the **session's transcript** (`~/.claude/projects/<dir>/<session
id>.jsonl` mtime and last entry), *not* from the `.lane-state/<role>.json` file. Measured 2026-10-07 01:28Z on
the live `mlmf` lane: its state file's last event was **5,013 minutes** ago while its transcript was written
**21 minutes** ago. That is the state-file role-by-cwd defect (`DESIGN.md` §0.2: a lane that `cd`s into a
worktree writes a *different* role's file), and it would have made an active lane look 3.5 days idle and
stopped it. The join is by `session_id` (revision 1 §2.4), and idle uses the transcript.

### 4.2 The sweep, and what it costs

`agentlife watch` also runs an **idle sweep** every `idle_sweep_secs` (default 60): for each `wanted` agent
that is idle by §4.1 and not exempt (§5), start a **lazy-stop**:

1. **Re-check at the last moment** (idle conditions again, plus: Synapse reports **no queued and no leased**
   mail for the role). A message that arrived after the sweep began cancels the stop.
2. **HANDOFF gate** (policy `idle_stop_policy`, §9):
   - if the HANDOFF is **fresh** (modified after the session's last activity), stop at **zero model cost**;
   - if it is stale, `ask-to-write` sends the lane a `[TASK]` to write HANDOFF and `assert-idle`, which
     costs **one model turn** on the idle lane (the lane's cached context is re-read, order of 50 k tokens,
     revision 1 §6.1), then stops;
   - `skip` leaves a stale-HANDOFF lane running.
   Default proposed: `fresh-else-ask`. Lanes that restart at task boundaries (portfolio `CLAUDE.md` §9) are
   mostly fresh when idle (`DESIGN.md` §7 measured 0.0 to 2.0 h behind for recently restarted lanes), so most
   lazy-stops cost nothing; the long-running stale ones cost one turn.
3. **Graceful stop** exactly as `down` (`DESIGN.md` §3), then set `intent = lazy`, journal `lazy-stopped`.

A stop that fails or is refused leaves the agent `wanted` and running, reported; **never a forced kill**.

### 4.3 Measured on the live fleet (a snapshot, not a trend)

The 14 real lanes at 2026-10-07 ~01:28Z, minutes since the session's last activity: twelve were active within
5 minutes (it was minutes after the token pause ended, so **this is the least idle the fleet will ever look**); `mlmf` ≈ 21 (by transcript, not by its state file);
`coderipper` ≈ 4; **`kiss` ≈ 3,434 (57 h)** with a live process, idle, state `Stop`. At X = 120 only `kiss`
qualifies. `kiss` is the motivating case: a ≈ 0.65 GB client held for 2.4 days doing nothing. (29 further
state files belong to dead or phantom roles and are not lanes, `DESIGN.md` §0.2.) The numbers say the
mechanism would have done almost nothing in this snapshot and would pay off in the quiet hours; they do
not say how much, and I have no trend data.

---

## 5. Exemptions: the PM, and agents waiting on the user

He wrote *"I'm not sure how we'll arrange that."* The PM's candidate signals were: the session's last event is
a question to him or a permission prompt, he typed to it within N minutes, plus an explicit pin. I tested
each against real data.

### 5.1 Signals, ranked by how much I trust them

| # | signal | strength | source | measured |
|---|---|---|---|---|
| **S1** | **pin**: `agentlife pin <agent>` | **definitive** | registry | the PM is pinned (§5.2) |
| **S2** | **explicit mark**: the agent runs `agentlife waiting --on user [--note …]` and `--clear` | strong | registry, written by the agent | new; mirrors `assert-idle` |
| **S3** | **permission or input prompt pending**: a `Notification` hook event of type `permission_prompt`, `idle_prompt`, `elicitation_dialog` or `agent_needs_input` | strong | hook (optional extra entry, §5.4) | matcher values are **documented** (hooks docs, 2026-10-06); *when* each fires is **not** documented |
| **S4** | **he typed to it recently**: the last transcript entry with `origin.kind == "human"` that is not the launch prompt is within `user_recent_minutes` (default **30**) | soft | transcript, no hook | see below |
| **S5** | the last assistant message is a question | **weak, not used alone** | `Stop`'s `last_assistant_message` (documented) or the transcript | would match questions to the PM, not to him |

**S4 is real and cheap, with one trap.** Every transcript user entry carries `origin`: across the 60 most
recent transcripts the kinds are `human` (285), `channel` (1,110), `peer` (521) and `task-notification`
(1,882), i.e. **typed prompts are distinguishable from peer and channel messages by a field, with no new
hook**. The trap: **the restore/launch prompt `read <role> HANDOFF and continue` is itself
`origin: human`** (31 of the 285). S4 must therefore exclude it (match the prompt agentlife/`lane-restart`
passes), or every freshly restored lane would look like it was just typed to. The other 254 `human` entries are not launch prompts; the four I read were genuine person-typed
messages.

**Why S5 is not used alone.** An agent whose last message ends in a question may be asking *the PM* or a
peer, not him. Using it as a trigger would pin most of the fleet. It can only *add* a hint for the review
("last message is a question"), never exempt by itself.

**Measured at 01:28Z:** of the 14 real lanes, only `pm` (1,459 min), `overmind` (6,049), `thinkersjournal-
community` and `auth-framework` (9,676 each) had any non-launch human input in their *current* session, and
**none within 30 minutes**, including the PM, whose last typed input in this session was about 24 h earlier.
That is the strongest argument for **S1 as a hard pin**: the PM is "always there for me to contact" and S4
would still have called it idle.

### 5.2 The PM is pinned, by a rule that is visible

`pin_roles` in config (default `["pm"]`) pins the agent whose launch directory is the portfolio root and
whose role/name matches; `agentlife list` shows `pinned: yes (rule)`. A pinned agent is **never lazy-stopped
by the idle sweep, never lazy-classified at restore, always eager at restore, and always first**. A person
may still `park` it explicitly (R3-Q3 asks whether even that should need extra confirmation, since it is
the one agent he said must always be there).

### 5.3 Precedence, and which way each error falls

Exempt if **S1** or **S2** (until cleared or `waiting_mark_ttl_hours`, default 24) or **S3** (until the next
`human`-origin transcript entry) or **S4** within `user_recent_minutes`.
Errors:

- *false exempt* (agent kept running that could have been lazy): costs ≈ 0.65 GB. The safe direction.
- *false idle* (agent lazy-stopped while a person was about to answer): the person's pending answer lands
  as mail in a stopped agent's mailbox, wakes it (§3), and the conversation resumes, so it is **recoverable
  and visible**, which is why the stop is allowed at all for non-pinned agents.

S2 needs the lane to cooperate, so it is a convention, not an enforcement: it belongs in each lane's
instructions next to `assert-idle` (a one-line addition the PM would make; I do not edit other repos'
instructions).

### 5.4 The hook this adds, and why it is optional

S3 needs `Notification` entries in `settings.json` (types listed above). That is **a third event beyond the
two the PM approved for R2** (`SessionStart`, `SessionEnd`), though far less frequent than `PreToolUse`. S1,
S2 and S4 need **no new hook**, so revision 3 works without it, and S3 is offered as an upgrade. **R3-Q5.**

---

## 6. Restore with lazy

Revision 2 §3 restored every candidate. Revision 3 classifies each candidate first:

```
candidate (wanted, not alive)  ──►  pinned or waiting on user ───────────► EAGER (start now)
                               ──►  busy at death ────────────────────────► EAGER
                               ──►  idle at death, Synapse role bound
                                    and Synapse path verified ───────────► LAZY (set intent=lazy, do not start)
                               ──►  idle at death, no Synapse path ──────► EAGER (fallback, reported)
```

**"Idle when they died"**, precisely: at the shutdown-start marker (revision 1 §2.6) the agent satisfied §4.1's
idle test *as of its last recorded activity* (the transcript's last entry before the marker was a completed
turn, and its last state event on that session was `Stop`, no subagent running). **Unknown counts as busy**:
a lane whose evidence is missing or contradictory is started eagerly. The reason: a lane **mid-task**
mis-classified idle would sit lazy with work in flight until someone happened to message it, and the
`PreToolUse`-lock-timeout log (`hook-errors.log`) shows busy flags *are* sometimes lost in the unsafe
direction. Both signals (state and transcript) must say idle.

**Until Synapse is the messaging path (M7/M9), nothing is classified lazy**: every candidate is eager, as in
revision 2, and the review says so. `lazy_enabled = false` is the shipped default (§9).

Eager agents are still ordered, batched and capped exactly as before; lazy ones cost no start and no
batch slot, which is the point of CireSnave's idea.

---

## 7. Restore order (his decision; options and a recommendation only)

Board 121 asks: *PM first, then most recently active first, or should some lanes (Synapse, OverMind) always
come early?* **Not decided here.** Options:

| option | for | against |
|---|---|---|
| **A. PM first, then most-recently-active** (revision 1 §3.1) | no hand-kept list; the lane most recently working is likely most wanted | OverMind/Synapse may not be the most recent yet are needed early |
| **B. PM first, then a `priority` list in `overrides.json`, then recency** | lets him name early lanes (Synapse, OverMind) | a list to maintain; the thing he wanted to avoid, in miniature |
| **C. PM first, then dependencies first (Synapse before the lanes that message through it)** | correct when lanes depend on a service lane | needs a declared dependency; today only Synapse is such a service |

Recommendation: **A with B as an optional nudge** (a short `priority` list that defaults to empty). With
lazy start on, order matters much less: only the eager set is ordered, and a lazy agent starts when needed
regardless of order. One caution that is not an ordering question: **Synapse must be up before agents that
claim roles**; `synapsed` auto-starts on first use (M5b, `m5-synapsed-design.md` L77–83), so no ordering is
required, but the logon wait should verify `/v1/health` before the first lane that uses it. "Say `default`"
(his phrase on the board) maps to A.

---

## 8. What stays as in revision 2

Durable pending restore with no timeout, approver-chosen durations (hour, day, until a date, forever, with
typed confirmation for long ones per OverMind's approved plan), revocation, audit, HMAC-signed grants, and
the `user-request` crate with `bound_hash`: all as written. Revision 3 adds exactly two things to them:
(1) a grant may **cover `wake`**, separately from `launch` and `unattended_restore` (revision 2 R2-Q4), and
(2) a wake blocked by no grant becomes a **pending request** shown as "mail waiting for <agent>".

---

## 9. Configuration additions (defaults, never ceilings)

| key (flag) | default | meaning |
|---|---|---|
| `lazy_enabled` | **false** | master switch; stays false until the Synapse path is verified (§1) |
| `lazy_after_idle_minutes` (X) | **120** | idle time before an agent is lazy-stopped; `0` disables the sweep |
| `idle_sweep_secs` | 60 | how often the watcher checks |
| `idle_stop_policy` | `fresh-else-ask` | `fresh-else-ask` \| `fresh-only` \| `ask` \| `skip` (§4.2) |
| `user_recent_minutes` | 30 | S4 window |
| `waiting_mark_ttl_hours` | 24 | S2 mark lifetime unless cleared |
| `pin_roles` | `["pm"]` | S1 rule (§5.2) |
| `wake_poll_secs` | 10 | watcher poll interval |
| `max_wakes_per_agent_per_hour` | 6 | storm limit |
| `max_wakes_per_minute` | 10 | global storm limit |
| `synapse_addr` | `127.0.0.1:7920` | from Synapse's own default (M5 spec Q1) |
| `synapse_account` | `ciresnave` | the account part of `role@account` |

Same precedence as revision 2 §5 (flag > env > file > default); unknown keys refuse the file.

---

## 10. What this changes in revision 2

| revision 2 | revision 3 |
|---|---|
| `intent ∈ {wanted, closed}` (§2) | adds **`lazy`**, with its own transitions (§2) |
| E3 "lazy start" listed as a later option in revision 1 §6.4 | **chosen by CireSnave and designed here**, but **off** until Synapse carries messages |
| §3 restore starts every candidate | candidates are classified eager/lazy (§6) |
| exemptions: none | pin + waiting-on-user (§5) |
| `agentlife watch` did not exist | new small resident process: wake poller + idle sweep (§3, §4) |
| agentlife not a Synapse participant | agentlife is a role `agentlife@<account>` (§1) |

Revision 1's "no resident process" stance is deliberately given up for exactly one small component, the
watcher, because lazy start *is* a resident listener. Everything else stays a short-lived command. If the
watcher dies, the logon/unlock task restarts it; lazy agents' mail simply waits (durably) in the meantime.

---

## 11. Reused / new

| piece | status |
|---|---|
| Synapse `/v1/list`, `/v1/send`, mailbox, presence, `synapsed` auto-start | **reused as built** |
| `pending` counts in `/v1/list` | **new, small, in Synapse** (`SYNAPSE-REQUIREMENTS.md`) |
| channel push of the woken session's mail (M7) and the cutover (M9) | **Synapse's, prerequisite**, not built |
| idle test (`assert-idle` signals, `has_live_shell_descendant`, `subagents_running`) | **reused** (`lane-restart` library) |
| transcript reading (activity, `origin.kind`) | **new**, read-only |
| `agentlife watch`, idle sweep, wake poller, wake limits | **new** |
| `waiting`, `pin`, `unpin` commands; `pin_roles` rule | **new** |
| `Notification` hook entries (S3) | **new, optional** |
| grants covering `wake`, pending request for an ungranted wake | **new, on top of** revision 2 §6–§7 |

---

## 12. Test additions

- **Classifier table (§5, §6)**, each row against injected transcript and state fixtures, asserted by agent
  name: pinned/idle; marked waiting; typed 5 min ago; typed 90 min ago; **launch prompt is `human`
  origin but must not count**; peer/channel messages must not count; last message a question but no
  other signal; stale `mlmf`-style state file with a fresh transcript (must read active); no transcript
  (unknown ⇒ busy ⇒ eager).
- **Idle sweep races:** mail arrives between sweep and stop (Synapse reports queued ⇒ stop cancelled);
  lease outstanding ⇒ no stop; stop fails ⇒ stays `wanted`.
- **Wake path with a fake Synapse** (a loopback stub implementing `/v1/list` with `pending`, `/v1/health`):
  mail to a lazy agent wakes exactly once (debounce); no grant ⇒ pending request, mail untouched; storm
  limits hold; sender role is journaled.
- **A real Synapse integration test** against a real `synapsed` over loopback (the way M5a tests it),
  once `pending` exists: queue mail, assert the watcher launches a stand-in agent that claims the role and
  receives the mail. This is the test that must pass before `lazy_enabled` can ever default to true.
- **Restore classification:** idle-at-death ⇒ lazy; busy-at-death ⇒ eager; missing evidence ⇒ eager;
  pinned ⇒ eager first; Synapse down ⇒ eager fallback, reported.
- **Scale sim:** 1,000 lazy agents, 50 woken at once with `max_running`/RAM floor injected: no more than the
  caps run, the rest stay pending, none lose mail.

---

## 13. UNVERIFIED

1. Whether `pending`/`leased` counts are cheap and consistent to expose (Synapse's call).
2. When each `Notification` type fires (docs list the types, not the circumstances).
3. That the `origin.kind` field is stable across Claude Code versions (observed in 60 transcripts; not
   documented as a contract).
4. Wake latency under contention (only single starts measured).
5. That M7's channel push delivers the queued mail promptly to a freshly launched session.
6. That the `read <role> HANDOFF and continue` prompt remains the only launch prompt S4 must exclude.

---

## 14. Numbered questions for CireSnave (via the PM)

- **R3-Q1. Does mail to a *parked* agent wake it?** Proposed: **no** (parked means off); `lazy` is the state
  that wakes. The alternative makes `parked` and `lazy` the same thing.
- **R3-Q2. May certain senders wake an agent without a grant?** E.g. the PM's mail always wakes. Proposed:
  **no exception**; the PM's request goes through the same grant, which is cheap to give once.
- **R3-Q3. May the PM itself be parked?** It is exempt from idle and lazy; should an explicit person-issued
  `park` of the PM need an extra confirmation?
- **R3-Q4. When a lane is idle but its HANDOFF is stale, which is right?** (a) ask it to write one, costing
  one model turn (proposed); (b) stop it anyway; (c) leave it running.
- **R3-Q5. Is adding `Notification` hook entries acceptable** as a third event beyond `SessionStart`/
  `SessionEnd`, for the stronger waiting-on-user signal (S3)? Without it S1, S2 and S4 still work.
- **R3-Q6. May a lane pin itself or mark itself waiting?** Proposed: yes (it only costs memory, so it errs
  safe); a person can unpin.
- **R3-Q7. Defaults.** X = 120 min idle, 30 min for "he typed recently", 24 h for a waiting mark, 6 wakes
  per agent per hour. These are guesses to be adjusted; say if any matters to you.
- **R3-Q8. Restore order** (board 121, §7): say `default` for option A, or name early lanes.
