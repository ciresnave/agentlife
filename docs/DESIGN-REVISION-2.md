# agentlife — design revision 2: park/stop as everyday operations, durable consent, timed approvals

**Status: PROPOSAL for PM approval. Docs only, no code.** Written 2026-10-04 against the PM's
`[TASK] design-revision-2`. Builds on `DESIGN-REVISION-1.md` (which builds on `DESIGN.md`); where this
document conflicts with either, **this one wins**, and §1 lists exactly what it supersedes.

**CireSnave's ruling, read in full from `C:/Projects/CIRESNAVE-DECIDED-ARCHIVE.md`, entry "121 (ruled
parts)" (not paraphrased; the parts that drive this revision, verbatim):**

- *"…a user or project manager agent should be able to park a lane or otherwise stop a lane. However,
  that isn't so much a question about restart as it is something that would happen during normal use of
  the lane. It still needs to be addressed. Lanes should be parkable so that they are still known about
  and can be launched again quickly but are not always required to be running."*
- *"On Q-D3: the answer is both. The free RAM floor is fine at 8GB but should be configurable."*
- *"On Q-D4, yes...any lane that is intentionally closed should remain closed unless intentionally
  reopened. 10 tabs per window is a good default but should be configurable. Restore can create its own
  layout for now."* and *"Retention of 14 days should be good as a default but should be configurable."*
- *"Do we need a Windows Hello timeout? … if the lanes restart when I am not at my PC but I come back 3
  days later (well after any timeout would have happened), I would still want to be able to authorize
  those and proceed. If a timeout cancels them, how would I pick them back up after I return? Is there
  some reason we need a timeout?"*
- *"Perhaps we should create a separate "user-request" crate? That way it can implement Windows Hello,
  SMS requests, push requests, etc. and code requesting something from a user could use them
  interchangeably?"*
- *"Should approvals to bypass prompts on lane launches ask the approver how long they approve those for?
  Some should be approved for an hour. Others should be approved for a day. Still others might need to be
  approved until a specific date/time. There would likely even be some that should be approved forever."*

**Evidence base.** OverMind `origin/main` `3bf513f`; Claude Code hooks documentation fetched 2026-10-04
(`code.claude.com/docs/en/hooks`); runtime facts as in revision 1. **UNVERIFIED** items are collected in §13.

---

## 1. What this supersedes, and one correction to my own revision 1

| revision 1 | revision 2 |
|---|---|
| §2.6 "deliberately closed" = `SessionEnd` before shutdown | replaced by the precise **intent rule**, §3 |
| §3 restore: consent, then execute; a prompt that nobody answers times out and starts nothing (§4.2) | **no timeout; a durable pending restore**, §6 |
| §4.2 one batch Hello per restore | kept as the *plan consent*, now **durable**, plus **timed standing grants** that can make a restore unattended, §7 |
| §4.4 `overrides.json` holds `park` flags | park is a **first-class registry state with its own commands**, §3–§4 |
| §6.3 `tabs_per_window` 10, memory floor 8 GB, `max-running`, retention 14 days, "proposed" | all **configurable defaults**, §5 |
| §2.5 placement "assigned and recorded so a restore reproduces the layout" | **restore picks its own layout**; nothing about layout is a requirement, §5.3 |
| §9 Q12 / `OVERMIND` reply: extract a small `hello-consent` crate | folded into a shared **`user-request`** crate, §8 |

**Correction (a finding of mine that was framed wrongly).** Revision 1 §6.2 says *"Hundreds running at
once cannot fit this machine."* CireSnave's ruling is right that this conflates two things: the models run
in the cloud; what the measured **≈ 0.65 GB per lane** is, is the **local Claude Code client process**
(plus its two `bun` helpers). This machine serves *requests* for file reads and writes well; the limit that
is real is **local client memory**, solved by parking plus, at that scale, a machine with far more memory.
So the design obligation is "scale, don't cap": **no ceiling is hard-coded anywhere** (§5, §11). The
token-cost concern in revision 1 §6.2 is a cloud-side spend concern and stands as written. Revision 1 is a
dated record, so its §6.2 gets a "superseded by revision 2 §1" note beside it (this PR), not a rewrite.

---

## 2. The model: wanted, closed, running

Three independent facts per agent, kept apart because conflating them is what made the first design
brittle:

- **liveness**: is the agent's process (pid + start time) alive right now. Derived from the process table,
  never stored as truth.
- **intent**: **`wanted`** (default; the agent should be running whenever the machine is up) or
  **`closed`**, with `closed_by`, `closed_how`, `closed_at`.
- **record**: the registry facts of revision 1 §2.3 (argv, cwd, name, mode, sessions).

An agent is a **restore candidate** iff `intent = wanted` and it is not alive. Everything the PM asked for
follows from making `intent` explicit, evidence-based and sticky.

`closed_how` takes one of:

| `closed_how` | meaning | counts as intentional | pruned after retention? |
|---|---|---|---|
| **`parked`** | closed by `agentlife park`: "known, quickly relaunchable, not required to run" | yes | **never**: a parked agent is *known* (CireSnave's word), so retention does not apply |
| **`exited`** | the person ended the session from the prompt (§3.2) | yes | yes, after `retention_days` (default 14) |

Parked and exited are the same state for restore (both stay closed until reopened). They differ in two
ways that matter: parked agents are listed prominently and exempt from pruning, and `exited` ones age
out. Whether one state with two labels is what he wants is **R2-Q1**.

---

## 3. Closed on purpose: the precise rule (Q-D4)

"Any lane that is intentionally closed should remain closed unless intentionally reopened." Intent has to be
told apart from a crash or a Windows-update kill using evidence a machine can read. Evidence, strongest
first:

### 3.1 What counts as intentional

1. **An `agentlife` command did it.** `park`, `stop`, or a graceful `down` (`DESIGN.md` §3) writes a
   `closed` journal event **before** the process is touched. This is authoritative, because agentlife is
   the actor and knows `requested_by`. Who may issue it: §4.
2. **The session ended from the prompt, and nothing explains it otherwise.** `SessionEnd` is documented
   to fire with a matcher that is the *reason*: `clear`, `resume`, `logout`, `prompt_input_exit`, `other`
   (hooks docs, fetched 2026-10-04). The rule treats the first two as *non-closes* (the process
   continues) and the next two as the signal:
   - `clear` / `resume`: the **process continues** into a new session (`SessionStart` with source `clear`
     or `resume`); **never** a close.
   - `prompt_input_exit` or `logout`: a candidate for intentional, **provided** the process is now gone
     **and** the event's time is **before** the shutdown-start marker (revision 1 §2.6), **and** the
     session was not `superseded` by a restart (`lane-restart --self` kills without firing `SessionEnd`,
     so a successor session of the same `agent_id` is the tell).
   - `other`: the docs call it the catch-all. **Not** treated as intentional.
3. **A person hand-launching the agent again clears `closed`** (`reopened_by: hand`), because starting it
   is exactly "intentionally reopened".

### 3.2 What does not count (so it is restored)

- No `SessionEnd` at all, process gone, **a shutdown began after the agent's last activity**: killed by
  Windows. Restore it. (This is the case that motivates the tool.)
- `SessionEnd` with reason `other`, or one that fires **after** the shutdown-start marker: restore.
- No `SessionEnd`, no shutdown, process simply gone: a **crash**. Not intentional; a candidate when
  someone runs `agentlife restore`, and listed as "crashed" in the review.

### 3.3 The hard case I cannot decide for him

A person who **closes the terminal window with the X** kills the process without a `/exit`. The docs do not
say whether `SessionEnd` fires on terminal close, crash or shutdown ("does not explicitly list all possible
termination scenarios" as of the fetch). If it does not fire, that action is indistinguishable from a
crash and the lane would be restored, against his wish. Mitigation proposed: those agents are shown in
the review under **"closed without /exit, not at shutdown"**, **unselected by default**, with one key to
select all. The opposite default (restore them, flagged) is the other defensible choice. **R2-Q3.** The
canary experiment of revision 1 §10 is extended to settle what the hook actually sees (§12).

### 3.4 Evaluation is persisted

Interpretation happens in `agentlife` at restore/`list` time (the hook only records facts), but **the
result is written to the journal as a `closed` event the first time it is derived**. A later boot, with
a different shutdown marker, cannot reverse an earlier decision.

---

## 4. Park, stop, unpark, list: everyday commands

| command | effect | consent |
|---|---|---|
| `agentlife park <agent>` | graceful `down` of the agent if running (`DESIGN.md` §3: ask it to write HANDOFF and `assert-idle`, then stop); sets `closed{how: parked}`; keeps the record forever | person: none. PM agent: only on an **idle** lane, `--yes` (the lane-restart rule). A lane parking **itself** (`park --self`): none. Any other agent: **consent** (it stops someone else's lane). |
| `agentlife stop <agent>` | same stop, sets `closed{how: exited}` (will age out) | same as `park` |
| `agentlife unpark <agent>` | clears `closed`, then launches through a **plan of one** (§6, §7), at the stored argv, through `lane-restart host` so the dev-channels dialog is still answered | **widening**: needs consent per §7 unless a valid grant covers it |
| `agentlife unpark --no-start <agent>` | clears `closed` only; the agent becomes a restore candidate | none (starts nothing) |
| `agentlife list [--closed] [--all]` | agents with intent, liveness, mode, last seen, HANDOFF age, parked/exited | none |
| `agentlife pending …`, `approvals …` | §6, §7 | see there |

Everything above is "normal use", not a restore feature: `park` works on a running lane at 2 p.m. on a
Tuesday. **Why parking is cheap to reverse:** the record keeps `launch_args`, `cwd`, `name`, `model`,
`permission_mode` and the HANDOFF path, so `unpark` is one command, not a hunt (CireSnave: *"known about and
can be launched again quickly"*).

**Unselecting an agent in the restore review is not parking.** It is **`deferred`**: it stays a candidate
and stays in the pending restore (§6). Closing is a separate, explicit act. This keeps "I did not want it
*this time*" from silently turning into "closed on purpose" (**R2-Q8** if he wants it otherwise).

---

## 5. Configuration: every number is a default, never a ceiling (Q-D3)

`C:/Projects/.agentlife/config.json`, plus flags; **precedence: flag > environment variable > config file >
built-in default.** Unknown keys refuse the file (a typo must not read as "no limit"). Narrowing keys may be
set by anyone who can write the file; **nothing here can widen authority** (that is §7's job).

| key (flag) | default | meaning |
|---|---|---|
| `free_ram_floor_gb` (`--free-ram-floor`) | **8** | stop starting agents when available physical memory falls below this; held agents are reported "held for memory", not failed. Relative to the machine, so a bigger machine simply starts more. |
| `max_running` (`--max-running`) | **none** | optional hard cap on concurrent agents, counting already-running ones |
| `tabs_per_window` (`--tabs-per-window`) | **10** | agents per `agentlife-<k>` window |
| `retention_days` | **14** | `exited` records older than this are pruned; **parked records and the journal are never pruned** |
| `batch_size`, `batch_delay_secs` | 3, 45 | restore pacing |
| `liveness_timeout_secs` | 120 | per-batch settle bound (not a consent timeout; §6) |
| `network_wait_secs` | 120 | logon wait for `gh`/network before starting (the measured 2026-10-01 approvals failure) |
| `approval_default_duration` | `1d` | the *preselected* choice in the duration chooser (§7.3); `forever` can never be the default |
| `pending_nag` | `once-per-unlock` | how often a waiting pending restore re-notifies (§6.4) |

### 5.1 How the design scales rather than caps

Nothing here is `O(agents)` in a way that bites at hundreds, and nothing is a constant that a bigger
machine could not raise:

- one registry file per agent; a write never rewrites the fleet;
- the journal is **append-only, segmented by month** (`journal-YYYY-MM.jsonl`; explicit named segments, not
  silent rotation, so the "never rotated silently" discipline of `restart.log` holds);
- ordering, batching and windows are computed, not enumerated; `agentlife-<k>` windows are created on
  demand;
- the memory floor and `max_running` are *relative* checks, so the same binary on a machine with 1 TB
  behaves correctly with no recompilation;
- the control tab shows **summaries and exceptions**, never an unbounded list (§6.3).

### 5.3 Layout

*"Restore can create its own layout for now."* Placement is **assigned by restore** (window
`agentlife-<k>`, tab titled by agent name, `k` = index ÷ `tabs_per_window`), recorded only for the report,
and **never read back as a requirement**. If layout becomes an issue he may revisit it; no design depends
on it.

---

## 6. A restore that waits as long as it takes

### 6.1 Why there is no timeout

I looked for a reason to have one. `with-secret` times out because *a command is blocked on the answer*
(design §2.2). A restore after a Windows update has no such blocker: nothing is harmed by waiting, and
CireSnave's scenario (back after three days) is unserved by any timeout. A timeout would also create a
state to recover from ("what did the cancelled restore leave behind?"). So: **no timeout, and no process
waits.**

### 6.2 The persisted pending-restore record

`C:/Projects/.agentlife/pending/<id>.json`, written atomically **at the moment the plan is built and any
part of it needs consent**: `created_at`, the boot it belongs to, `reason`, the **entries that wait** (each
with `agent_id` and *why* it waits: no grant, new agent, widened mode, …), and `notified_at[]`. It is
**state, not a process**: the logon task builds the plan, writes the record, notifies, and **exits**.
Nothing holds a Hello dialog open for days (a dialog would not survive lock, sleep, log-off or reboot).

Covered entries do not wait: if a valid standing grant (§7) covers an agent, it starts at logon without a
prompt, and only the uncovered remainder is pending. A restore after an update can therefore be fully
unattended for agents he granted and wait only for the rest.

### 6.3 Picking it up on return

Triggers, any of which is enough, none of which depends on the others:

1. **Logon task** (always checks for pending first).
2. **Session-unlock trigger**: the same task also has a Task Scheduler *session state change → unlock*
   trigger that runs `agentlife pending --prompt`: if pending records exist it opens the control tab
   with the plan summary and asks. **UNVERIFIED** that this trigger can be created from `schtasks`/XML on
   this machine without elevation; it is a standard Task Scheduler trigger type, and §12 tests it.
3. **`agentlife pending`**, any time: `pending` (list), `pending show <id>`, `pending approve <id>`,
   `pending discard <id>`.
4. **A toast** at logon/unlock (UNVERIFIED from an unpackaged binary, as in revision 1 §5), and a
   `[NOTICE]` to the PM lane over the broker once it is up.
5. Later, remote backends from the `user-request` crate (§8) can resolve the same pending id from a phone;
   the answer binds to a plan hash (§6.5).

The control tab shows a **summary and the exceptions** (new agents, widened modes, anything refused, "closed
without /exit"), never an unbounded list, so hundreds of waiting agents stay reviewable.

### 6.4 Nagging

`pending_nag = once-per-unlock` by default: one prompt per unlock while a pending record exists and no
answer has been given, not a prompt every few minutes. `quiet` (only the logon notice) and `every-N-minutes`
are the other values. **R2-Q7.**

### 6.5 Staleness: agents that still exist, not a clock

He asked what staleness actually matters. The answer: **a clock is not it.** What matters is whether the
agents in the plan are still the agents that should start. So the stored pending record is only an *owed
restore* notice; **the plan that is consented to is rebuilt from the current registry at pickup**, shown as
a diff against what was stored, and frozen at consent (`DESIGN-REVISION-1.md` §4.2: executed exactly,
refused if a listed agent's launch fields changed after consent). At rebuild, each entry is checked on
exactly these facts and dropped, with its reason, if any fails:

1. its record still exists and `intent` is still `wanted` (someone parked it meanwhile, or he did);
2. it is **not already running** (pid + start time identity): he or another lane started it by hand;
3. its launch `cwd` still exists (a worktree may be gone, and agentlife reports, never creates);
4. its launch fields are unchanged since the plan was stored (else it is re-listed as *changed*, needing
   a fresh decision);
5. the hard rules still pass (`DESIGN.md` §4.1 / revision 1 §4.3 item 4).

New candidates discovered at pickup (an agent that was running at a *second* shutdown) are **added to the
rebuilt plan and marked new**, never silently carried inside an old approval. A newer boot supersedes an
older pending record (marked `superseded`, kept for audit); nothing is lost because candidates are always
derived from the registry. A pending record whose every entry fails closes itself ("nothing left to
restore"). **A cancelled or failed Hello is not a denial**: the record stays pending. An explicit
`pending discard` defers those entries for this boot (R2-Q8 asks what he wants discard to mean).

---

## 7. Approvals with a duration

### 7.1 Two kinds of consent, kept distinct

- **Plan consent** (revision 1 §4.2): "start *these* agents now". Per plan, bound to its hash. Durable (§6).
- **Standing grant** (new, this revision): "agent X may be launched at up to mode M with these flags,
  without asking, until T". This is what his "an hour / a day / until a date / forever" is about, and it
  is what lets a restore run unattended for covered agents.

A grant covers **named agents** (resolved to `agent_id`s at grant time and shown in the prompt text), a
`max_mode`, a flag allowlist, and `covers`: `launch` and/or `unattended_restore`. **No wildcard and no
"future agents"**: a grant to "everything" would be the signed roster of revision 0 by another name.
A new agent always needs plan consent or an explicit grant. **R2-Q4** asks whether `unattended_restore`
should be a separate yes or implied by any launch grant.

### 7.2 Hard rules still apply under a grant

A grant widens what a person has seen; it does not repeal `bypassPermissions only for the PM`
(CireSnave's standing rule, `CLAUDE.md` §9 of the portfolio). Whether an explicit Hello grant may
override that rule for another lane was **never answered** (revision 1 Q6; his *"forever"* example was about
launch-permission bypass, but not whether it is limited to the PM). **R2-Q2.** Until then: refused.

### 7.3 Who chooses the duration, and how

Windows Hello's `UserConsentVerifier` shows a message and verifies; it has **no input fields**. So the
approver picks the duration in the **control tab / CLI**, and Hello then verifies *that exact choice*
(the prompt text states it: *"Allow `pm` to launch with bypassPermissions — FOREVER"*). The choices:

| choice | expiry stored |
|---|---|
| **1 hour** | `granted_at + 1h` (UTC) |
| **1 day** | `granted_at + 24h` (not "until midnight"; this is deliberately *not* `with-secret`'s rule) |
| **until a date/time** | the entered local time, stored in UTC and shown back in local time |
| **forever** | no expiry; displayed as `FOREVER` in every listing |

The preselected choice is `approval_default_duration` (`1d`); `forever` is never preselected and is
confirmed by name in the prompt text. **Per OverMind's approved `user-request` plan (relayed by the PM,
2026-10-04): the duration is chosen *before* the Hello prompt, which then names the chosen grant, and
`forever` and long grants need a typed confirmation.** What counts as "long" is OverMind's to define; this
design only consumes it. **R2-Q5** asks whether `forever` should be re-confirmed
periodically or capped for non-PM agents.

**Secrets are unchanged.** `with-secret`'s same-day-per-secret ruling (`WITH-SECRET-DESIGN.md` §1: *"Approve
once per lane per secret with a timeout so that an approval now isn't still valid tomorrow"*;
`approval.rs::expiry`) **stays the default for secrets**, in its own store. The two share the
`user-request` crate (§8) but **not** their duration policy; each consumer applies its own.

### 7.4 Revocation, listing, audit

- `agentlife approvals list [--all]`: every grant with agents, mode, flags, `covers`, granted-by,
  granted-at, **expiry or FOREVER**, state (`active`/`expired`/`revoked`), last-used.
- `agentlife approvals show <id>`; `agentlife approvals revoke <id>`.
- **Revocation takes effect at the next launch decision.** It cannot un-launch a running agent;
  `revoke --stop-running` additionally runs `park`-style stop on agents started under it. Revoking is
  *narrowing*, so it needs no consent and any caller may do it, **logged with who** (a lane revoking
  another's grant is an annoyance, not an escalation, and the journal shows it).
- **Audit log** (the journal): `granted` (who, via which backend, the plan or request text, the duration),
  `used` (which restore, which plan hash, which agents), `revoked`, `expired`. Append-only.
- **Integrity** (authority, unlike registry facts): each grant is **HMAC-signed with a DPAPI-protected
  key**, exactly the `with-secret` `ApprovalCache` pattern (`approval.rs`): a grant that fails its MAC,
  or is edited, is **ignored and journaled as rejected**, and the cost of a bad file is one more prompt,
  never a wrongly honoured launch. **Stated limit, unchanged:** any same-user process can read the DPAPI
  key and forge a grant (`with-secret` design §3); the claim is "a casually misbehaving lane cannot
  create one", not more.

### 7.5 Validity at use time

A grant is valid iff its MAC verifies, it is not revoked, `now` is before its expiry (or it is `FOREVER`),
the agent's current launch fields are **within** `max_mode` and the flag allowlist, and the agent still
has the identity it had at grant time. A changed `cwd`, a higher mode or a new flag **voids coverage for
that agent** and puts it back to needing consent, so a grant cannot be ridden to a launch the approver
never saw.

---

## 8. The `user-request` crate: what agentlife needs from it

OverMind plans it; agentlife consumes it. It replaces the narrower `hello-consent` crate OverMind first
proposed (that code becomes the Windows backend). Requirements derived from §6–§7, stated as needs, not as
an API mandate:

1. **Durable requests.** `submit(Request) -> RequestId` persists the request; the crate never requires the
   caller to stay alive. A request outlives the process, the reboot and the day.
2. **No built-in timeout** (a caller may *poll with a bound*, but the request itself does not expire
   because a clock ran out). Expiry, if wanted, is the caller's policy.
3. **Choices with a bound answer.** A request carries `options` (e.g. the duration list) and a **plan/hash
   binding** (OverMind's plan calls it `bound_hash`, checked by `answer()` against the artifact, which for
   agentlife is the frozen plan hash); the answer returns `{choice, bound_hash}` and is **single-use**.
4. **Backends are interchangeable and have different presence requirements.** Windows Hello is
   *present-only* (collects an answer only when a person is at the desktop); SMS/push are *remote*. The
   interface must say which, so `agentlife` can notify through one and collect through another.
5. **Notify and collect are separate steps** (`notify(&Request)`, `collect(&Request) -> Option<Answer>`),
   because "tell him" and "get his answer" happen at different times (§6).
6. **The consumer supplies the text.** The crate shows what it is given; it does not summarize. A prompt
   whose text can drift from the plan it binds is the TOCTOU the frozen plan exists to close.
7. **Distinguish cancel from deny.** `NoAnswer`/`Cancelled` leaves the request pending; only an explicit
   `Denied` closes it.

Open for OverMind, not for me: where it lives, and whether `with-secret` migrates to it (R2-Q9 below only
asks CireSnave whether he cares).

---

## 9. Hook ownership: R2, with an install and test requirement

R2 (separate `agentlife hook`, `SessionStart` and `SessionEnd` only; the per-tool-call hook untouched) is
the PM's judgement and I agree. The PM installs the `settings.json` entries **when I have a tested
binary**; this section is what "tested" and "install" mean.

### 9.1 The entries

Shell form (one command string, **no `args` key**: OverMind spec §10.4 found Claude Code does not deliver
`args` to a command hook), appended to each event's array and never replacing `hooks` (the existing
`run_wrap_hidden.vbs` entry stays):

```json
"SessionStart": [ { "hooks": [{ "type": "command",
    "command": "C:/Projects/.claude-hooks/agentlife.exe hook SessionStart" }] } ],
"SessionEnd": [
  { "matcher": "prompt_input_exit", "hooks": [{ "type": "command",
    "command": "C:/Projects/.claude-hooks/agentlife.exe hook SessionEnd --reason prompt_input_exit" }] },
  { "matcher": "logout", "hooks": [{ "type": "command",
    "command": "C:/Projects/.claude-hooks/agentlife.exe hook SessionEnd --reason logout" }] },
  { "matcher": "other", "hooks": [{ "type": "command",
    "command": "C:/Projects/.claude-hooks/agentlife.exe hook SessionEnd --reason other" }] },
  { "matcher": "clear", "hooks": [{ "type": "command",
    "command": "C:/Projects/.claude-hooks/agentlife.exe hook SessionEnd --reason clear" }] },
  { "matcher": "resume", "hooks": [{ "type": "command",
    "command": "C:/Projects/.claude-hooks/agentlife.exe hook SessionEnd --reason resume" }] }
]
```

Using one entry per documented matcher passes the **reason** on the command line, so the design does not
depend on the payload carrying a `reason` field (UNVERIFIED: OverMind's `HookInput`, `lane_state_writer.rs:60–70`,
has none, and the docs fetch did not show the SessionEnd input schema). Rollback removes only entries whose
command is `agentlife.exe`.

### 9.2 Constraints the docs impose

- **`SessionEnd` has a shared 1.5 s budget** (hooks docs, fetched 2026-10-04; raisable per hook up to 60 s).
  The end hook does one locked append and exits: no network, no process-table walk beyond the pid it
  needs, no waiting. A slow end hook is a lost record, which is the failure this whole design guards
  against.
- **`SessionStart` also fires for `compact`, `clear`, `resume`, `fork`.** `compact` and `clear` are the same
  *process*; the hook groups sessions by `(pid, process start time)` and treats those as a continuation, not
  a new launch (this is the cwd-drift behaviour OverMind spec §2 documents for compaction).
- **The hook is dumb.** It records facts and exits 0; it never decides intent (§3.4) and never fails a
  session: a non-zero exit is non-blocking but silent, so failures go to `hook-errors.log` (the lesson of
  `.lane-state/hook-errors.log`).

### 9.3 "Tested binary" means

Per OverMind spec §10.3 ("the acceptance check is a real state WRITE, not the diagnostic alone"): real hook
JSON on stdin → a real registry record on disk → read back by `list`; then the §11.6 sequence (diagnostic on
**one** lane, observe a few real turns, only then widen). Install by rename-not-overwrite (spec §11.2): the
hook binary is in use by every session. The PM installs; a lane does not.

---

## 10. Reused / new / extracted (updated from revision 1 §7)

| piece | status | note |
|---|---|---|
| hook machinery: lock, atomic write, pid/cmdline/launch-cwd/flag parsing | **reused**, library | `lane_state_writer.rs`; OverMind will make `recorded_cwd`, `write_atomic` `pub`, record `name`, and record the process **start time** beside the pid |
| lane-restart state-file naming bug (role from event cwd, `:557`/`:609`) | **OverMind fixes** (their reply, relayed by the PM) | agentlife's registry does not depend on it (keys by session, joins by `session_id`) |
| `.lane-state` files, `restart.log` | **read-only**, joined by `session_id` | unchanged |
| ConPTY host, approvals fetch, startup-dialog keystroke | **reused** | launch through `lane-restart host` |
| process identity, `kill_verified`, `find_claude_process_in` | **reused** | `facts.rs` |
| Hello / SMS / push consent | **consumed** from the **`user-request` crate** | §8; replaces `hello-consent` |
| `valid_identifier`, argv allowlist, `;` guard, env strip, `spawn_relaunch`, liveness | **extracted from OverMind** when asked | `OVERMIND-EXTRACTION-NOTE.md`; v0.x may carry a drift-tested copy |
| registry (agents + monthly journal), `agentlife hook`, `agent_id` env carry | **new** | revision 1 §2 |
| intent model, `park`/`stop`/`unpark`, the closed-on-purpose rule (§3) | **new** | this revision |
| config file + flags with precedence (§5) | **new** | this revision |
| durable pending restore, pickup triggers, rebuild-at-pickup (§6) | **new** | this revision |
| standing grants: durations, revocation, audit, HMAC (§7) | **new**; HMAC/DPAPI pattern **reused** from `with-secret` `approval.rs` | this revision |
| shutdown-start inference from the System log | **new** | revision 1 §2.6 |
| plan builder: order, memory floor, `max_running`, windows, freeze + hash | **new** | revision 1 §3, §6 |
| control tab, `pending`, `approvals`, `list` | **new** | §4, §6, §7 |

---

## 11. Scale checklist (no hard-coded ceilings)

Every limit in this design is one of: (a) a **configurable default** (§5); (b) a **relative check** against
the machine (free-RAM floor); or (c) a **bound on a wait**, not on a count (batch settle time). There is no
constant maximum number of agents, windows, grants, pending records or journal entries. The things that
are *not* user-visible constants are performance properties: per-agent files, monthly journal segments,
summaries-not-lists in the control tab. At hundreds of agents the practical limit is the machine's memory
for local client processes (≈ 0.65 GB each, measured 2026-10-03), which the floor and parking manage and a
bigger machine raises.

---

## 12. Test additions (on top of `DESIGN.md` §9 and revision 1 §10)

- **Intent rule table (§3):** every row against all three shutdown-marker sources: `/exit` before
  shutdown; `/exit` after shutdown began; `other`; `clear`/`resume` (process continues); superseded by
  `lane-restart --self`; killed with no `SessionEnd`; crash with no shutdown; `park` by person / PM / self
  / other lane; hand re-launch clears `closed`. Each asserted **by agent name, not by count**.
- **Canary extension (UNVERIFIED items §3.3):** for a stand-in and then one real canary, record whether
  `SessionEnd` fires and with which matcher for `/exit`, terminal-window close, `taskkill`, and a real
  `shutdown /r`, and whether `SessionEnd` completes inside the 1.5 s budget.
- **Pending restore durability:** create a pending record, **kill the process, reboot the test host's
  state directory**, run `pending`, assert it is intact; approve it "days later" by faking only the clock
  (injected), and assert the plan is rebuilt, entries that no longer exist are dropped with reasons, and a
  newly running agent is not double-started.
- **Plan binding:** change a launch field between consent and execution; assert refusal for that agent,
  start for the rest.
- **Grants:** each duration; expiry at the boundary; revoke; `revoke --stop-running`; MAC tamper;
  field change voiding coverage; the `forever` display; `with-secret`'s same-day rule unaffected.
- **Config precedence:** flag over env over file over default; unknown key refuses; floor relative to an
  injected free-RAM function at 8 / 64 / 1024 GB.
- **Scale sim:** 1,000 registry records with a stand-in launcher: floor and `max_running` honoured,
  windows assigned at `tabs_per_window`, control-tab output bounded, journal segment boundary crossed.
- **Hook test (§9.3):** real JSON in, real file out, read back; a 1.5 s budget test with a slow disk
  stand-in; a `compact`/`clear` sequence from the same pid yields one launch, not three.
- **Unlock trigger:** create the Task Scheduler session-unlock trigger from a script and confirm it fires on
  a real lock/unlock (a manual acceptance step, logged in `docs/ACCEPTANCE.md`).

---

## 13. Still UNVERIFIED (carried into the real test)

1. Whether `SessionEnd` fires on terminal-window close, crash, or Windows shutdown, and its payload shape
   (docs: scenarios not listed; no `reason` field in OverMind's `HookInput`).
2. Whether a Task Scheduler **session-unlock** trigger can be created without elevation and fires reliably.
3. Whether a new environment variable survives `wt.exe`, and `wt -w <name>` addressing (revision 1 §10).
4. Windows toast from an unpackaged Rust binary.
5. That `agentlife`'s notion of "mode higher than" matches Claude Code's real ordering (`DESIGN.md` §4.1).

---

## 14. Numbered questions still ambiguous (for CireSnave, via the PM)

- **R2-Q1. Parked vs stopped.** One "closed" state with two labels (`parked` never pruned and prominently
  listed; `exited` pruned after 14 days), as proposed? Or do you want them to behave identically?
- **R2-Q2. Does the "only the PM may bypass permissions" rule still hold when you personally grant bypass
  to another lane with Hello?** Your *"forever"* example was about launch-permission bypass. Is that grant
  for the PM only, or could you grant it to any lane?
- **R2-Q3. Closed with the window's X, not `/exit`.** If the hook cannot tell that from a crash, should
  such agents be restored (flagged) or left closed (flagged) by default? Proposed: left unselected in the
  review.
- **R2-Q4. What a launch grant covers.** Only "may start at mode M", or also "may be restarted
  *unattended* after an update"? Proposed: two separate boxes, both off until chosen.
- **R2-Q5. `forever`.** Should it need periodic re-confirmation, or a cap for anyone but the PM? What
  duration should be preselected (proposed 1 day)?
- **R2-Q6. Who may park or stop another lane.** Proposed: you any time; the PM only when the lane is
  idle; a lane only itself; any other agent needs your consent. Right?
- **R2-Q7. Re-notification.** Once per unlock, once per logon only, or every N minutes while a restore
  is pending?
- **R2-Q8. "Not now" vs "never".** In a pending restore, an agent you unselect is *deferred* (stays a
  candidate, asked again at the next logon/unlock). Should `discard` instead defer until the next *boot*,
  park, or stop asking about that agent?
- **R2-Q9. `user-request` ownership.** You said you have no preference where it lives; do you want
  `with-secret` moved onto it as well (one consent code path), accepting that its secrets rule
  (same-day) stays a consumer-side policy?
- **R2-Q10. Timing of the first install.** The PM installs the hook entries once I have a tested binary
  (§9.3). Is "one lane first, then widen" (`OverMind` §11.6) acceptable for the agentlife hook too?
