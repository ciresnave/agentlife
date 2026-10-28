# Synapse-only for every lane: the agentlife side of the cutover plan

PM task 2026-10-28. CireSnave, 2026-10-28, verbatim: *"I want every lane to be able to run with Synapse alone.
... No claude-peers."* and, for the dialog, *"Any lane should be able to run with Synapse and Synapse only."*
Measured 2026-10-28 ~18:20Z at `origin/main` 582be0a. This covers what agentlife and `lane-restart` own; the
daemon, the roles' keys and the PM's sweep are named where they block, not planned here. Nothing below was run
against a lane. Synapse's own readiness reading is `synapse/docs/CUTOVER_READINESS_2026-10-15.md` (13 days old; its
G5 and G6 have moved since).

## What is true today (measured, with its control)

| fact | measurement | control |
|---|---|---|
| 31 lane specs; dev-channels: 4 `server:synapse` only, 8 both, 19 `server:claude-peers` only | python over `C:\Projects\.lane-state\*.launch.json` `launch_args` | 4 + 8 + 19 = 31 = the file count |
| Only 5 specs carry `--mcp-config` (`agentlife`, `overmind`, `pm`, `restarttest`, `synapse`) | same script | the 5 files exist on disk |
| `claude-peers` is registered at **user scope** (`~/.claude.json` `mcpServers`: `claude-peers`); 0 of 59 projects add a server; Synapse is not user-scope | server names only, values not read | the key list is non-empty |
| The 8 "both" lanes (`bitman`, `fmp-rs`, `httpant`, `json-mcp-server`, `pm`, `rust-analyzer-mcp-server`, `wasm-sandbox`, `webhostant`) name `server:synapse` but **7 of them have no `--mcp-config`**, so where their Synapse server would come from is **unverified** (only `pm` has one) | as above | `pm.mcp.json` exists |
| The pilot daemon knows **5 roles**: `agentlife`, `lane-commysdk`, `overmind`, `pm`, `synapse` | `synapse list` against `synapse-pilot` on 127.0.0.1:7920 | the call returned 5 rows incl. my own role |
| The any-lane dev-channels record is merged (ciresnave/ciresnave#14); agentlife#176 tests it | `gh pr view` | merged 17:50:42Z |

So of 31 lanes, at most 5 have a Synapse identity on that daemon today, and 26 cannot receive mail from it:
a recipient must have claimed once (`LAUNCHER-SERVICE.md`, scratch-daemon check).

## What a Synapse-only lane needs (per lane)

1. **A Synapse role and key** (the Synapse lane's `synapse id`/claim flow). Blocker for 26 lanes.
2. **A per-role MCP config** `<state>/<role>.mcp.json` like `agentlife.mcp.json` (the `synapse-claude-channel`
   command with `--role <role>` and the daemon's `SYNAPSE_HOME`/`SYNAPSE_ADDR`), passed as `--mcp-config`.
3. **The spec's `launch_args`**: `--mcp-config <that file>` plus `--dangerously-load-development-channels
   server:synapse` only, no `server:claude-peers`. The any-lane record answers exactly that Channels value;
   a spec that still lists both is answered by *no* any-lane record (the both-pilot record covers 3 roles only
   and expires 2026-10-31), so a half-migrated lane stops at the dialog.
4. **`claude-peers` out of the lane's session.** It is user-scope, so it loads in every session whatever the spec
   says. Removing it (`claude mcp remove -s user claude-peers`, or `--strict-mcp-config` with only the lane's own
   `--mcp-config`) is the step that makes "Synapse and Synapse only" literally true. It also removes it from
   CireSnave's and the PM's own sessions. **A CireSnave-level change by his words, executed by the PM.**
5. **CLAUDE.md and MESSAGE-FORMATS** wording that names claude-peers (`list_peers`, `send_message`): the PM's files.

## What in agentlife and lane-restart still talks to the broker

| piece | state | Synapse path |
|---|---|---|
| `lane-restart` notify (`notify.rs`, `host.rs`, `approvals.rs`) | `notify_transport` in `~/.overmind/lane-restart.json`, **default `broker`** | `synapse` transport built (G6 "built, not run" in the readiness doc; scratch-daemon check passed 2026-10-22). Flipping it is the PM's: I never flip it |
| `agentlife` `down` wrap-up message and `status --peers` (`src/peers.rs`) | `Broker` and `SynapseMessenger` both exist; `main.rs` picks by config | built |
| `restore` live-session detection, role to peer join (`peers_join.rs`) | uses the broker's list to find who is alive | **unverified** whether the Synapse `list` join is wired for every caller; read `main.rs` call sites before the flip |
| duplicate-launch check (`duplicate.rs`) | process table and state files, not the broker | no change |
| approval records | `claude-peers-dev-channels.json` (expires 2027-03-19), pilot records expire 2026-10-31, any-lane has no expiry | once every spec is synapse-only the first three are dead letters; retire them, do not extend |

## Key placement: can a lane read only its own key?

**Measured** (names and sizes only, no contents): the pilot home keeps one directory per role,
`roles/<role>/{signing.key.pem, sealing.key.pem, cert.pem}`, plus `account/account.key.pem`. Its ACL is
`SYSTEM`, `Administrators` and `TUF-LAPTOP\cires`, all inherited. Synapse's `keystore.rs` refuses a key that is
"accessible to more than its owner" (`PermissionsTooOpen`), and the owner is the Windows user.

**What per-lane ACLs can and cannot do.** Every lane runs as that one user, so any ACL that lets lane A read
`roles/a/` also lets lane B read it: Windows ACLs separate principals, not processes of one principal, and
denying the user would lock the lane out of its own key. So on this machine, as the lanes run today:

- **Cannot:** stop one lane reading another's signing key. After the bulk claim, any lane could sign as any role.
  The daemon verifies keys (readiness doc, G9), but it cannot tell whose hands hold a valid key.
- **Can, by arrangement, with the same-user limit unchanged:** give each lane only its own `--role` in its MCP
  config; keep the keys out of every lane's cwd and out of git; keep the owner-only check the keystore
  already makes; and rely on the daemon's `security-events.jsonl` for what it records (what it records for a
  forged claim is **not verified**). Detection at best, not prevention.
- **Can prevent, with a different principal per lane:** (a) a **WSL distro user per lane**, which is what
  `lane-sandbox` and the `lane-spike` pilot do (`lane-commysdk` cannot read another Linux user's `~/synapse-home`);
  (b) a **Windows local account per lane** (a launcher change: `CreateProcessWithLogonW` or a scheduled task per
  user, plus the lane's tools installed per user); (c) **Windows Sandbox / AppContainer** per lane, which the
  AntSpawn matrix records as `windows-sandbox` yes. Each is a separate project with its own cost.

**So: real per-lane key isolation needs the WSL isolation (or one of (b), (c)); on plain Windows it cannot be
had.** I recommend: Windows lanes cut over with the arrangement above, the limit written into the cutover record as
stated; lanes whose keys matter most (`pm`, `overmind`, anything holding the `StateAdmin`/`RoleAdmin` grants in
`keystore.rs`) go first into the WSL isolation. Whether that residual risk is acceptable is the PM's to put to CireSnave
in one sentence: on Windows the keys are not per-lane.

## Order (each step reversible; one lane first, as every rollout here)

0. **Prerequisites, others'**: G7 (who starts `synapsed` at login), a role and key per lane, `launch_senders` for
   wake (separate; `M5-WAKE-DESIGN.md` slice 3).
1. **Tooling, mine, small PRs**: (a) `lane-restart specs` can rewrite one lane's `launch_args` to the Synapse-only
   form, dry-run first, refusing a lane with no MCP config file; (b) `agentlife doctor` reports, per lane, which
   of {role claimed, `--mcp-config` present and readable, no `server:claude-peers`, any-lane record loadable}
   holds; (c) a test that a Synapse-only spec's dialog is answered by the any-lane record and a both-server one
   is not (the second half is already in `real_records.rs`).
2. **Pilot lane**: `lane-commysdk` already has a role on the daemon and is the WSL pilot lane; or `agentlife`
   (already synapse-only, so the lowest-risk first full "no peers" run). The PM picks.
3. **Waves by lane count, not importance**: the 4 synapse-only lanes need only step 4 (strip user scope); the 7
   "both" lanes with no config and the 19 peers-only need steps 1 to 3 (26 lanes, as the daemon's role list says);
   `pm` has its config already. Each wave ends when the
   lane has *restarted* under the new spec and sent and received one message, read back from `synapse inbox`.
4. **Last**: strip `claude-peers` from user scope; flip `notify_transport`; retire the three old records.
5. **Rollback** per lane: restore the previous `launch_args` (the spec rewriter keeps one backup) and re-add the
   user-scope server. Before step 4, both servers work side by side, so rollback of a lane never loses mail.

## Not decided here

Whether the PM's `[NOTICE]` broadcast (G8) is needed after cutover; the broker's shutdown; anything that writes
another lane's HANDOFF or runs `start`/`stop` (PM only).

## Rulings (PM, 2026-10-28)

1. Pilot is `agentlife`: strip `claude-peers` from its session with `--strict-mcp-config` and its own
   `--mcp-config` first; user scope is touched only at step 4.
2. Step 1(a), the spec rewriter, is mine to build: dry-run first, one backup, refuses a lane with no MCP config.
3. The 7 "both" lanes are assumed to have **no** Synapse server (their `server:synapse` channel name is inert and
   they are not on the daemon's role list). Plan them as peers-only lanes; verify by the MCP tools a restarted
   lane actually lists. The PM has asked the Synapse lane for the bulk role/key claim (26 lanes) and for G7.
