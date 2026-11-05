<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# One user-level Synapse entry for every lane: what `SYNAPSE_ROLE` does and does not give us

PM [TASK] fleet-spec-tooling, slice 6 (2026-11-04): *"Try SYNAPSE_ROLE (the adapter usage line lists
it) so one user-level Synapse entry could serve every lane; report whether the home can be derived
too."* This is that report. No code changed.

## What was read and run

* Source: `synapse` at `origin/main`, `crates/synapse-claude-channel/src/main.rs` (read with
  `git show`, not from the working tree). Installed adapter: `synapse-claude-channel 6.0.0-rc.206`,
  whose `--help` prints `usage: synapse-claude-channel --role <role> [--home <dir>] (or SYNAPSE_ROLE)`.
* Two runs of the installed binary, neither able to reach the real daemon or the real home:
  1. **Control, no role anywhere:** `env -u SYNAPSE_ROLE -u SYNAPSE_HOME synapse-claude-channel`
     prints the usage line, exit 2.
  2. **Environment only, no flags:** `SYNAPSE_ROLE=probe-slice6 SYNAPSE_HOME=<empty temp dir>
     SYNAPSE_ADDR=127.0.0.1:1 synapse-claude-channel` got past role and home resolution and failed on
     `synapsed is not running: no daemon answered; no announce file at <temp>\synapsed.json`, exit 1.
     The temp directory stayed empty and the real home's `roles/` was unchanged afterwards.

## Findings

1. **The role can come from the environment.** `--role`, else `SYNAPSE_ROLE`. Run 2 above is the
   evidence; run 1 is the control that the same command without the variable is refused.
2. **The home cannot be derived from the role by the adapter.** `--home`, else
   `synapse::keystore::default_home()`, which is `SYNAPSE_HOME` or one platform default. It never looks
   at the role. So a single entry gets one home unless the home reaches the process some other way.
3. **The daemon is found through the home too.** The error in run 2 names `<home>\synapsed.json` and
   `SYNAPSE_DAEMON_HOME`. A per-lane home (`synapse-lanes\<role>`) works because the provisioning script
   copies the announce file into it; a lane's `mcp.json` today also sets `SYNAPSE_DAEMON_HOME` (the
   pilot home) and `SYNAPSE_ADDR` (`lane_new::mcp_config`).
4. **`LANE_ROLE` is already in every lane's environment.** `relaunch::set_lane_role` sets it on every
   launch, so the lane's role is available to whatever starts the adapter. `SYNAPSE_HOME` is not set by
   lane-restart today.

## What that means for a single user-level entry

A user-scope MCP entry named `synapse` would be matched by `--dangerously-load-development-channels
server:synapse` the way the user-scope `claude-peers` entry already is (the `--strict-mcp-config` note in
`spec_synapse.rs`). The entry would be:

```json
{ "command": "<cargo bin>/synapse-claude-channel.exe",
  "env": { "SYNAPSE_ROLE": "${LANE_ROLE}",
           "SYNAPSE_HOME": "<LOCALAPPDATA>/synapse-lanes/${LANE_ROLE}",
           "SYNAPSE_DAEMON_HOME": "<LOCALAPPDATA>/synapse-pilot",
           "SYNAPSE_ADDR": "127.0.0.1:7920" } }
```

* **Role: yes**, given that the host expands `${LANE_ROLE}` in the `env` block (see below).
* **Home: yes, but not by the adapter.** It is derivable only from outside it, by either path:
  (a) the same `${LANE_ROLE}` expansion inside a path, as above, which fits the per-lane homes that
  `lane-new` / `lane-provision` create; or (b) lane-restart exporting `SYNAPSE_HOME` at launch beside
  `LANE_ROLE`, which it can compute (`Roots::lane_home`) and which also covers a lane that keeps a
  non-standard home.
* **Not covered:** the lanes that share the pilot home (`--home ...\synapse-pilot`) would need that home
  in the entry instead, so a fleet that mixes both layouts cannot use one fixed entry until the layouts
  are one. That is a provisioning decision, not a tooling one.

## Not verified, and how to verify it

Whether Claude Code expands `${LANE_ROLE}` in the `env` of a **user-scope** MCP entry was not run: the
only way to test it is to start a `claude` session, and a session that loads the adapter claims a role
on the real daemon. A safe test would use a throwaway role and home, `SYNAPSE_ADDR` pointing at a closed
port (as run 2), and a one-line stand-in command in place of the adapter that writes its environment to
a file; then read that file. Until that is run, the role finding rests on the adapter's own behaviour
(proved) and the expansion on the documented `.mcp.json` behaviour (not tested here).

## Consequences for the tools built in this series

* `specs audit` / `heal_synapse` read `<role>.mcp.json` and the identity home it names. A lane served
  by a user-level entry would have neither file and would read as "no identity". If the entry is adopted,
  those two need a second source (the entry's env, resolved for the role) before the fleet is moved.
* `lane-provision` still has to provision the identity (certificate and announce file) per role; only
  the per-lane `mcp.json` would go away.
