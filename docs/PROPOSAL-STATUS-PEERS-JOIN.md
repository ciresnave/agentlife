# Proposal: `lane-restart status --peers` (slice 3 (a)3)

**Status: built in 0.14.23** (PM rulings 2026-10-14: go; opt-in, read-only and soft-failing is enough; extend
`notify::Peer` and add `list_all` in `lane-restart/notify.rs`, no dependency onto `agentlife`; summary printed only
under `--peers`, truncated, never in the default JSON). The proposal as written follows; the measurements are at the end.
PM task 2026-10-14: write what is joined, what it reads, what it would cost, what it needs from the PM. Read at `origin/main` = `bbd842d` (0.14.20). The versions are the PM's.

## The problem, as observed

2026-10-14 ~12:00Z: `status` reported `synapse-restarttest-lane` as `Crashed` while `list_peers` showed a live
session at the same cwd. `status` knows each role's cwd, pid and activity from `.lane-state`; the broker knows
each live session's cwd and free-text summary; the PM's sweep reads both and joins them by eye. I did not
establish why that role was `Crashed` with a live session (state pid dead, session restarted without a hook
writing state, or a different `claude` at that cwd); the join below is what would make the cause readable.

## What is joined

Per role in `status`, the broker peers whose session is that role's `claude`.

**Join key: the parent process, never the cwd and never the peer id.** `src/peers.rs` already records why:
a peer's `pid` is the MCP helper, its parent is the lane's `claude` (verified on three live lanes on
2026-10-07), two live lanes can share one cwd (`C:\Projects\auth-framework`), and ids rotate on every restart.
So: `peer.pid -> parent_of -> == LaneState.pid`, and the existing liveness check (`state_process_is_live`,
pid plus start time) says whether that pid is still the same `claude`. `peers::peers_of_claude` is exactly
this predicate.

Output, one new optional field on `RoleStatus` (absent without `--peers`, so the default JSON is unchanged):

| `peers` value | meaning |
|---|---|
| `[{id, summary, last_seen}]` | live peer(s) whose helper's parent is this role's live `claude` |
| `[]` | broker answered; no peer belongs to this role's `claude` |
| `"unknown"` | broker unreachable, timed out or malformed: **never read as "no peer"** |

Plus one **unmatched** list at the report level: peers whose parent is no role's `claude` pid. That is the
`synapse-restarttest-lane` shape (a live session no state file claims), listed with its cwd so it can be
located, and with `nested_in`-style labelling only if the cwd matches a role (a label, not a join).

## What it reads

- `.lane-state/*.json` and the process table (already read by `status`).
- `POST /list-peers` on the loopback broker (`CLAUDE_PEERS_PORT`, default 7899): one request per `status`
  call, `{"scope":"machine"}`. Read-only; it sends nothing. `summary` and `last_seen` are returned by the
  broker today but `peers::Peer` deserialises only `id`, `pid`, `cwd`: adding two optional fields.
- Parent pids from the process table (`ParentProcess::parent_of`, already used by `restore` and stop).

It writes nothing and needs no PM authorisation: it is as read-only as `status` is today, and fails soft.

## Cost

- **Code: small.** Two things move: the broker client exists twice (`src/peers.rs` in `agentlife`, and
  `crates/lane-restart/src/notify.rs`, which has its own `post_json`/`dechunk`/`Peer`). `status` lives in
  `lane-restart`, so the new reader either uses `notify.rs`'s client (which lacks `pid`) or the two clients
  are unified first. Recommendation: extend `notify::Peer` with `pid`, `summary`, `last_seen` and add one
  `list_all(addr)` there, rather than a dependency from `lane-restart` onto `agentlife`. This is a decision
  for the PM only because it touches the notify path that the host depends on.
- **Runtime: one loopback request, 5 s timeout** (the existing client's), only under `--peers`.
- **Tests:** a fake broker (the `serve()` helper in `peers.rs` tests) plus `RecordingFacts`; cases: joined,
  no peer, broker down -> `"unknown"`, a peer with no matching role -> unmatched, two peers under one
  `claude`, two roles one cwd (joined by pid, not merged). Each guard mutation-tested.
- **Not measured:** how many peers a real `/list-peers` returns at 20 lanes, and whether the helper's parent
  is always the lane's `claude` on a `--self` relaunched tab. Both are checkable read-only before building.

## Needs from the PM

1. **Go or strike.** Is `status --peers` wanted, as opposed to the PM's own sweep staying by eye?
2. **Dependency on the broker.** `status` has never contacted anything; `--peers` is opt-in, read-only and
   soft-failing, so a down broker degrades one column. Say if opt-in is not enough.
3. **Where the client lives** (extend `notify.rs`, or unify with `peers.rs`).
4. **Whether `summary` may be printed.** It is free text a lane writes; `status` output goes to the PM and
   into logs. I would print it only under `--peers`, truncated, and never into the JSON default.

## Not proposed

Messaging a lane from `status`; acting on an unmatched peer; reconciling a `Crashed` role automatically.
Those are decisions, not a reader.

## Measurements (read-only, 2026-10-14 ~14:55Z, origin/main `a14c5e9`, broker `127.0.0.1:7899`)

Method: `POST /list-peers` (machine scope), then each peer's helper pid -> parent -> grandparent from the
process table (CIM `Win32_Process`), and each state file's recorded pid under `C:\Projects\.lane-state`
(29 state files carried a pid).

- **Peer count: 18** at ~17 lanes plus this one.
- **Helper's parent is a `claude.exe`: 18 of 18, and each parent pid equals exactly one state file's pid**
  (18 of 18 joined by pid). Nothing was joined by cwd.
- **Relaunched-tab shape: 18 of 18 `claude.exe` have `lane-restart.exe` as their parent and `WindowsTerminal.exe`
  above that**, i.e. all 18 are the host-relaunched shape. I did not separate "relaunched by `--self`" from
  "started by `lane-restart start`": both put the host above `claude`, so this does not discriminate them.
- **Built tool against the real broker**: `status --peers --json`: 31 roles, 18 with a peer, 0 unmatched, no
  error; the plain `status --json` has no `peers` key. The `synapse-restarttest-lane` row of 12:00Z is now
  `Crashed` with no peer, and the live session at that cwd is the role `restarttest` (`Running`, joined).
- **Not measured:** a broker that answers slowly (the timeout is the existing 5 s), and a session whose helper
  has already exited (it would simply not be in the broker's list).
