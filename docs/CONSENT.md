# Consent and the durable pending restore (M4, first part)

Status (0.12.2): the **trait, the fake, the pending record, `agentlife pending`, the real backend**
(`consent::real::UserRequestBackend` over `user-request` 0.11.1) **and running an approved restore**
(`src/run.rs`) are built and tested. `consent::installed()` returns the real backend, and
`consent::real::HelloPrompt` is wired to `restore`, `pending approve` and `pending --prompt`. See
"Running an approved restore" below. Still **not built**: the control tab, the logon task's registration
and any real run against real lanes (the PM's canary).

## Why a trait

OverMind's `user-request` crate was **not on crates.io** when this was built (2026-10-08;
`cargo info user-request` and `overmind-user-request`: "could not find in registry"). CireSnave's Sources
rule forbids a path or new `git =` dependency, and the PM refused an exception. Ruling: build the
non-consent parts behind a trait shaped like the crate's pending API, test against a fake that enforces its
semantics, wire the real crate once it is published. It is published (0.11.1, crates.io, rust 1.95) and is
wired (0.12.1, next section); the trait stays so the rest of agentlife is tested without a Hello store.
Nothing is copied from OverMind.

## The real backend (`src/consent/real.rs`, 0.12.1)

Rulings from OverMind `origin/main@04aa5d2`, relayed by the PM 2026-10-10:

* **Not a registered lane.** The requester is role `agentlife`, empty `session_id`, this process's pid and
  start time, `managed: false` (`real::this_process`); the prompt reads "agentlife (pid N) - NOT a registered
  lane". The binding is the plan hash in the subject (`restore_plan_subject`), and the kind is
  `Scope::AnyRequester` and `MaxGrant::OneUse`: who asks is shown, not matched on. The fake does the same.
* **The shared store.** `locate::dir()`, `locate::head_copy()` and `locate::PROTECTOR` (DPAPI): never a path of
  ours, so `user-request list` / `revoke --all` see and stop agentlife's approvals. The store is opened **per
  call** and dropped at its end, so `store.lock` is never held while a prompt is up or a restore runs. Reads
  use `Store::inspect` and a change to something that must exist uses `open_existing`, so asking never
  creates a store; only `submit` does. `USER_REQUEST_DIR` / `USER_REQUEST_HEAD` work in debug builds only, so
  tests set them (one test, under a lock, for the production constructor; the rest use a temp store with a
  test key protector so both CI legs run them).
* **Windows only, closed elsewhere.** The crate's `Dpapi` refuses off Windows and `HelloConsent` says Hello
  exists only on Windows, so on Linux every call fails closed: nothing can be recorded, approved or spent.
* **A channel answer that is not an answer leaves the request pending.** `Outcome::Refused` and
  `Unavailable` map to `Outcome::Unavailable` (with the reason); `Denied` is a cancel and closes it.

Mapping of an error to `AnswerError` reads what became of the record (gone means voided and **nobody was
asked**), not the store's prose; the prose only picks the kind of void.

## The contract (`src/consent.rs`)

| call | meaning |
|---|---|
| `submit(request, grant, bound_hash) -> id` | records a request that holds no privilege and has no timeout. The same request and hash returns the same id |
| `begin_answer(id, bound_hash_now)` | a different hash, an altered record or an ended grant **voids** the request and nobody is asked; otherwise reserves a **new** prompt |
| `resolve(asking, outcome)` | `Approved` spends the request. **`Cancelled` closes it** (CireSnave: "cancel refuses"). `TimedOut` and `Unavailable` leave it pending; `TimedOut` starts the 10-minute cooldown |
| `withdraw(id)` | the requester gives up; a prompt already up can no longer be approved |

A restored request **re-prompts and never approves by itself**; only one prompt per request at a time.

`consent::fake::FakeConsent` enforces all of that (tests only prove agentlife's use of the contract; they do
not prove the real crate).

## The pending record (`src/pending.rs`)

`<home>/pending/<id>.json` says what is owed: the frozen plan's file (`<home>/plans/`), its hash, the agents,
the reason, and `closed` (never deleted, so the trail stays). It is **state, not a process**.

* `create` freezes the plan and submits a request bound to **the plan hash**. Asking again about the same
  plan returns the same record. A plan that starts nothing, or whose hash does not match its content, is
  refused before anything is stored.
* `answer(rebuilt)`: the caller passes the plan rebuilt from the registry **now**. Its hash goes to
  `begin_answer`. If it differs the request is voided, the person is never asked, and the result names the
  agents that differ; the caller makes a new request from the rebuilt plan. This is DESIGN-REVISION-2 §6.5:
  staleness is "not the same agents", not a clock. An approval returns the **frozen** plan, whose hash is by
  then proven equal to the one held now.
* A frozen plan whose content was edited is refused, withdrawn and closed (`load_frozen` checks its hash).
* `discard` withdraws first and closes the record only after that succeeded.

## `agentlife pending`

`list [--json] [--all]` and `show <id>` only read. `approve` and `discard` need the backend and, without it,
say so and change nothing.

## Open questions (not decided here)

1. **A kind for plan consent.** `user-request`'s `KindId` is a closed set compiled into that crate (`Secret`,
   `LaneDialogBypass`). agentlife needs a `RestorePlan` kind with a stated maximum grant. Needs OverMind and
   the PM; it changes what the crate publishes.
2. **What a grant means for a plan.** Plan consent is "start these agents now". See "Proposal" below; the 24 h first assumed was withdrawn (PM, 2026-10-08).
3. **The prompt text** for a plan (three lines: who, what, how long) belongs to the channel; agentlife
   supplies `subject` ("restore N agents") and `summary` only.
4. **Superseding (built):** creating a pending record for a different plan withdraws every older open record
   and closes it `superseded` (kept for audit; one that cannot be withdrawn stays open and is voided unasked
   when answered, since the hashes differ). "Newer boot" is approximated by "newer plan": records carry no boot id.
   **Not built yet:** network wait, the control tab, and the real execution after approval (`restore::execute` exists and is
   not called).

## Running an approved restore (`src/run.rs`, 0.12.2)

One function owns the order, for every command that can start agents (`restore`, `pending approve`,
`pending --prompt`):

1. **The caller must be a person** (R10): `Caller::Agent` and `Caller::Unclear` are refused before the
   store is touched. A person at a terminal and the logon task (no `claude` above it) pass. This is a
   convenience, not a boundary (`caller.rs`): a lane that detaches from its `claude` ancestor passes it;
   what protects the machine is the Hello prompt a person must answer.
2. One restore at a time (`restore.lock`), held until the report is written.
3. The agent list is **printed**, then the person is asked (`pending::answer`).
4. After an approval, the plan is **rebuilt again** from the registry and the process table; if its hash is
   not the approved one, **nothing is spent and nothing starts** (the approval stays unspent and goes stale).
5. The approval is **spent** (`pending::spend_approval`, which also refuses one older than 5 minutes: that
   one *is* spent), and only on `Ok` the **frozen plan that was approved** is executed.
6. The report goes to `<home>/reports`; the exit code is 0 only if every planned agent came up or was
   already running.

`pending approve` and `pending --prompt` run the plan on approval at once: an approval is good for 5
minutes, so approving without running would only waste it. They rebuild the plan with the defaults, so a
request made with `restore --only` or `--priority` is voided unasked there (the differing agents are
named); run `restore` again with the same flags.

**What a person sees is not the consent.** The terminal list is printed by agentlife; the Hello prompt
itself is the crate's three lines and names **only the plan hash** (`Wants: restore lanes: plan <hash>`).
A restore started by the **logon task** therefore shows the person **no agent list** at the moment they
answer: they approve a hash. Until `user-request` puts the summary in its own prompt (asked of OverMind,
2026-10-10), that is the honest state, and a person at a terminal is the only one who sees the list first.

## One-shot wiring (the `RestorePlan` kind merged: OverMind#131, `user-request` 0.10.0)

CireSnave ruled the proposal below one-shot (*"One-shot."*). The kind exists; agentlife is wired to its
contract behind the `Consent` trait (at that change the crate was not yet on crates.io; see "The real backend").

* **Grant:** `Grant::OneUse`: no duration, never `Forever`. The request is bound to ONE frozen plan
  (`subject` "plan <hash>", `bound_hash` = the plan hash); a changed plan voids it unasked; a request
  restored after a restart re-prompts; Cancel closes it.
* **Spend before running:** `pending::spend_approval` calls `Consent::approved_at` then
  `Consent::spend_one_use(kind, subject, requester)` (the crate's `Store::find` and
  `Store::spend_one_use`, 0.11.1) and returns the frozen plan **only on `Ok`**. The restore must run only
  on that `Ok`; `Err` means nothing was spent. The approval is found by what it covers (`RestorePlan`,
  subject `plan <hash>` built by `consent::restore_plan_subject`, never by hand). **It is not narrowed by
  who asks** (`Scope::AnyRequester`: a restore after a reboot is a new process, and that is the case the
  kind exists for): an earlier version of this document, and the fake, said another requester's role finds
  nothing; the real store does not do that, and that was corrected in 0.12.1. The plan hash is the binding.
  The caller supplies the `Requester` (no process-table constructor exists in 0.11.1); agentlife's is role
  `agentlife`, empty `session_id`, its own pid and start time, `managed: false`. A second approval or request
  for a plan with an unspent approval is refused by the store.
* **Freshness is agentlife's:** the crate never expires an unspent approval, so `spend_approval` refuses an
  approval whose `approved_at` is **older than 5 minutes** (`pending::APPROVAL_FRESH_SECS`; PM ruling
  2026-10-08). The refused approval is spent all the same: ask again. An `approved_at` more than 60 s in the
  future is refused too. Exactly 5 minutes is accepted.
* **`HelloPrompt` must stay the ONLY production `Prompt`.** agentlife fabricates the `Approval` from the
  `Outcome` a `Prompt` returns, so any `Prompt` that returns `Approved` grants. Never add another in
  production code.
* **Registered lane:** the prompt reads "agentlife (pid N) - NOT a registered lane" unless OverMind's
  registry lists agentlife as a lane. That registration is outside this repo (asked of the PM).
* The 30-minute window below is superseded; the rest of that section (what an approval covers, who asks)
  stands.

## Proposal: the `RestorePlan` kind for `user-request` (for CireSnave's ruling)

A new kind and its maximum grant are a security parameter on the Hello prompt, so this is a proposal, not
a decision. Nothing is wired until the kind exists in OverMind's `KindId`.

**What an approval covers.** Starting exactly the agents in one frozen plan, identified by the plan's hash
(`bound_hash`): each by id and name, its working directory and its rebuilt launch arguments including the
permission mode. It starts new `claude` processes through `lane-restart host`, in batches. It does **not**
cover: any other plan (a changed plan voids the request, unasked), a mode wider than the plan lists
(`bypassPermissions` stays PM-only and refused elsewhere), stopping or parking anything, or later wakes.
Standing permission to launch (an hour, a day, forever) is the separate M5 grant and is not this kind.

**Who asks.** Role `agentlife`, supplied by agentlife (the crate has no process-table constructor), run by the logon task, the unlock
trigger or a person at a terminal with no `claude` ancestor. Restore is not meant to be run by a lane; agentlife refuses an agent caller for `restore`, `pending approve` and `pending --prompt` (0.12.2, `run::require_person`).

**Shortest window that works.** One-shot is enough. The requester executes in the same process right after
the approval and never consults the grant again; a crash mid-run needs a fresh approval, which is the safe
direction. So the preferred shape is a kind whose approval is **spent on use and is not a standing grant**
(the pending request is already single-use in 0.8.0). If the crate requires a duration, ask for the
**shortest that covers the hand-off from answer to use: a maximum of 30 minutes**, requested as 30
minutes. Never `Forever`, and no day-long default. (No midnight cutoff: a relative duration is the whole
rule, as `DESIGN-REVISION-2.md` chose for its "1 day" row, "not until midnight".)

**Why not longer.** A longer window would be a standing launch grant under another name, valid for plans
the person never saw. The plan hash already binds the approval to what was shown; a short window bounds
how long that approval can be used. **What the window governs.** Only the approval step: the time between
the person's answer and the requester spending it. `restore::execute` does not consult the grant during the
batches, so a restore that outlasts the window is **not** interrupted. **Why not shorter.** Not because a
long run would stop half-way (it does not), but because the requester may be slow to pick the answer up
(a busy machine, a retry); a window of a few minutes would void a good approval for no safety gain.
