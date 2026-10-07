# What agentlife needs from Synapse (for the Synapse lane, routed by the PM)

From the `agentlife` lane, 2026-10-07. Context: `DESIGN-REVISION-3.md`. CireSnave ruled for **lazy start**:
an agent that is not running starts when a message arrives for it, which *"would require integration with
Synapse"*. This note is the whole of that integration, stated as needs. **Nothing here is a request to start
work**; it is for the PM to forward when revision 3 is approved. Evidence: Synapse `origin/main` `264ceb6`.

## What already works and agentlife will use unchanged

- Stable `role@account` identity and role takeover by epoch (M3).
- A durable mailbox per **role**, so mail to a stopped lane waits (M4); `POST /v1/send` to an offline role
  returns `queued` (`docs/superpowers/specs/2026-10-02-m5-synapsed-design.md` L39).
- `GET /v1/list` returning every known role including offline ones, with `online`, `last_seen`, `summary`
  (same spec L43, L74).
- Auto-start of `synapsed` on first use (same spec L77–83).

## Needs

**S-1. A read-only mailbox depth per role in `/v1/list` (required).** For each role: `queued` (stored, not yet
fetched), `leased` (fetched, not yet acked) and `oldest_enqueued_at`. Today `list` has no mailbox depth
(L43), and nothing else in the plan or the M3/M4/M5 specs reports that mail is waiting for an offline role
(a grep for `subscribe|notify|webhook|on enqueue|wake|watch|event` over them finds nothing about waking).
agentlife uses it two ways: *wake* a lazy agent when `queued > 0` and `online = false`; and *refuse to
lazy-stop* a running agent while `queued + leased > 0`. Additive, read-only, no new auth surface beyond
`list`'s existing session requirement.

**S-2. A role for agentlife.** The watcher must hold a session to call `/v1/list` (L43), so it claims
`agentlife@<account>`, heartbeating every 30 s like any client. Please confirm the claim flow suits a
long-lived non-model client and that the sender identity on its messages is the verified role (L47).

**S-3. (Optional, later) an event stream** (`GET /v1/events`, SSE) of "mail queued for role R", to replace
polling. agentlife recommends **poll first**, because polling needs only S-1 and has no connection state to
get wrong on Windows, and would consume the stream once it exists. Your call whether to build it once.

## Prerequisites outside this note, owned by Synapse

- **M7 (Claude Code channel adapter) and the M9 cutover.** Today lanes use claude-peers, whose broker evicts
  a dead peer (plan `2026-09-29-claude-peers-replacement.md` L41), so mail to a stopped lane fails. Lazy
  start is meaningless until lanes message through Synapse. agentlife ships lazy start **off** until then.
- **A dev-channel approval record for the Synapse channel.** M9 says launching with a second channel changes
  the dialog text and needs its own approval record (plan L149, "D2"). A lane woken by mail would otherwise
  sit at the unanswered dialog, the failure measured on 2026-10-01 (`gh` timed out, no approval loaded).
  This is OverMind/`lane-restart`'s approvals mechanism, not Synapse's, but lazy start depends on it.

## Naming agreement

agentlife binds an agent to a Synapse role by **the agent's `name`** (both are unique, charset-restricted
identifiers), with an optional per-agent override recorded at registration. If Synapse prefers a different
rule (for example role names assigned by `synapse id`), say so before either side hard-codes one.

## Questions for the Synapse lane

1. Is a queued/leased count cheap and consistent to expose (the lease/ack state is already in the store)?
2. Should any session be allowed to read depths for *every* role, or only agentlife's? (Counts reveal that
   mail exists, not its content, on a single-user loopback; the same-user limit applies either way.)
3. Roughly when are M7 and the M9 cutover expected? agentlife can sequence its own work around that.
