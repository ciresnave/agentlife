# Consent and the durable pending restore (M4, first part)

Status at this change: the **trait, the fake, the pending record and `agentlife pending`** are built and
tested. **No real consent backend is wired**, so nothing can be asked or approved, and `agentlife restore`
without `--dry-run` still refuses. Wiring is one function, `consent::installed()`.

## Why a trait

OverMind's `user-request` crate (0.8.0, OverMind#125, merged 2026-10-08T15:28Z) is **not on crates.io**
(`cargo info user-request` and `overmind-user-request`: "could not find in registry", measured 2026-10-08
about 15:30Z). CireSnave's Sources rule forbids a path or new `git =` dependency, and the PM refused an
exception (2026-10-08). Ruling: build the non-consent parts behind a trait shaped like the crate's pending
API, test against a fake that enforces its semantics, wire the real crate once it is published (it moves
homes under board 143). Nothing is copied from OverMind; the shape was read from its README and
`store/pending.rs` at `main`.

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
4. **Not built yet:** the logon task and unlock trigger, network wait, the control tab, superseding an older
   pending record by a newer boot's, and the real execution after approval (`restore::execute` exists and is
   not called).

## Proposal: the `RestorePlan` kind for `user-request` (for CireSnave's ruling)

A new kind and its maximum grant are a security parameter on the Hello prompt, so this is a proposal, not
a decision. Nothing is wired until the kind exists in OverMind's `KindId`.

**What an approval covers.** Starting exactly the agents in one frozen plan, identified by the plan's hash
(`bound_hash`): each by id and name, its working directory and its rebuilt launch arguments including the
permission mode. It starts new `claude` processes through `lane-restart host`, in batches. It does **not**
cover: any other plan (a changed plan voids the request, unasked), a mode wider than the plan lists
(`bypassPermissions` stays PM-only and refused elsewhere), stopping or parking anything, or later wakes.
Standing permission to launch (an hour, a day, forever) is the separate M5 grant and is not this kind.

**Who asks.** Role `agentlife`, taken from the process table by the crate, run by the logon task, the unlock
trigger or a person at a terminal with no `claude` ancestor. Restore is not meant to be run by a lane; agentlife's own refusal of an agent caller for `restore` is not built yet.

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
