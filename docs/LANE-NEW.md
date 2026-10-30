# `lane-new <role> <repo>`: design

Written 2026-10-30 at `origin/main` a4fa90a (0.29.39). PM TASK 2026-10-30: automate what the PM did by hand six times
that day (rockfish, bonefish, openclownfish, cpufish, vulkane, ...). **Design only; no code.** PM-only, dry run by
default, every step read back. Facts below are marked **[PM]** (from the PM's task text), **[read]** (read at the ref
above from `C:\Projects\.lane-state`, a lane's spec, `spec.rs`, `start.rs`, `synapse-provision-weekly.ps1`) or **[inferred]**.

## What a new lane is, by the files

Read from rockfish, a lane created this way **[read]**:

| artifact | where | shape |
|---|---|---|
| repo | `ThinkersJournal/<repo>`, public **[PM]** | clone at `C:\Projects\<repo>`; seed: `LICENSE-APACHE`, `LICENSE-MIT`, `.gitignore`, `.gitattributes`, `README.md`, `HANDOFF.md` |
| launch spec | `.lane-state/<role>.launch.json` | schema 1, `role`, `name`, `cwd`, `model: sonnet`, `permission_mode: auto`, `launch_args` = `--mcp-config <state>\<role>.mcp.json --dangerously-load-development-channels server:claude-peers server:synapse`, `handoff_path`, `state` |
| MCP config | `.lane-state/<role>.mcp.json` | one `synapse` server: `synapse-claude-channel.exe --role R --home <LOCALAPPDATA>/synapse-lanes/R`, env `SYNAPSE_HOME`, `SYNAPSE_ADDR=127.0.0.1:7920`, `SYNAPSE_DAEMON_HOME` = the pilot home |
| soak list | `.lane-state/roles-soak.txt` | one role per line, `#` comments |
| Synapse identity | `<LOCALAPPDATA>\synapse-lanes\<role>\` | certificate from `synapse id provision`, plus the daemon's `synapsed.json` copied in |
| live lane | `.lane-state/<role>.json` (+ `host-<role>-<pid>.log`) | written by the lane's own hooks once the session is up |

## The failure modes already paid for **[PM]**

Each becomes a rule of the tool and a test:

1. **Backslash escaping in the spec.** Hand-written JSON with `C:\Projects\...` paths. *Rule:* the tool never writes
   JSON text. It builds a `SpecFile` and goes through `spec::write_atomic` (serde escapes; the HMAC/atomic-write path
   is the one every other writer uses); the MCP config is a `serde_json::Value`. *Test:* round-trip every path field
   through `spec::read` and compare to the input string; golden-compare the MCP config's key set and order to rockfish's.
2. **Wrong cwd for the log path.** *Rule:* no path in the tool is relative. Every path is derived from three roots
   fixed at startup (state dir, projects root, lane-home root), never from the process cwd. The report prints each
   resolved absolute path. **Open question 1:** the PM's phrase is ambiguous (the host log `host-<role>-<pid>.log`? a
   log the PM redirected by hand?); the rule covers both, but I want the exact case for the test.
3. **Launch spec `state` must be a valid variant (`suspended`).** The variants are `running`, `stopping`,
   `suspended`, `crashed`, `failed_relaunch` **[read]**. *Rule:* a new lane's spec is `suspended` (the value the PM
   found valid), written by the type, not by text. With `stop_kind: hard` the launcher service's `launcher_may_start`
   refuses it (`stop-hard`, read at start.rs:509), which is wanted: a brand-new lane must not wake on mail before the PM
   starts it. **Not yet read:** whether the PM's own `start` path accepts `suspended` + `hard`; slice 1's test must run
   the real `plan_start` for it, and if it refuses, step 4 needs a different write (open question 6). *Test:* spec
   parses; `plan_start` (real function, fake process table) accepts it for the PM caller and `launcher_may_start`
   refuses it.

Further modes the code implies **[inferred]**, tested the same way: role or repo name unsafe (same regex as the
weekly script: `^[A-Za-z0-9][A-Za-z0-9_.-]*$`, no `..`); role already present in the soak list, a spec, or a lane
state (refuse, never overwrite); the second `lane-new` run after a partial first (must converge, see Resume);
`start.lock` left behind; model is anything but `sonnet`/`haiku` (never Opus, CLAUDE.md §9).

## Contract

`lane-new <role> <repo> [--apply] [--continue] [--model sonnet|haiku]`

* **PM only, by process** (the same `caller_role` check `specs import` uses), not by `LANE_ROLE`. A lane's refusal is
  the first line of output, exit 1.
* **Dry run by default.** No switch: prints the ordered plan, with every path and the exact command for the
  PM-only step, writes nothing, runs nothing. `--apply` acts.
* **Every step: check, act, read back, print `ok`/`FAIL` with the property read.** An exit code is never the
  read-back (CLAUDE.md §5). A step that finds its end state already true prints `already` and does nothing.
* **Resume = rerun.** No checkpoint file: each step is check-then-act, so rerunning converges. A failed read-back
  stops the run at that step, exit 1, with the completed steps listed.
* Exit 0 all steps ok; 1 refused or a step failed; 2 stopped at the PM-only step awaiting `--continue`.

## Steps (in order)

| # | step | act | read back |
|---|---|---|---|
| 0 | validate | names, role unknown everywhere, model, disk floor (the existing 20 GB rule), caller is PM | the refusal list is empty |
| 1 | **create the repo (PM's hands)** | prints one command, `GH_TOKEN="$(gh auth token --user ciresnave)" gh repo create ThinkersJournal/<repo> --public --description "..."`, and stops (exit 2). The tool never holds or requests that token (CLAUDE.md §0, CIRESNAVE-EXPECTATIONS §3.1a) | on `--continue`: `gh api repos/ThinkersJournal/<repo>` as the bot returns 200 and `private == false` (a public repo reads without the owner token). **Control:** the same call on a name known not to exist returns 404 |
| 2 | clone + seed | `git clone` to `C:\Projects\<repo>`; write the template files; one commit "seed" on `main`; push | `git ls-remote origin main` equals the local SHA; each file present at `origin/main` by `git show`; licence files hash-equal to the reference copies (provenance, CLAUDE.md §9) |
| 3 | MCP config | write `<role>.mcp.json` from a typed value | re-read: parses; `--role` arg equals the role; home path under the lane-home root |
| 4 | launch spec | write `<role>.launch.json`, `state: suspended`, `written_by: other("lane-new")`, `stop_kind: hard` | `spec::read` returns a spec equal to the one built (cwd, handoff path, args) |
| 5 | soak list | atomic append of `<role>` to `roles-soak.txt`, keeping its line endings | re-parse with the weekly script's rule: role present exactly once; line count +1 |
| 6 | Synapse identity | run `synapse-provision-weekly.ps1 -RolesFile <temp file holding only this role> -Apply` (reuses its parsing, counts and "no key printed" rules; touches no other lane) | its `created`/`ok` count is 1 and `refused` is 0; `<lane home>\<role>\` holds a certificate and `synapsed.json` |
| 7 | start | `lane-restart start --role R --model sonnet --reason "lane-new"` | `<role>.json` shows a pid that is alive and not the (absent) previous session; spec now `running`; the host log exists at the printed absolute path and is non-empty |
| 8 | report | one `[NOTICE]`-shaped summary line per step, the lane's repo SHA, and the follow-ups the tool does **not** do (below) | none |

## Not done by the tool (printed as the last lines, so they are not forgotten)

* **Branch protection and CI** (CLAUDE.md §9 "CI and protection everywhere"): a new repo needs CI checks and a protection
  rule that requires them, and creating those needs the owner token or the first commits. Printed as "TODO: protect `main`
  once CI exists". **Open question 2:** should step 2's seed include the portfolio CI workflow, and does the PM want
  `lane-new` to print the protection command too?
* The ROADMAP/board row for the lane, the Serena project registration, and any `CLAUDE.md` entry: the PM's.

## Template

`crates/lane-new/templates/` (compiled in with `include_str!`, so the tool has no loose files to go stale): `README.md`,
`HANDOFF.md` (names the role, points at `AGENT-ONBOARDING.md`, first task = "ask the PM for your charter"),
`.gitignore`, `.gitattributes`. Placeholders `{{role}}`, `{{repo}}` only; an unreplaced `{{` after rendering is an
error. The two licence texts are **not** invented: copied from a named existing repo's files, hash-checked in step 2.
**Open question 3:** which repo is the reference for licences, README and `.gitignore` (agentlife's, or one of the six
the PM just made)?

## Where it lives

A new crate `lane-new` in this workspace (shares `lane-restart`'s `spec`, `caller_role` and `start`), subcommand of
`agentlife`? **Recommend a `lane-restart lane-new` subcommand**: the PM check, the spec writer and the `start` planner
are already there, and a second binary would copy them. Steps 1-8 are separate functions behind a `Facts` trait
(filesystem, `gh`, `git`, `synapse`, process table) so every step is tested against a fake.

## Tests

Per step: one that passes, one that fails its read-back and must stop the run (a fake that lies about the
read-back, with a control where the fake tells the truth), and the three paid failure modes above. A resume test:
fail at 5, rerun, assert steps 0-4 print `already` and only 5-8 act. Mutation per CLAUDE.md §6: make step 4 write
`state: running`, see the `plan_start` test go red; restore. No test starts a real lane or touches a real repo; the
first real run is the PM's, on a throwaway role, dry run first.

## Slices

1. Steps 0, 3, 4, 5 (local files only, fully fake-tested), `--dry-run` plan output. No network, no `gh`.
2. Steps 1, 2 (repo + seed; `gh`/`git` behind the trait).
3. Steps 6, 7, 8 (provisioning and start), then the PM's throwaway-role dry run.

## Open questions for the PM

1. The exact "wrong cwd for the log path" incident (command, cwd, which log).
2. Seed the portfolio CI workflow in step 2, and print the protection command?
3. Reference repo for licences / README / `.gitignore`.
4. Org name: `ThinkersJournal` as given **[PM]**; confirm that is the GitHub org login (the strategy doc says "Thinker's Journal").
5. Does the bot account have write access to a new org repo by default, or does the PM grant it in step 1's command (then step 2's push waits on it)?
6. If `plan_start` refuses `suspended` + `hard` for the PM, which state does the PM want a never-started lane to carry?
