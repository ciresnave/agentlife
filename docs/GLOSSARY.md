# Glossary

> **claude-peers is being retired (CireSnave 2026-10-31: "Get claude-peers out of our workflow and get every lane on Synapse.").** This document is kept as written; where it says claude-peers, broker or peer id, read it as the state when it was written. The live code that still speaks to the broker stays until every lane answers a PING on Synapse (PM ruling 2026-10-31), then goes in its own PR. Current launch form: `--mcp-config <role>.mcp.json --dangerously-load-development-channels server:synapse` (docs/SYNAPSE-ONLY-CUTOVER.md).

The terms this repository's docs and CLI use, each defined once. Where a term has a design page, it is
named; nothing here overrides that page.

**Agent.** A Claude Code session the registry tracks, found by an id or a name. Its last session carries
a process's pid and start time. (`DESIGN-REVISION-1.md`)

**Approval.** A person's answer to a restore, given through Windows Hello. It is one-use and good for a
short window; spending it starts the frozen plan. (`CONSENT.md`)

**Batch.** A group of agents `restore` starts together, with a delay before the next; the PM goes first and
alone. (`DESIGN.md` 2.2)

**Broker.** The claude-peers message broker on loopback (`127.0.0.1:7899`). `agentlife` reads its peer
list and sends it a wrap-up request; nothing here ever talks to it from outside the machine.

**Closed on purpose.** The verdict `reconcile` gives an agent whose session a person ended before any
shutdown, as opposed to one that was killed.

**Consent.** The contract a restore must pass before anything starts: submit a request, ask the person,
spend the approval. It is a trait with a real backend (OverMind's `user-request`) and a fake for tests.
(`CONSENT.md`)

**Frozen plan.** The restore plan as it was when the request was made, saved under `<home>/plans/` and
named by its hash. An approval returns the frozen plan, never a rebuilt one.

**Grant.** The limit an approval carries: how many times it may be spent (once) and for how long.

**HANDOFF.** The file a lane writes in its launch directory to say what the next session needs. A graceful
stop waits for one written after the request. (`GRACEFUL-STOP.md`)

**Hook.** A Claude Code `SessionStart` or `SessionEnd` entry that runs `agentlife hook`, records the
session in the registry, and never fails the session.

**Host.** The `lane-restart` process that relaunches a lane's tab; a relaunched lane's `claude` is its
child.

**Journal.** The append-only record of what `agentlife` did to an agent (`down-requested`, `closed`,
`stopped`, `close-failed`).

**Lane.** One long-lived agent working on one project, started in that project's directory.

**Lane state.** OverMind's per-role record, `.lane-state/<role>.json`, written by its hooks: the pid, the
cwd and whether the lane is busy. `import-lane-state` seeds the registry from it, read-only.

**Launch spec.** `<role>.launch.json`: what `lane-restart` needs to start a role again. A running lane is
stopped only if its spec is complete.

**Park.** Mark an agent closed and keep that forever, so `restore` leaves it down. `unpark` clears it.
(`stop` marks it too, but the mark ages out.)

**Peer.** One session registered with the broker. Its `pid` is the MCP helper process, whose parent is the
lane's `claude`; peers are joined to agents by that parent, never by cwd or id.

**Pending restore.** A restore that waits for a person: a record plus a frozen plan, not a process.
(`agentlife pending`, `CONSENT.md`)

**Pin.** Mark an agent that must never be stopped lazily; stopping it needs `--confirm` with its name. The
PM is pinned.

**PM.** The portfolio project-manager lane. It is started first, and it alone may run with
`bypassPermissions`.

**Plan hash.** A hash of the whole restore plan. The approval is bound to it, so a registry that moved in
between voids the request instead of approving a different plan.

**Process identity.** A pid together with its start time in whole seconds. A pid alone is never trusted,
because it can be reused.

**Reconcile.** Compare each agent that should be running with the process table and say why one is not.

**Registry.** The directory of agent records under `<home>`, joined with the process table by `list`.

**RestorePlan.** The kind of request `user-request` records for a restore, bound to a plan hash.
(`CONSENT.md`)

**Restore.** Start every agent that should be running and is not, after a restart or logon, in batches, and
only after consent. `--dry-run` prints the plan and starts nothing.

**Role.** A lane's explicit identifier (`[A-Za-z0-9_-]`), never derived from its directory. The key of its
lane state and launch spec.

**Roster.** The first design's hand-kept list of lanes; superseded by tracking agents dynamically.
(`ROSTER-SCHEMA.md`)

**Stale worktree.** A registered git worktree whose branch has a merged pull request at the worktree's own
HEAD, whose remote branch is gone, and whose tree is clean. `agentlife worktrees --stale` lists them and
removes nothing.

**Unmatched peer.** A broker session whose helper's parent is no role's live `claude`, listed by
`lane-restart status --peers` with its cwd.
