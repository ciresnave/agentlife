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
   resolved absolute path. **The incident (PM answer, same day):** `(bash tools/gate-sched.sh ... > .lane-state/x.log &)`
   run from a shell whose cwd had become `C:\Projects\.claude-hooks` (after `cd .claude-hooks && lane-restart start`);
   the *relative* redirect failed "No such file or directory". *Test:* run the plan with the process cwd set to a
   directory that contains no `.lane-state`, and assert every printed path is absolute and the same as from the
   default cwd.
3. **Launch spec `state` must be a valid variant (`suspended`).** The variants are `running`, `stopping`,
   `suspended`, `crashed`, `failed_relaunch` **[read]**. *Rule:* a new lane's spec is `suspended` with
   `stop_kind` null (PM answer: `lane-restart start` accepted that for all six lanes made today, and refused `stopped`
   as an unknown variant), written by the type, not by text. The launcher service's `launcher_may_start` refuses
   `suspended` + no kind (`stop-unclassified`, read at start.rs:512), which is wanted: a brand-new lane must not wake
   on mail before the PM starts it. *Test:* spec parses; `plan_start` (real function, fake process table) accepts it
   for the PM caller; `launcher_may_start` refuses it; an unknown variant string fails `spec::read`.

Further modes the code implies **[inferred]**, tested the same way: role or repo name unsafe (same regex as the
weekly script: `^[A-Za-z0-9][A-Za-z0-9_.-]*$`, no `..`); role already present in the soak list, a spec, or a lane
state (refuse, never overwrite); the second `lane-new` run after a partial first (must converge, see Resume);
`start.lock` left behind; model is anything but `sonnet`/`haiku` (never Opus, CLAUDE.md §9).

## Contract

`lane-new <role> <repo> [--apply] [--model sonnet|haiku]`

* **PM only, by process** (the same `caller_role` check `specs import` uses), not by `LANE_ROLE`. A lane's refusal is
  the first line of output, exit 1.
* **Dry run by default.** No switch: prints the ordered plan, with every absolute path, writes nothing, runs nothing.
  `--apply` acts.
* **Every step: check, act, read back, print `ok`/`FAIL` with the property read.** An exit code is never the
  read-back (CLAUDE.md §5). A step that finds its end state already true prints `already` and does nothing.
* **Resume = rerun.** No checkpoint file: each step is check-then-act, so rerunning converges. A failed read-back
  stops the run at that step, exit 1, with the completed steps listed.
* Exit 0 all steps ok; 1 refused or a step failed. No step needs the owner token (PM answer: the bot's default token
  created `ThinkersJournal/<name>` and pushed to `main` with no grant, measured today), so the run never pauses; only
  branch protection and archiving need it, and those are printed, not run.

## Steps (in order)

| # | step | act | read back |
|---|---|---|---|
| 0 | validate | names, role unknown everywhere, model, disk floor (the existing 20 GB rule), caller is PM | the refusal list is empty |
| 1 | create the repo | `gh repo create ThinkersJournal/<repo> --public --description "..."` with the **ambient bot token** (never `GH_TOKEN` for another account, never `gh auth switch`: CLAUDE.md §0). Skipped (`already`) if the repo exists | `gh api repos/ThinkersJournal/<repo>` returns 200, `private == false`. **Control:** the same call on a name known not to exist returns 404 |
| 2 | clone + seed | `git clone` to `C:\Projects\<repo>`; write the template files; one commit "seed" on `main`; push | `git ls-remote origin main` equals the local SHA; each file present at `origin/main` by `git show`; licence files hash-equal to the reference copies (provenance, CLAUDE.md §9) |
| 3 | MCP config | write `<role>.mcp.json` from a typed value | re-read: parses; `--role` arg equals the role; home path under the lane-home root |
| 4 | launch spec | write `<role>.launch.json`, `state: suspended`, `written_by: other("lane-new")`, `stop_kind` absent | `spec::read` returns a spec equal to the one built (cwd, handoff path, args) |
| 5 | soak list | atomic append of `<role>` to `roles-soak.txt`, keeping its line endings | re-parse with the weekly script's rule: role present exactly once; line count +1 |
| 6 | Synapse identity | run `synapse-provision-weekly.ps1 -RolesFile <temp file holding only this role> -Apply` (reuses its parsing, counts and "no key printed" rules; touches no other lane) | its `created`/`ok` count is 1 and `refused` is 0; `<lane home>\<role>\` holds a certificate and `synapsed.json` |
| 7 | start | `lane-restart start --role R --model sonnet --reason "lane-new"` | `<role>.json` shows a pid that is alive and not the (absent) previous session; spec now `running`; the host log exists at the printed absolute path and is non-empty |
| 8 | report | one `[NOTICE]`-shaped summary line per step, the lane's repo SHA, and the follow-ups the tool does **not** do (below) | none |

## Not done by the tool (printed as the last lines, so they are not forgotten)

* **Branch protection** (CLAUDE.md §9 "CI and protection everywhere"): needs the owner token **and** the check names
  from a first CI run, so it cannot be done at creation. The last line printed is
  `tools/protect-main.sh ThinkersJournal/<repo> <contexts...>` with the placeholder for the contexts, plus "run after
  the first CI run on `main`" (PM answer: print, not run).
* The ROADMAP/board row for the lane, the Serena project registration, and any `CLAUDE.md` entry: the PM's.

## Template

`crates/lane-new/templates/` (compiled in with `include_str!`, so the tool has no loose files to go stale): `README.md`,
`HANDOFF.md` (names the role, points at `AGENT-ONBOARDING.md`, first task = "ask the PM for your charter"),
`.gitignore`, `.gitattributes`, and the CI seed below. Placeholders `{{role}}`, `{{repo}}` only; an unreplaced `{{` after
rendering is an error. **References (PM answer):** `LICENSE-MIT` and `LICENSE-APACHE` copied from onemkl's (holder Eric
Evans, "correct until the entities exist"), hash-checked in step 2 against the onemkl copies at `origin/main`;
`.gitignore` is `/target`; `README.md` says the project is planned and its crate names are reserved.

**CI seed (PM answer: yes):** `.github/workflows/ci.yml` and `.github/CODEOWNERS` in the shape of capant's at
`origin/main` (read: jobs `fmt`, `clippy`, `test (<os>)` on ubuntu/windows/macos, `doc`, `cargo deny (advisories, licences,
bans, sources)`, `portfolio`; CODEOWNERS `* @ciresnave`). capant's `capant-check` job is capant-specific and is **not**
copied. The names are printed so the PM can pass them to `protect-main.sh`. **Slice 2 reads capant's file again at that
time and diffs the template against it**; a seeded workflow that drifts from the fleet's is the failure, and a hidden
`cargo` step in a lane with no `Cargo.toml` yet would fail its first run (open question 7).

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

## Answered (PM, 2026-10-30, from creating six lanes by hand) and still open

Answered, and folded into the text above: the cwd incident (failure mode 2); seed the CI workflow and print the
protection command; onemkl as the licence reference; `ThinkersJournal` is the org login (`ThinkersJournal/rockfish`
exists); the bot's default token creates and pushes without a grant; a never-started lane is `suspended` with no
`stop_kind`.

Open:

7. A seeded `ci.yml` copied from capant runs `cargo fmt`, `clippy`, `test`, `doc` and `cargo deny` against a repo with no
   `Cargo.toml` yet, so its first run fails. Does the PM want the seed to include a minimal `Cargo.toml` + `src/lib.rs`
   (crate name reserved, as the README says), or jobs guarded on `hashFiles('Cargo.toml') != ''`?
