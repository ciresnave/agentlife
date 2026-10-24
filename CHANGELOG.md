# Changelog

One version for the whole workspace (`agentlife`, `lane-restart`, `lane-state`).

## 0.29.18

- **CI: an advisory CodeRipper job in the main workflow (PM TASK 2026-10-24).** `jobs.portfolio` calls `ciresnave/coderipper/.github/workflows/portfolio.yml` pinned to commit c665210ad0972cf3ddb7bc81bdd7800c3248a938 with `contents: read` and no inputs (both repositories are public, so the default GitHub-hosted runner applies). It is NOT a required check and no rule is silenced or configured out of the analyser. Baseline findings are to be turned into one ROADMAP row per failing rule once the PM sends them. It ran on this PR's own checks (the `portfolio` job passed in 4 min 12 s on head ca7e92e). Version 0.29.18 is provisional (#170 merged as 0.29.17).

## 0.29.17

- **Docs only: `docs/LANE-RESTART-WSL.md` records P2 and P4 as measured (PM go, 2026-10-24) and corrects the process-match rule.** Run on `lane-spike` with a stand-in (`lane-sandbox -- sleep 600`, no claude, no host, no settings touched). P2: `wsl.exe --exec` keeps a spaced prompt, `a;b`, an embedded quote, `--name` and a literal `$HOME` as single arguments, `--cd` takes the POSIX path, the wrapper's `-- cmd` convention works. P4: the `ps -u` probe sees the whole lane tree from outside the sandbox and killing the Windows-side `wsl.exe` ended all of it (`bwrap --die-with-parent`). **Correction:** the probe also lists two `bwrap` processes whose command line ends with the lane's own argv, so the rule "args carry `--name <role>`" would have matched them; the rule is now `argv[0]` basename `claude` plus `--name <role>` plus a start time not before the host's `wsl.exe`. Not measured: a real `claude`; P1, P3, P5 are held by the PM. Version 0.29.17 is provisional (#168 merged as 0.29.16).

## 0.29.16

- **Docs only: `docs/ROADMAP.md` row for OverMind#96 (b), `SECRET_WORDS` lacks `passkey` (PM TASK 2026-10-23, roadmap-or-close).** Read at `origin/main` 616b8d2: OverMind#96 (a) is fixed in agentlife's `host.rs` since the import (e742f01: the unhandled-dialog capture runs on its own thread) and OverMind#117 by 52fa622 (#113: the 800 ms literals remain, but the window starts at the child's first output, with a regression test), agentlife#14 already has Next row 2. This lane does not close the OverMind issues; the PM does, after this merges. Version 0.29.16 is provisional (#167 merged as 0.29.15).

## 0.29.15

- **Docs only: `docs/LANE-RESTART-WSL.md`, the spec for a `lane-restart` launch mode `wsl` (PM TASK 2026-10-23, WSL2 pilot, decision 179 B).** Spec fields (`launch: wsl`, `wsl.distro`, `wsl.user`, `wsl.wrapper`, optional `wsl.state_dir`; a WSL spec is `schema: 2` so an older build refuses it instead of launching a POSIX `cwd` as a Windows process), the exact argv (`wsl.exe -d D -u U --cd CWD --exec WRAPPER -- claude PROMPT ... dev-channels flag last`), the Windows host kept as host, state read through the distro's UNC path, a `WslFacts` implementation of the `SystemFacts` seam, `--self` refused for a WSL lane (the lane asks the PM by Synapse), the refusals, a red-first test plan and five pilot measurements. Nothing is built, and every WSL behaviour not yet seen is marked UNVERIFIED: `lane-spike` was Stopped when this was written, and starting it is the PM's pilot. Version 0.29.15 is provisional (#166 merged as 0.29.14).

## 0.29.14

- **Dependency: `persistant` 0.4.4 to 0.9.28 (PM TASK 2026-10-23; CireSnave: all dependencies on their most recent versions).** The requirement is `0.9.28`; the lock resolves it to 0.9.42. It builds without source changes. Version 0.29.14 is provisional.

## 0.29.13

- **Docs only: `docs/GITHUB-APP.md` closes the "Unproven" note (PM TASK 2026-10-23).** The permission key `organization_self_hosted_runners` at **write** was accepted by GitHub's real create page: installation 174476958 of `antspawn-runner` (id 5441608) carries exactly `organization_self_hosted_runners=write` + `metadata=read` (PM's read, `-Verify` ok). Also records that repository Actions: Read-only is not in the manifest (403 on `antspawn-proof` until added in the browser). Version 0.29.13 is provisional.

## 0.29.12

- **Dependency: `ctutils` 0.4.3 to 0.4.5 (RUSTSEC-2026-0385, "Non-constant-time mask generation and `u128` comparisons", PM TASK 2026-10-23).** `cargo deny` failed on `main` (seen on #164, 2026-10-24); `cargo update -p ctutils` only, and `cargo deny check advisories` reads `advisories ok` afterwards. Version 0.29.12 is provisional.

## 0.29.10

- **Fix: `github-app-setup.ps1 -Apply` could lose GitHub's redirect (CI flake on #162, 2026-10-22, and a real bug in the script).** The wait loop called `HttpListener.GetContextAsync()` every poll and abandoned the task when the 2 s wait timed out; an abandoned task still collects the NEXT request and drops it, so any request that arrives after more than one poll (a human clicking for minutes, then GitHub's redirect to `/callback`) can be swallowed and never answered, and the script then waits out its timeout. CI saw it as "existing connection was forcibly closed" on the test's first request after 129 s. The script now keeps ONE outstanding accept across polls and starts a new one only after it completed. The test's first request is now sent 5 s late (more than two poll timeouts) with a 20 s client timeout, so the race is exercised every run instead of by chance: seen red against the old script (the request times out), green against the fix. The failing job ran on a GitHub-hosted runner, not on this machine, so the Visual Studio update here was not involved. Not done: nothing else in the script was re-measured.

## 0.29.9

- **Docs only: roadmap C5 design, `ciresnave-bot` as a GitHub App (PM TASK 2026-10-22; CireSnave: "Yes on ciresnave-bot as an App.").** `docs/GITHUB-APP.md` gains the design: a token broker (the App key never reaches a lane) minting 1-hour, repository- and permission-scoped installation tokens per lane in three tiers (`lane`, `read`, `workflows`), a `gh` shim and git credential helper, what commits and PRs show (`<slug>[bot]`, `app/<slug>` in `gh`, the App's noreply email), one App first (PM decision) versus per-lane Apps later, a survey of what is keyed to the bot login (0 code references; CODEOWNERS is `* @ciresnave` in 30 of 113 repositories; the PM's admin read of branch protection: 33 repositories protected, 0 name the bot, 3 require reviews, 61 unprotected, 19 private on the free plan), the Docker publishers (synapse: Docker Hub, auth-framework: ECR, auto-discovery: GHCR), and a staged rollout starting with a one-repository pilot. Every GitHub behaviour not yet seen is marked UNVERIFIED and is a pilot step. Nothing is built. Version 0.29.9 is provisional.

## 0.29.8

- **`gh-runner-setup.ps1`: three defects from the PM's two live runs (2026-10-22).** (1) **Service name:** found by directory, not by a computed name (below). (2) **Password alphabet:** `New-RandomPassword` used `% ^ ! ? + =`; `config.cmd` is a `.cmd` file and cmd.exe mangles `% ^ !`, so the second run died with `Invalid windows credentials entered`, `config.cmd` exited 1, and a runner (licence-tools) was registered with no service. The alphabet is now letters (no look-alikes), digits and `- _ @`; the test sends 40 generated passwords through a real `.cmd` file and compares what it received, with a control showing the old characters are mangled. (3) **Rotation broke existing services:** an existing account's password was rotated to a value nobody knows, which would have failed every runner service already configured with the old one (ThinkersFoundation-site at its next start). The same run now gives each service that logs on as the account (either spelling, any root) the new password through `Win32_Service.Change` (no command line), and names the service if one cannot be updated. `Select-AccountServices` is tested with fake services; the `Change` call itself is NOT run against a real service. Details of (1): `-Apply` configured `ThinkersJournal/ThinkersFoundation-site` correctly (service Running as `.\gh-runner-private`, runner online) and then threw `service actions.runner.ThinkersJournal-ThinkersFoundation-site.TUF-LAPTOP-ThinkersJournal-ThinkersFoundation-site not found`, aborting before the next repo: the runner truncates names, and the real one is `...TUF-LAPTOP-ThinkersJ-9595`. New `Find-RunnerService` matches the service whose `RunnerService.exe` lives under the runner directory (case-insensitive, quoted or not, with trailing arguments; a sibling directory sharing a prefix does not match; two matches are refused). `-Remove` looks the service up before `config.cmd remove` and checks it is gone. `scripts/tests/gh-runner-setup-service.tests.ps1` (run by `tests/runner_setup_service.rs`, Windows) uses fake service objects including the over-80-character name; mutation seen red: dropping the trailing separator from the prefix fails the sibling-directory test. Not done: a re-run against a real install, and the `ThinkersFoundation-site` runner left half-finished by the failed read-back needs the remaining repos re-run (the PM's call). Version 0.29.8 is provisional (#158 0.29.6 merged, #160 0.29.7 ahead of it).

## 0.29.7

- **Docs and an example, no runtime change: the notify-outbox -> Synapse send path was run against a real daemon (cutover gate 3, PM TASK 2026-10-22).** `examples/synapse_outbox_check.rs` queues messages and drives `relay_outbox` against whatever `SYNAPSE_HOME`/`SYNAPSE_ADDR` name (a throwaway home only). Result in `docs/LAUNCHER-SERVICE.md`: the message to a claimed `pm` arrives once as `agentlife-launcher@<account>/<message_id>`, redelivers once after its lease with the same id, a resend of the same id adds nothing, ack works, and a message to a role that never claimed is refused `unknown_recipient` and stays queued. Not covered: the `agentlife launcher` process, an sqlite-store daemon. The 0.27.11 `-MailboxStore` parameter of `scripts/synapsed-task.ps1` already sets `SYNAPSE_MAILBOX_STORE` in the wrapper when given; this daemon (rc.123) calls that variable deprecated and prefers a recorded store (`synapse storage use`).

## 0.29.6

- **Docs only (roadmap C3):** `docs/TOKEN-INVENTORY.md`, a names-only inventory of every token and credential we hold or script (the vault's 18 entries, the two GitHub identities and their scopes, 22 `CODECOV_TOKEN` repo secrets, crates.io and Docker Hub secrets, secrets that workflows reference but nothing configures), each marked replaceable by a GitHub App, by OIDC (Codecov, crates.io Trusted Publishing, AWS) or not replaceable, with an order of work and five questions for CireSnave (org-level secrets could not be read). No value was read. No code change.

## 0.29.4

- **Tests only: the four Synapse approval records are tested as they stand in `ciresnave/ciresnave`, not as built from the inert example.** `crates/lane-restart/tests/fixtures/approvals/` holds byte-for-byte copies of `synapse-dev-channel-pilot`, `claude-peers-synapse-dev-channels-pilot`, `synapse-dev-channel-restarttest` and `claude-peers-dev-channels` (approvals folder at commit 3b5d508, 2026-10-22T14:26Z); `handlers/real_records.rs` checks each against a hand-written expectation: `parse_and_validate` ok, action `1`, exact roles, `is_active` true for each role one second before `expires_at` and false at it, false for an out-of-scope role (true only for `*`), answers exactly its own Channels set and none of the other two (plus `, server:extra` and `,server:extra` variants), and refuses a screen asking for a password or missing either anchor. The two real captures (`REAL_DIALOG_SCREEN`, `TWO_SERVER_SCREEN`) are each answered only by records naming their set. Mutations on the pilot fixture, each applied once and each failing two tests: one character of Channels, one anchor, one role, the expiry date. Not done: a drift check against the repo (a non-gating job), and a real one-server `server:synapse` capture (none exists; that screen is the two-server capture with its Channels value replaced). The copies go stale when CireSnave edits a record; the commit is named so the comparison is possible.

## 0.29.2

- **Approvals load in ONE `gh` call, however many approval files exist (PM finding, 2026-10-22).** `lane-restart approvals` refused five per-lane records ("gh timed out after 1s", "the 20s approvals budget is used up"): `GhReader` did one `gh api` read per file, so the cost of a load grew with the number of approvals and the 20 s budget ran out mid-folder. New `RepoReader::snapshot` (defaulted to `None`, so every existing reader and test double is unchanged) returns the default branch's tip, the listing and every file's text together; `GhReader` answers it with a single GraphQL query (`head:object(expression:"HEAD")` and `object(expression:"HEAD:<folder>")` with each blob's text, size, truncated and binary flags), `approvals::parse_snapshot` reads the answer, and a blob that arrives truncated, binary or oversize has no body and is refused by name. `load` takes that path when the reader offers it and runs the same per-file rules (the name-is-the-id rule, the 64 KiB and 64-file limits, validation, the SHA check) over it; `CachingReader` stores the listing and each file by commit as before, so the last-known-good fallback (0.28.0) works from a snapshot too. Known-bad test: 40 approvals load in exactly one call and a reader that panics on any per-file call proves it, while the same 40 through a per-file reader with a six-call budget load only some (the control that reproduces the starvation). Not done: the tarball alternative; secrets in blobs are never fetched differently (the same validator and secret-word veto apply).

## 0.29.1

- **Script, no code change (roadmap C1, CireSnave 2026-10-22: "lets lean toward GitHub apps anywhere tokens are required"):** `scripts/github-app-setup.ps1` creates the runner GitHub App through GitHub's manifest flow and stores its key. Dry run by default; `-Apply` serves a one-page loopback form, GitHub redirects back with a single-use code, the one-time `state` is checked (constant-time, Host must be `127.0.0.1:<port>`, GET only), the code is exchanged, and the private key goes straight into a DPAPI user-scope blob in `%LOCALAPPDATA%\antspawn\github-app` (owner-only ACL; the watcher account, PM-confirmed) and is never printed, logged or put in an argument or environment variable; the client and webhook secrets are discarded. CireSnave clicks twice (create, install). Permissions: organization Self-hosted runners write, Metadata read; `-RepoAdminFallback` adds Administration write. `-Verify` signs an RS256 JWT, lists the installations, mints one installation token each, checks the permissions, revokes the token and records the installation id. Only `https://github.com`/`https://api.github.com` or a loopback http address are accepted as endpoints. `scripts/tests/github-app-setup.tests.ps1` (run by `tests/github_app_setup.rs`, `pwsh`, Windows) drives it against a local stand-in for GitHub, including the known-bad controls (wrong or missing state, wrong code, non-GET, wrong signature, missing permission, not installed, a second `-Apply`, non-GitHub endpoints) and checks that no output or file holds the PEM, a secret or a token; mutations (state unchecked, token not revoked, PEM or blob printed) each fail a test. Not done: creating the App (CireSnave's two clicks), rotating only the key, the `with-secret` vault as a store. See `docs/GITHUB-APP.md`.

## 0.29.0

- **Breaking (pre-1.0): one approval record answers the folder-trust dialog for every folder under a prefix (board 244; CireSnave 2026-10-22: "Yes.  Blanket trust all directories under C:\Projects so future projects are already approved for that dialog.").** antspawn and configurant stopped at the trust dialog on 2026-10-21/22 because that approval is one record per lane folder. New `match.folder_prefix` (`handlers::MatchSpec`, new `folder_trust` module): a record carrying it can answer the folder-trust dialog and nothing else (load refuses it unless it pins the trust anchors and lines, has no `fields` or `isolated_lines`, and its action is exactly `<Down><Enter>`); a folder is answered only if it is a plain `X:\a\b` path (no UNC or `\?\`, `..`, `/`, 8.3 name), strictly under the prefix component by component (so `C:\Projects-evil`, `C:\ProjectsX` and the prefix itself fail), equal to its own canonical form on disk (a junction or symlink out is refused, the prefix itself must not be redirected), and shown on one unwrapped row. Every such answer is appended to `<state dir>/folder-trust-answers.log` (time, lane, handler, resolved folder) and announced to the notify role in one `[NOTICE]` line; `SCREEN CHANGED` diagnostics carry the folder verdict. `HandlerAnswered.resolved_folder`, `MatchReport.folder` and `MatchSpec.folder_prefix` are new (hence breaking); `handlers::matches_with`, `find_matching_handler_resolved`, `host::check_and_inject_with` take the filesystem explicitly. Tests (each mutation of the canonical-equals check, the component-wise prefix, the action check, the prefix-redirect check and the wrapped-row check fails one): `..`, junction out (staged and a REAL NTFS junction), UNC, 8.3, prefix confusion, the prefix itself, a missing folder, another drive, a wrapped path; dev-channels dialogs and secret-asking text never answered; expired record; load-time refusals. The draft record is `approval-example/trust-folder-prefix.json` (his merge in `ciresnave/ciresnave` is the approval; this change activates nothing by itself). Not done: the per-lane records are not removed.

## 0.28.0

- **Breaking (pre-1.0): a slow GitHub no longer leaves a lane's startup dialog unanswered (PM-directed root cause, 2026-10-22).** Two lanes (antspawn 05:32Z, fuel 10:36Z) stopped at the `--dangerously-load-development-channels` dialog although `claude-peers-dev-channels` is approved for every role: the host logged `APPROVALS 0 active - could not resolve ciresnave/ciresnave's default branch: gh timed out after 6s/7s`. Cause: the commit-keyed cache could not help because the branch-tip lookup (two sequential `gh` calls inside an 8 s budget for the whole load) always goes to the network. Measured over 591 host logs: 5 timeouts, 4 of them among ~124 loads since the 8 s budget. Now: (1) the tip is ONE call, `repos/<repo>/commits/HEAD` (a repo's HEAD is its default branch; equal to the branches call for `ciresnave/ciresnave`, 2026-10-22); (2) `approvals::LOAD_BUDGET` 8 s -> 20 s (the appear verdict is 120 s now); (3) `CachingReader` remembers the last commit whose load COMPLETED (`<cache>/<hash>.good`: repo, commit, verified-at) and, when the tip lookup fails with a TRANSIENT error (`approvals::is_transient`: timeout, connect, DNS, TLS, reset, HTTP 500/502/503/504) and that commit is at most `approvals::MAX_STALE` (24 h, PM-approved) old, loads it from the cache instead, marked `Loaded::stale` and `STALE: ...` in the `APPROVALS` log line and `lane-restart approvals`; the stand-in never refreshes its own verified time. Any other failure (404, 401, 403, rate limit, an unrecognised message), no remembered commit, one older than 24 h, or files missing from the cache still gives `0 active`, as before. Cost: an approval removed from the repo stays honoured for up to 24 h while GitHub is unreachable from this machine. (4) The host logs `APPROVALS-WARNING` (and prints a warning) at boot when no handler is active. `approvals::Loaded` gains `stale` and `RepoReader` gains defaulted `stale_note` and `record_good`, hence breaking. Tests: seven fall-back cases and their refusals (each mutation of the transient check, the stale guard and the age limit fails a test), plus the real two-server dialog capture and server:synapse / claude-peers record shapes matched exactly, and unrecognised or secret-asking dialog text still refused. Not done: folder-trust approvals for the new lanes antspawn and configurant (one record per lane, each carrying CireSnave's quote, in his repo; board 244).

## 0.27.11

- **Script, no code change:** `scripts/synapsed-task.ps1` takes `-MailboxStore <value>`, written into the wrapper as `set "SYNAPSE_MAILBOX_STORE=<value>"` so it reaches the scheduled-task daemon (the wrapper set only SYNAPSE_HOME/ADDR/LAUNCHER_ROLES, so a store chosen by the lane never got there). Default unset: the dry-run output is identical to 0.27.8 (compared with the timestamp in the wrapper name masked), no line is written. A value with a cmd metacharacter is refused like `-LauncherRoles`. Not registered; the PM's T-0 run applies it. Not breaking.

## 0.27.10

- **Lock-only security fix:** `rustls-webpki` 0.103.15 -> 0.103.16 and `webpki-root-certs` 1.0.9 -> 1.0.10 (RUSTSEC-2026-0382/0383). `cargo deny check advisories` ok and `cargo audit` exits clean (one allowed warning: `inout` 0.2.3 yanked). No code change.

## 0.27.8

- **`lane-restart host` parses with clap derive (audit group 5, row 6, last slice):** `host --role <role> -- <child argv...>`; the old and new parsers were compared over all 299,593 argument sequences of up to 6 tokens from an 8-token pool (5,184 accepted) and agreed on accept/reject and every parsed value, `--` handling included. `host_boot::record` still runs before the parse. `ArgError` is gone: nothing hand-parses an argument list any more. `state <event>` stays a slice pattern in `main` (a single positional, the hook hot path: no loop to replace). Refusal text is clap's. No API change.

## 0.27.7

- **`lane-restart attach` and the restart flags (`--role`/`--self`/`--dry-run`/`--yes`) parse with clap derive (audit group 5, row 6, slice 3):** same method as 0.27.2/0.27.3. Old and new parsers were compared over all 111,111 argument sequences of up to 5 tokens from a 10-token pool (500 accepted `attach`, 2,721 accepted restart lines, so the comparison saw both outcomes) and agreed on accept/reject and every parsed value; for `attach`, a trailing `--role` with no value was already a refusal, now caught one step earlier with the same exit 1. `--self` still defaults to false. Refusal text is clap's. Still hand-written: `host`, `state`, `--help`/`--version`. No API change.

## 0.27.6

- **CI builds and tests with `--locked` (CodeRipper SUP-001):** `cargo build` and `cargo test` in `.github/workflows/ci.yml` now pass `--locked`, as `clippy` already did, so a stale `Cargo.lock` fails CI instead of being silently re-resolved. No code change.

## 0.27.5

- **Fix: `lane-restart` host no longer reports `outer-input` as abandoned when its input ends just after the child exits:** teardown sampled the outer-input thread's liveness flag at once, while only the PTY reader got a bounded wait, so a fast child on a loaded runner made `host::tests::no_helper_thread_outlives_a_host_run` fail on ubuntu CI (agentlife#142, run 0; passed on rerun). The host now waits up to `INPUT_END_TIMEOUT` (1 s) for the thread; a really blocked input is still reported. New test `an_outer_input_that_ends_just_after_the_child_is_not_abandoned` (fails without the wait with the CI message). Not breaking.

## 0.27.4

- **Declared public API (`[reachability] public_api`):** `.coderipper.toml` declares the library's `pub` items that the binaries do not call (6 files), so `reachability` stops reporting them as unused. Configuration only; no code change, not breaking.

## 0.27.3

- **`lane-restart start`, `stop`, `specs import`, `specs retire`, `assert-idle` and `approvals` parse their arguments with clap derive (audit group 5, row 6, slice 2):** same method and guarantees as 0.27.2. The old and new parsers were compared over all 111,151 argument sequences of up to 4 tokens from an 18-token pool and agreed on accept/reject and on every parsed value; exit 1 on every refusal, all spellings, last-wins repeats, `--soft` with `--hard` refused, a missing `--reason` still reaching `start::validate_reason`. The refusal TEXT for these is now clap's; `ArgError::MissingValue` and `Conflict` are gone. Still hand-written: `host`, `state`, `attach`, the restart flags, `--help`/`--version`. No API change.

## 0.27.2

- **`lane-restart install`, `ledger` and `status` parse their arguments with clap derive (audit group 5, row 6, first slice):** the three hand-written `match arg.as_str()` loops are `crates/lane-restart/src/cli.rs`. Spellings, last-wins repeats, a flag value taken verbatim even when it starts with `-`, `--quiet-over` refused twice, and exit 1 on every refusal are unchanged: the old and new parsers were compared over all 245,411 argument sequences of up to 4 tokens from a 22-token pool and agreed on accept/reject and on every parsed value. The refusal TEXT for these three is now clap's. `--help`/`--version`, `state`, `host`, `start`, `stop`, `specs`, `assert-idle`, `approvals`, `attach` and the restart flags stay hand-written. No API change.

## 0.27.1

- **`lane-restart install` no longer stops on an earlier `.bak` that is running (PM report 2026-10-19):** installing 0.27.0 was refused with `cannot move the installed exe to lane-restart.exe.bak: Access is denied`, because the `.bak` left by the previous install was still the image of running hosts, and Windows will not replace or delete a running exe (it can only be renamed). The install now deletes an earlier `.bak`; if that fails it renames it to `lane-restart.exe.bak.<unix seconds>` and says so on stderr; only if that fails too is the install refused, before anything changed, naming the file to move by hand. No API change.

## 0.27.0

- **Breaking (pre-1.0): a failed launch names the stage that did not happen, and a host no longer waits on GitHub for long (PM-directed root cause, 2026-10-19):** restart.log had 25 `relaunch FAILED: session never processed its prompt` in 442 restarts, of two kinds (measured 2026-10-08..19). **Late** (10 cases): a new host log appears 0-29 s after the 20 s verdict, because the host loads its approvals (about 7 sequential `gh api` calls, 15 s EACH) before it starts claude; that load took 4-8 s when GitHub answered and 19.5-44.5 s in every late case. **Never** (7 cases, 2026-10-10 to 2026-10-19): no host log and no host process at all, so `wt.exe` returned from the spawn and nothing ran; the cause is not found. Now: (1) `lane-restart host` writes `host-boot-<pid>.txt` (date, then its arguments) as its FIRST act, before argument parsing, approvals or claude (swept after 2 days); (2) a launch that fails with no claude ever seen is `RelaunchError::HostNeverStarted` (no boot record for the role after the spawn) or `ClaudeNeverStarted` (a record, no claude), both still starting `relaunch FAILED:`; a claude that was seen keeps `SessionNeverProcessedPrompt`; `StateReader::host_started` (default `None`, which keeps the old failure) feeds it; (3) after `wt.exe` is spawned the launch waits up to 2 s for the stub and appends how it ended (`exit N after X ms`, or `still running`) to `spawn.log` in the state directory, which is where the next Never case will show whether `wt.exe` failed; (4) `approvals::GhReader` has a `budget` (8 s for ALL its calls, `approvals::LOAD_BUDGET`) and the host and `lane-restart approvals` read through `approvals::CachingReader`, which keeps directory listings and files by COMMIT SHA (immutable content; every cached byte is validated as if fetched; a miss, a corrupt entry or an unwritable cache falls back to `gh`), so a host's load is normally the two branch-tip calls. Not done: the cause of a Never case, and a retry of the spawn.

## 0.26.0

- **Breaking (pre-1.0): `LaunchSpec.role` and `.name` (and `SpecFile.role` and `.name`) are `lane_state::role::Role` (audit group 5, finding 7):** both were `String`, checked with `valid_identifier` in `prepare_launch` and `spec::build` after the value was already held. A `LaunchSpec` now cannot hold anything but a plain identifier, so those two checks are gone and the refusal happens where the value enters: `From<&LaneState>` became `TryFrom<&LaneState>` (error `RelaunchError::InvalidIdentifier`, as before), so a restart, `lane-stop` or `specs import` of a state with a bad role or name is refused before anything is written or killed, and a `<role>.launch.json` whose `role` or `name` is not an identifier no longer parses (it reads as an invalid spec). `relaunch::claude_argv` returns `Result` for the same reason. On disk the strings are unchanged. Not done: `StopReason` (free text from `--reason`).

## 0.25.0

- **Breaking (pre-1.0): `lane_state::permission_mode::PermissionMode` replaces the permission-mode strings (audit group 5, finding 7):** `LaneState`, `HookInput`, `CliFlags`, `LaunchSpec`, `SpecFile`, `AgentRecord`, the list row and the plan `Entry` carried `permission_mode`/`mode` as `Option<String>`, and `plan.rs` and `summary.rs` each kept their own copy of the known-mode list. It is now `Default`, `AcceptEdits`, `Auto`, `BypassPermissions` or `Other(String)`, with `as_str()`, `is_known()` and `rank()`; the one list lives in the type. On disk and in hook payloads it is the same plain string, and a spelling this build does not know reads as `Other` (kept verbatim, still flagged `mode-needs-a-person`, still ranked as widened), never refused and never guessed. `START_PERMISSION_MODE` is a `PermissionMode`. Behaviour unchanged. Also lists `failed_relaunch` in `lane-restart status --help`. Not done: `StopReason` (free text from `--reason`) and `LaunchSpec.role`/`name` -> `Role`.

## 0.24.0

- **Breaking (pre-1.0): `SpecState::FailedRelaunch` and `Lifecycle::FailedRelaunch` (a failed restart is retryable):** when `lane-restart` stopped a lane and the new session never came up (`relaunch FAILED`, or the spawn itself failed), the spec stayed `stopping` with no `stop_kind`: the launcher refuses that as `stop-unclassified`, and `status` read it as `crashed`. The restart now writes `failed_relaunch` (best effort; the launch error is still what is returned). The launcher treats it like a crash, so no stop kind is needed and `lane-restart start` / a message / `agentlife restore` is the retry; `status` reports `failed_relaunch`, startable. The enum variants are the break (exhaustive matches). Not done: no automatic second spawn inside the restart, because a claude that is merely late would then run twice.

## 0.23.0

- **Breaking (pre-1.0): `LivenessTiming.appear_timeout`; a slow-starting lane is no longer declared dead (PM finding 2026-10-19):** `lane-restart --self` gave up on a relaunch when no claude process had appeared 20 s after the spawn (`progress_timeout`), logged `relaunch FAILED: session never processed its prompt` and left `launch.json` at `stopping`. On 2026-10-18 unpopped, synapse and lightbulb failed exactly 20-23 s after the spec was written with no host log for the new session; thinkersjournal-community's claude started 8 s AFTER its verdict (host 08:41:56, claude 08:42:24, verdict 08:42:16 local) and the lane came up. Now a process not yet seen is waited for up to `appear_timeout` (default 120 s) before the failure; once seen, its disappearance still fails at once. `total_timeout` (15 min) is unchanged. The new field is the break (struct literals). Not found: why claude starts that late (`wt.exe` and conhost start, load); unpopped's host log shows no new session at all, so a lane that never starts still fails, now after 120 s, and the `stopping` spec still reads as `crashed`.

## 0.22.0

- **Breaking (pre-1.0): `SpecFile.written_by` is `spec::WrittenBy`, and the launch-arg allow-list is `LaunchFlag` (audit group 5, finding 7):** `written_by` was a free `String` (`SpecInputs.written_by` a `&str`); it is now `Restart`, `Launch`, `Stop`, `Import`, `Unpark` or `Other(String)`. On disk it is the same plain string, and a spelling this build does not know (an older or newer build's) reads as `Other` rather than refusing the file. The launch-arg allow-list was a `(&str, FlagArity)` table; it is `LaunchFlag { DevChannels, AddDir, McpConfig, Settings }` with `token()`, `arity()` and an exact-match `from_token()`, so an allowed flag cannot lack a token or arity. Behaviour unchanged. Also fixes a regression of 0.21.0: an error in `config.json` names the offending key again (`peers_addr: ...`). Not done: `stop_reason` is free text from `--reason`, and `permission_mode` crosses hook payloads and the mode lists in `plan.rs` and `summary.rs`.

## 0.21.0

- **Breaking (pre-1.0): `config::Layer` fields are typed, and the loopback rule is a type (audit group 5, findings 5 and 4b):** the enum-valued keys (`approval_default_duration`, `pending_nag`, `idle_stop_policy`) were `Option<String>` parsed later by `typed()` in `resolve`; the address keys were parsed and loopback-checked by two copies of the same code with different messages. Now `Layer` holds `DurationChoice`, `PendingNag`, `IdleStopPolicy`, `ProgramName`, `DirName` and the new `LoopbackAddr` (`peers_addr`, `synapse_addr`, also the `Config` field types; deref to `SocketAddr`), and `resolve` only picks. A bad value is refused where it enters: `Layer::set` (flag), `from_env_with` (error names the `AGENTLIFE_*` variable) or the config file (`ConfigError::File`; serde's message does not name the key, only the offending value). `resolve` no longer sees a malformed typed value. The two loopback messages are now one. Not changed: `pin_roles` and `synapse_account` stay `String` (checked in `resolve`).

## 0.20.0

- **Breaking (pre-1.0): `Config` program and directory fields are typed (audit group 5, finding 4):** `host_program` and `claude_program` are `config::ProgramName`, `portfolio_root` and `lane_state_dir` are `config::DirName`. The rule (non-empty, no `;` which `wt.exe` reads as its command separator, and for programs no control character) was an inline closure at the one parse site; `new` now carries it and the types deref to `str`. Behaviour unchanged. `launch::Programs` stays `String`.

## 0.19.1

- **runner-lint: `RunnerLabel` enum (audit group 5, finding 8):** the `RUNNER_LABELS` string slice is `enum RunnerLabel { SelfHosted, Gpu }` with a case-insensitive `parse`. Behaviour unchanged; additive, internal.

## 0.19.0

- **Breaking (pre-1.0): "never Opus" is a type, and it fails closed (audit group 5, finding 3):** the rule was `is_opus` (a substring test) plus an `allow_opus: bool` threaded through `prepare_launch`, `resolve_launch_model`, `launch_and_wait_with` and `kill_and_relaunch_*`; a new Opus id without "opus" in it passed, and any caller could pass `true`. Now `relaunch::Model` is built only by `Model::parse(name, Option<&OpusPermit>)`: only `sonnet`, `haiku`, `claude-sonnet-*` and `claude-haiku-*` (an optional `[...]` suffix ignored, and never any name containing "opus") parse without a permit; every other name, including future tiers such as `claude-fable-*`, is refused as `RelaunchError::ModelRefused` (replaces `OpusRefused`). `OpusPermit` has a private field and one constructor, `from_env()` (`LANE_RESTART_ALLOW_OPUS=1`), which is `pub(crate)` and called only by `launch_and_wait` and the interactive relaunch path: the launcher service, `lane-start`, spec import, the spec check and the stop path take `None`/no permit and cannot obtain one. `lane_stop` reads the recorded model with `recorded_launch_model` (plain text, no validation, nothing launches from it). `allow_opus_from_env` and `resolve_*(..., bool)` are gone. The environment variable remains an honest human gate: any process can set it, and a type cannot stop that.

## 0.18.0

- **Breaking (pre-1.0): the `wt.exe` command line is checked as a whole, by type (audit group 5, finding 2b):** `wt.exe` reads `;` as its own command separator. `first_unsafe_argument` ran on the working directory and the claude argv in `prepare_launch`, but `spawn_launch(&LaunchSpec, &[String])` was public and unchecked, and `host_wrapped_argv` prepended `lane-restart.exe`'s own path AFTER the check, so a `;` in that path reached `wt.exe` unchecked. Now `relaunch::HostedArgv` is built only by `HostedArgv::new(cwd, role, claude_argv)`, which wraps the argv and refuses a `;` anywhere in cwd or the final line; `spawn_launch`, `spawn_relaunch`, `SpawnFn` and `PreparedLaunch.argv` take/hold it (it derefs to the claude argv). `first_unsafe_argument` stays public. Not done: `LaunchSpec.role`/`name` stay `String` (checked by `valid_identifier` in `prepare_launch`); retyping them to `Role` touches ~100 test constructions and is a separate, additive change. Lane-restart/lane-state/launcher tests pass locally; the root crate's `hook_e2e` tests fail here only because this shell runs under a claude process ("another claude is above this one"), so CI is the read for them.

## 0.17.1

- **Audit group 5, finding 2a: one definition of a role (`lane_state::role`):** eleven hand-copied checks of `[A-Za-z0-9_-]{1,64}` (`relaunch::valid_identifier`, `launcher::valid_role`, `src/config.rs`, `src/launch.rs`, `AgentId::new`, `readiness.rs`, `run.rs`) now call `lane_state::role::is_valid`, and `Role` is a validated newtype (`new`/`FromStr`/`TryFrom<String>`/`Deserialize` all check). Additive; `launcher` gains a `lane-state` dependency. Not changed: `pending::check_id` (a request id: no length cap, a different rule) and `signal_names_role` (a signal id, allows `.` and `:`). `LaunchSpec.role`/`name` become `Role` in the 0.18.0 `WtArgv` change, which reshapes that struct anyway. UNMEASURED until built: written during a disk hold.

## 0.17.0

- **Audit group 5, finding 1: an unchecked handler cannot be constructed (`lane-restart` `handlers`):** `HandlerSpec` used to be a public, directly deserializable struct, and `refusal_reason` was a check callers had to remember. Reproduced: JSON with a bad id, no anchors and empty provenance deserialized into a `HandlerSpec`, and `check_and_inject` wrote its action on any screen (production was safe only because `approvals.rs` happened to call `parse_and_validate`). Now `RawHandlerSpec` is the unchecked shape, `HandlerSpec` is a private-inner newtype reachable only through `TryFrom<RawHandlerSpec>` / `Deserialize` (`serde(try_from)`), read via `Deref`, and `refusal_reason` is private. Breaking for library consumers that built `HandlerSpec` literals or mutated its fields; none in this workspace.

## 0.16.22

- **Docs: runner spike run 2 recorded (`docs/RUNNER-SETUP.md` 5a, 5, 2d):** as `gh-runner` in a Windows service (session 0, not interactive) the 4070 is visible to `nvidia-smi`, both GPUs to Vulkan, a CUDA kernel runs, the protected paths are not writable, and the MSVC step and the execution-policy `.env` worked. Decision rule: keep the service. Reboot/logoff behaviour is UNMEASURED.

## 0.16.21

- **Docs: the standard MSVC-environment step for Windows runner jobs (RUNNER-SETUP 2d, `docs/spike/runner-spike.yml`):** the probe's CUDA step failed with `Cannot find compiler 'cl.exe' in PATH` (the service has no developer environment). The step finds Visual Studio with vswhere, runs `VsDevCmd.bat -arch=x64`, exports the env to later steps and fails loudly naming the missing component. UNMEASURED until the probe is rerun.

## 0.16.20

- **Fix: `gh-runner-setup.ps1 -Apply` writes `PSExecutionPolicyPreference=Bypass` to the runner's `.env`:** spike run 1 died on the Restricted execution policy at every `shell: powershell` step. Process scope, that runner only; execution policy is not a security boundary here (RUNNER-SETUP 5a). UNMEASURED until the spike is rerun. An existing runner needs `-Remove` first.

## 0.16.19

- **Docs: runner spike run 1 recorded (`docs/RUNNER-SETUP.md` 5a):** setup and registration worked; every `shell: powershell` step failed on the Restricted execution policy, so nothing was measured. Options a-d and a recommendation are listed; no script change yet.

## 0.16.18

- **M8 phase 3: `agentlife status --peers` (Synapse transport):** one line (or `--json`) per Synapse role: online/offline, last seen, queued/leased mail, cleaned one-line summary. Role == lane, so there is no pid join. Needs `notify_transport=synapse`; otherwise it says to use the broker command. A daemon that cannot be read is an error, never an empty list. `lane-restart status --peers` keeps the broker and, when `notify_transport=synapse`, prints a one-line "moved: run `agentlife status --peers`" instead of querying a broker that is going away (the published `lane-restart` crate takes no Synapse dependency). The `/v1/list` entry shape (`global_id`, `online`, `last_seen`, `pending`) was seen on the scratch daemon of 2026-10-17; `summary` and `cwd` are read as optional and were null there. Run against a scratch daemon (rc.101) with a fake home: the Synapse table and `--json`, the lane-restart "moved" line, and the broker-config refusal all behaved as described; a lane that sets a `summary` was not exercised.

## 0.16.17

- **Fix: Synapse sends were addressed by bare role, which a real daemon refuses:** `/v1/send` wants `role@account`; a bare `pm` is `404 unknown_recipient` (measured 2026-10-17 on a scratch `synapsed` rc.101; `synapsectl send` qualifies the name itself, which hid it). The 0.16.15 launcher relay and alarms and the 0.16.16 `agentlife down` send were wrong as merged (default transport is still `broker`, so nothing live was affected). Now the account is read from the home's keystore and the recipient is qualified (an address with `@` is kept). Re-measured on the scratch daemon: a launcher send, a `down` send and a relayed outbox message all reached `pm`; an unknown role is still 404. The scratch recipe is in `docs/LAUNCHER-SERVICE.md` ("Scratch-daemon check"); not in CI, it needs `synapsed`.

## 0.16.16

- **M8 phase 2: `agentlife down` and the logon-restore probe can leave the claude-peers broker (same `notify_transport` switch, default `broker`):** with `synapse`, `down` asks the lane to wrap up through the Synapse daemon as role `agentlife-cli` (claimed per run), addressed by the lane's spec role and only if that role is online (`/v1/list`), instead of joining a broker peer by process parent. `Messenger::recipients` carries the difference (default = the old join, so the broker path is unchanged). The restore wait probes the daemon's address from `<SYNAPSE_HOME>/synapsed.json` (never starting one). `down`'s messages no longer say "claude-peers". **Needs at T-0:** role `agentlife-cli` with `send`, and the target role must have claimed once. **Not run:** a real claim/send against a daemon. Still on the broker: `lane-restart status --peers` (`notify::list_all`).

## 0.16.15

- **M8 phase 1: the notify path can leave the claude-peers broker (dual backend, default unchanged):** `notify_transport` in `~/.overmind/lane-restart.json` (`broker` default, or `synapse`). With `synapse`, a host's unmatched-dialog [ASK] is queued in `.lane-state/notify-outbox/` (temp file + rename, fixed `message_id`) instead of sent, and `agentlife launcher` relays the queue through the Synapse daemon as its own `agentlife-launcher` claim (`/v1/send` to role `pm`; a refused message stays queued, junk is renamed `*.bad`), and sends its stall alarms the same way. No per-host claims. `synapse` and `synapsed-client` pinned `=6.0.0-rc.101` (latest published). **Not run:** a real send/claim against a daemon (scratch-home test pending the recipe from the synapse lane), and `status --peers` and `src/peers.rs` still use the broker (next PR, 0.16.16). Unit tests: outbox order/retry/quarantine, transport parsing, send body, config fallback.

## 0.16.13

- **CI: `cargo clippy --locked` (coderipper SUP-001, Q14):** the clippy step now refuses to resolve past `Cargo.lock`. Build and test steps are unchanged.

## 0.16.12

- **`gh-runner-setup.ps1 -Apply` died creating the account; fixed:** the real spike run (2026-10-17) failed at `New-LocalUser`: `Description` was 61 characters, the limit is 48. Nothing was created. The dry run never reaches that cmdlet. Description shortened; red-first test (failed at length 61) now reads every `New-LocalUser` call from the script's AST and holds each literal `-Description` (48), `-FullName` (256) and `-Name` (20) to its limit, plus the default `-Account`. Still UNMEASURED: the rest of the `-Apply` path.

## 0.16.11

- **Runner spike workflow written (`docs/spike/runner-spike.yml`, docs/RUNNER-SETUP.md 5a):** session id, protected-path write
  attempts, `nvidia-smi`, `vulkaninfo --summary` and a real CUDA kernel launch, on `[self-hosted, windows, gpu]`, dispatch and
  push-to-main only. `runner-lint` reports 0 findings; the script's dry run for `ciresnave/runner-spike` is clean. **Not run yet:**
  the GPU-from-a-service question stays UNMEASURED until the PM registers the runner. Also documented: `-Apply` needs a
  `GH_TOKEN` that can see a private repo, since `ciresnave-bot` gets 404 and the visibility check throws.

## 0.16.10

- **`gh-runner-setup.ps1`: private repos default to a persistent runner (board 223, ruled):** CireSnave, 2026-10-16: "Go with your
  recommendations." With `-Ephemeral` unset the runner is persistent when every `-Repo` is named in `-Private`, ephemeral otherwise
  (a public or undeclared repo in the call makes the whole call ephemeral). An explicit `-Ephemeral` still overrides, and
  `-Ephemeral:$false` stays refused for a public repo. The dry run prints the mode and why. `docs/RUNNER-SETUP.md` records the ruling
  and the Humboldt exception. Registration still waits on board 224 (ACL); nothing was registered or run elevated. Tested by the dry
  run only (5 new cases in `scripts/tests/gh-runner-setup-acl.tests.ps1`, one mutation caught).

## 0.16.9

- **`lane-restart assert-idle` (and the state hook) walk up to 8 shell layers, not 4:** `lane-restart assert-idle: no claude
  process found within 4 shell hops` was raised in the coderipper lane by `(lane-restart assert-idle || lane-restart.exe
  assert-idle) | tail -5` from the Bash tool: the subshell-in-a-pipeline adds a fifth bash layer above the exe (on this box a
  plain call already sits under three). Reproduced in another lane with the same command; the plain call and `| tail` alone
  pass. Red-first test `walks_a_subshell_inside_a_pipeline_five_shells_deep` fails at 4. A stranger image before `claude` is
  still refused at once, so only depth is loosened; the exceeded-limit test now follows the constant.
- **Not a defect:** a PM dry run reading `state file is Ns old, refusing as stale` on an *idle* lane. `STALE_STATE_AFTER` is
  120 s and hooks write only on events, so an idle lane is always stale; the lane's own `--self` restart fires a hook first.
  (coderipper: no transcript events after its 20:19:58Z compaction = the state file's last write.) The refusal text now says so
  and names the `assert-idle` then `--self` path; the 120 s rule itself is unchanged, since a stale read must not pass as idle.

## 0.16.8

- **`lane-restart` `on_unhandled` tests start their window at the right moment (#113, OverMind):** a flaky test window
  (one failure in 160 iterations of `a_blocked`, message not captured, cause NOT identified) is hardened. Soak after the
  change: 0 failures in 520 iterations (2080 test runs) under CPU load; combined 1 in 680. No red-first test exists for the
  original failure, so this is a hardening, not a proven fix.

## 0.16.7

- **Test diagnostics, no behaviour change:** every `launch_e2e` failure message that prints the restore report now also prints the
  hook log. The same signature (one lane of several launched together: `no session appeared within 40 s`) failed a second,
  different test on the Windows CI leg (`twelve_real_stand_ins...`, run 38940546634, lane `l09` of a batch of five); the cause is
  still NOT found. A stress test of 12 threads writing distinct files in one directory through `atomic::write_atomic` (a suspected
  race in its shared scratch directory) did not fail in 3 runs, so that is not shown.
  The stand-in now also logs `start`, `hook-begin` and `hook-end` with epoch-millisecond timestamps, and the failure message lists
  each agent's session `started_at`, so the next occurrence says whether a lane launched late, started late, had a slow or failed hook,
  or registered and was not seen.

## 0.16.6

- **`lane-restart`: handlers gain `match.isolated_lines`, and a folder-trust record template:** like `match.lines`, and the row
  after the pinned row must be blank, so a trusted path exactly one column short of the screen width followed by a wrapped
  suffix cannot pass as the trusted path. `MatchSpec` gains the field; `MatchReport.lines` also lists isolated pins.
  New `docs/TRUST-DIALOG.md` (the captured dialog, the host PTY width: the console window size, 80 x 25 fallback, resized
  with the terminal) and `approval-example/trust-folder-agentlife.json` (inert template and test fixture).

## 0.16.5

- **Launch names are validated at record time (`lane-restart`):** a launch name that is not a safe token is
  recorded as unnamed instead of stored (OverMind's `name-validate-at-record`, #115). A later event never fills in
  an unsafe name, and a fresh launch replaces an older safe name with none.

## 0.16.4

- **Runner policy for private repos (docs, script, `runner-lint`):** every private repo uses the runner
  (CireSnave 2026-10-16). `runner-lint --private` lints as a private repo (a pull-request runner job needs the
  same-repo `if` but no `environment:`) and exits 2 unless `gh api` says the repo is private. `gh-runner-setup.ps1`
  no longer labels every runner `gpu`: `-Labels` defaults to none. `docs/RUNNER-SETUP.md` section 8a lists, per
  private repo and job, what must change for a plain Windows runner, and 2b gives the persistent and ephemeral
  variants as a one-line switch.
- **`gh-runner-setup.ps1` refuses to create the runner account while the lane tools are writable by it (board 224):**
  `-ProtectedPaths` (`C:\Projects`, `.claude-hooks`, `with-secret.exe`, `lane-restart.exe`, `.lane-state`) must grant no write
  right to Everyone, Authenticated Users, Users, Interactive or the account. New `-CheckAclsOnly`; the exact elevated `icacls` fix is
  printed, never run, and `-AlsoGrant <group>` keeps read-and-execute (never write) for a named group in it. This machine fails
  the check today. Tested by `tests/runner_setup_acl.rs` (Windows).

## 0.16.3

- **Test diagnostics, no behaviour change:** the `fake_claude` stand-in keeps each hook's exit code, time and stderr in
  `FAKE_HOOKLOG`, and `launch_e2e` prints it when a lane does not come up. A Windows CI run (2026-10-16, run
  38921949112) had one of two lanes launched together never register a session in 40 s, the rerun was clean, and
  nothing recorded why; the cause is NOT found.

## 0.16.2

- **Runner policy, widened (docs, script, new unpublished crate):** `docs/RUNNER-SETUP.md` records the policy
  (device tests on push/schedule/dispatch; a same-repo PR only behind the same-repo condition AND an environment;
  never a fork PR). `scripts/gh-runner-setup.ps1` now accepts any repo owned by `ciresnave` or `ThinkersJournal`
  (`-Owners`), treats a repo as public unless named in `-Private` (verified with `gh api`), and the 3-name
  `-TrustedRepos` default is gone. New crate `runner-lint` (`publish = false`) fails a workflow job that can run on a
  self-hosted runner and is reachable from a pull request or an unlisted trigger without both gates.

## 0.16.1

- **`lane-restart`: handlers gain `match.lines`:** each pin must equal one whole screen line (trimmed), so a folder
  pin cannot match a longer path (`C:\Projects\agentlife-evil`) as a text anchor would. A pin shorter than 8
  characters, spanning lines or naming a secret is refused at load; a wrapped line fails closed. `MatchSpec` and
  `MatchReport` gain a `lines` field.

## 0.16.0

- **Breaking for handler authors and API users (`lane-restart`):** a handler `action` is now a closed keystroke
  grammar: at most 3 tokens, at most one letter or digit, `\r`, `<Enter>`, `<Down>`; no raw ESC, no free text. The
  host sends `ESC [ B` for `<Down>` itself (`handlers::action_bytes`). `action_is_a_plain_keystroke` is replaced
  by `action_is_allowed` / `parse_action`. `"\r\r"` was refused before and is accepted now.

## 0.15.19

- **Script and docs, no code change:** `scripts/gh-runner-setup.ps1` and `docs/RUNNER-SETUP.md`, self-hosted GitHub
  runner setup (dry run by default; ephemeral runners; trusted repos only). Nothing registers a runner by itself.

## 0.15.18

- **Script and docs, no code change:** `scripts/synapsed-task.ps1`, the per-user logon task that starts the Synapse
  daemon with a fixed environment (dry run by default; not registered by anything). See `docs/TASKS.md`.

## 0.15.17

- **Dependency bump, no behaviour change:** `synapsed-client` and `synapse` 6.0.0-rc.85 -> rc.89 (the
  launch-result vocabulary is pinned in the client; the workspace is now rsa-free).

## 0.15.16

- **Test fix, no behaviour change:** `peers::tests::a_broker_that_is_not_there_is_an_error` bound a port,
  dropped it, and asserted the error text named the broker. Windows reports a closed-port connect as a 10060
  timeout, and nothing stops a parallel test's listener from taking the freed port, which would answer
  with a different error. The cause was not reproduced; the test now asserts only that an error has
  a message, and passes without asserting if a parallel listener answered.

## 0.15.15

- **Test fix, no behaviour change:** `launch::tests::a_direct_spawn_runs_the_hosted_command_in_the_tabs_directory`
  (Unix) waited for `role.txt` to exist, but the shell creates it before writing, so it could read an empty
  file (seen once on CI at #103, passed on rerun). It now waits for the whole expected text. Only a Linux run
  exercises it.
- **launch-result outcome vocabulary settled (Synapse lane, 2026-10-16):** the daemon accepts exactly
  `started | already-running | refused | failed` (anything else is a 400); `launching`/`unreachable` are its
  own list states and are never sent, so the OPEN note under 0.15.14 is closed: our `Outcome` already matches.
  The mapping is now `launcher_service::outcome_wire`, with a test that each of our outcomes is one of the four
  (mutation-checked); `dry-run` is never posted.

## 0.15.14

- **`agentlife launcher`: the launch-on-message service, notifier-only build** (`docs/LAUNCHER-SERVICE.md`,
  build order step 1). Long-polls Synapse as role `agentlife-launcher` through `synapsed-client` 6.0.0-rc.85
  (`waiting`, `launch_result`, `launch_clear`, and `/v1/list` for the R7 online observation), decides each
  signal with the `launcher` engine, posts every result (refusals included) from an outbox, re-checks
  conditional refusals with `launch-clear`, and sends stall alarms to the `pm` lane over claude-peers. It
  **never starts a lane**: with no standing grant (M5 unbuilt, board 216) a launchable signal is answered
  `refused: no-grant`, and the mechanism it hands the engine always fails closed. One instance (OS advisory
  lock `<state>/launcher.lock`); the caller record `<state>/agentlife-launcher.launcher-caller` is written only
  while a session is held and removed only if it still names this process; the store is never created on its
  own (`--init-store` once). `--once` runs a single cycle. Not run against a real daemon, lane or logon: unit
  tests use stand-ins and the wire shapes were read from Synapse's own tests at 3230c03.
- **Not in this change:** the second Task Scheduler task (`install-task` does not register the service yet, so it
  is run by hand) and the grant read (rule 7). The per-role start lock (0.15.11), `stop_kind` (0.15.10-12) and
  the launcher path's `restore.lock`/caps/floor (0.15.13) are in `main`.
- **Review fixes (Sourcery, 8 of 10 threads):** a launch spec that exists but cannot be used (corrupt,
  incomplete, another role's) now fails closed (`closed-on-purpose`) instead of reading as launchable; a role's
  intent is its newest registry record's, so an old parked agent no longer blocks a role that is wanted now;
  a `/v1/waiting` entry without `epoch` no longer parses (it was answered with a blank epoch) and any
  unreadable entry is named in the service log every poll; a stall alarm is marked raised only after it
  reached the PM (`Engine::pending_alarms` + `mark_alarmed`; `alarms()` kept) so a failed delivery is retried;
  `Engine::recheck` returns `Result` and the service does not send `launch-clear` unless the `Clearing`
  mark saved; repeated poll trouble backs off 5/10/20/40/60 s and resets on a clean cycle; the caller record
  is written to the state directory `lane-restart` reads (the platform one), not `lane_state_dir`.
  `recheck` returning `Result` and `Signal.epoch` being required are source-incompatible for the (unpublished)
  `launcher` crate. Answered with a reason, no change: conditional refusals are re-posted (the policy
  re-evaluates a stored refusal after `launch-clear`), and no duplicate-result mapping (the client documents
  only `stale_signal` for this endpoint).
- **OPEN, found in review, not fixed here:** `synapsed-client`'s `launch_result` documents the outcomes
  `launching`, `failed`, `refused`, `unreachable`, while `Outcome::Started`/`AlreadyRunning` serialise as
  `started`/`already-running`. Notifier-only never posts those two, so nothing breaks today; it must be
  settled with the Synapse lane before the mechanism is switched on.
- **Dependency tree, on the lean client (rc.85):** `synapse` is taken with `default-features = false,
  features = ["native-base"]` (the same features `synapsed-client` uses; the default features bring
  `jsonwebtoken` and so `rsa`, RUSTSEC-2023-0071; `cargo tree -i rsa` now matches nothing). **`deny.toml` (PM
  ruling 2026-10-16):** allows BSD-3-Clause, ISC, Zlib, 0BSD, CC0-1.0 and CDLA-Permissive-2.0 globally; MPL-2.0
  only through a `[[licenses.exceptions]]` entry for `attohttpc` 0.16.3, the one crate that carries it
  (unmodified dependency, file-level copyleft, notices kept). `cargo deny check`: advisories, bans, licences
  and sources ok.

## 0.15.13

- **The launcher path of `lane-restart start` holds `restore.lock`, the caps and the memory floor**
  (PM task 2026-10-15; OverMind `docs/LANE-LAUNCH-POLICY.md` @e4c6ae3 sections 2-3). For a launcher
  caller only (the PM's own `start` is unchanged), after every spec refusal and before anything spawns:
  1. **`restore.lock`**, the file `agentlife restore` takes (`<agentlife home>/restore.lock`, body
     `<pid> <start secs>`), held to the end of the start. Held by a live owner: `failed` /
     `lock-held`; unreadable: `failed` / `restore-lock-unavailable`. Two launcher starts of different
     roles therefore serialise (the second is `lock-held` and Synapse retries it), and so do a launcher
     start and a restore. A test in the root crate proves each side sees the other's file.
  2. **Caps**, counted from `restart.log`: spawns by launcher callers in the last hour, 3 per role and
     12 overall (`refused` / `rate`), and 8 per role in the last day (`refused` / `rate-daily`: **the
     daily number is this lane's proposal; the policy names none**). Only a row where a spawn was
     attempted counts, so refusals, `already-running`, `held-for-memory`, `lock-held` and dry runs never
     consume budget; the check runs under `restore.lock`, so two starts cannot both read 11. A ledger
     that is unreadable, or has a line that is not an entry, is `failed` / `store-unavailable` (fail
     closed: a spawn row might be hiding in it; `lane-restart ledger` names the line).
  3. **The memory floor** `agentlife restore` holds: `AGENTLIFE_FREE_RAM_FLOOR_GB`, else
     `free_ram_floor_gb` in `<agentlife home>/config.json` (`AGENTLIFE_HOME` honoured), else 8. Below it:
     `failed` / `held-for-memory`; a floor that cannot be read or is out of range: `failed` /
     `config-unreadable`. Unmeasurable memory passes, as it does in `restore`.
  Each `start` ledger line now also carries `signal_id`, `result` (`<outcome>:<reason>`, vocabulary A)
  and `spawned` (new optional `log::Entry` / `ledger::Row` fields: a struct-literal `Entry` in external
  code needs the three new fields). New `StartRefusal` variants `RateCap`, `HeldForMemory`,
  `RestoreLockHeld`, `GateUnavailable`. New `lane_restart::launcher_gate`. Vocabulary A reasons added:
  `rate`, `rate-daily`, `held-for-memory`, `lock-held`, `store-unavailable`, `config-unreadable`,
  `restore-lock-unavailable`. Unit-tested and each guard mutation-tested; **not run against a real lane,
  and `run_start_cmd`'s wiring is not unit-tested**. `--dry-run` takes no lock and applies no cap or floor.

## 0.15.12

- **The launcher starts only a soft stop or a crash** (board 216, "216: yes"). For a launcher caller,
  `lane-restart start` now reads the spec's `stop_kind`: a `suspended`/`stopping` spec must be `soft`;
  `hard` is refused `stop-hard` and no kind `stop-unclassified` (both `refused`, exit 1, terminal); a
  `crashed` spec, or `running` with its process gone, is a crash and is started without a mark. The PM's
  `start` is unchanged. The spec is the authority, not the agent registry: `agentlife stop --soft` leaves
  registry intent `Closed{exited}` and writes `soft` to the spec, `park` writes `hard`. A launch now
  clears `stop_kind` (it is not a stop), so a lane the PM restarts after a hard stop is not still
  marked hard when it later crashes. New `StartRefusal::StopNotSoft`. The 0.15.11 lock gains a test
  with two real processes (a re-run of the test binary holds the lock; the parent is refused until the
  child exits). Not run against a real lane. Still not built: the memory floor and `restore.lock` on the
  launcher path, the per-role daily cap and the ledger line for launcher starts.

## 0.15.11

- **`lane-restart start` takes a per-role start lock** (`docs/LAUNCHER-SERVICE.md`, "Two starters at once";
  the mechanism requirement before the launcher may start lanes unattended). `<state>/<role>.start.lock` is an
  OS advisory lock (`File::try_lock`), taken before `plan_start` and held through the spawn and the
  confirmed-live wait, so `role_status` and `duplicate::check` are the re-check after the lock. A second
  starter of the same role gets `already-running` (`--json` reason `start-lock-held`, exit 3, the contract it
  already had); a lock that cannot be created or taken is `failed` / `start-lock-unavailable` and launches
  nothing. A role that is not an identifier takes no lock and is refused as before; `--dry-run` takes none.
  Different roles never contend; the OS releases the lock on a crash, so there is no stale file. New
  `lane_restart::start_lock` module and `start::take_start_lock`; new `StartRefusal` variants (a public enum:
  a source-incompatible addition for an exhaustive external `match`). Unit-tested, each guard
  mutation-tested; not run against a real lane or two real starters.

## 0.15.10

- **Every stop records whether it is soft or hard** (board 216, PM task 2026-10-15; CireSnave verbatim:
  "216: yes"). New `stop_kind` (`soft` | `hard`) in the launch spec, written on both the `stopping` and the
  `suspended` write. **Hard** is the default and means a person or the PM stopped the lane on purpose: no
  launcher may undo it. **Soft** means the lane ended in the course of its own work. `lane-restart stop` takes
  `--soft` / `--hard` (both is an error); `--self` is soft and `--self --hard` is refused ("a lane cannot
  hard-stop itself"). `agentlife stop` takes `--soft` (a person only; it applies to ending a RUNNING session,
  and `--soft` on a not-running agent is refused rather than ignored); `agentlife park` is always hard. A spec
  with no `stop_kind` (written by an older build, by a launch, or by `specs import`) is **unclassified**; the
  launcher built on top of this never starts it. `lane-restart status --json` shows `stop_kind`. Nothing
  reads the kind yet: this is the writer half, so a launcher change can follow without a migration. The
  field is additive (schema stays 1); an older build that rewrites a spec drops it, which reads as
  unclassified, the safe side. Unit-tested on both write paths, the `--self` refusal and the park rule, with
  each guard mutation-tested; not run against a real lane.

## 0.15.8

- **`launcher` crate: `epoch` is a string.** Synapse's signal carries `epoch` as an opaque string and
  `/v1/launch-result` requires it back (`LaunchResultBody.epoch: String`, Synapse 3230c03); the engine modelled
  it as a number, so a real signal would not have parsed. `Signal.epoch`, the stored request rows and
  `LaunchResult` now carry it as a string and echo it verbatim. `Engine::mark_posted` takes the result and
  matches the full `(signal_id, attempt, epoch)` key, so another epoch's answer acknowledges nothing. Breaking
  for the unpublished `launcher` crate only; a store written by an earlier build (none exist: the service was
  not runnable) fails closed as unreadable. Unit-tested; nothing ran against a real daemon.

## 0.15.7

- **`start` launch-to-live seconds are timed around the launch only.** `lane-restart start --json`
  (`launch_to_live_secs`) and the ledger line used a timer started before the state-dir and reader setup, so
  the figure was an upper bound. `execute_start` now returns `launch_elapsed`, taken immediately around
  `launch_and_wait_with`; the spec write-back and planning are excluded. Unit tests unchanged (they inject a
  fake sleep, so the real elapsed time is about zero); not measured against a real launch.

## 0.15.6

- **`agentlife unpark --no-start` reopens a stopped lane's launch spec.** A `lane-restart stop` or `agentlife
  down` leaves the spec `suspended`; since 0.15.3 `restore` keeps such a lane out of its plan, and
  `Wanted` + `suspended` is the same state as "the PM stopped it", so an unparked lane would never be restored.
  `unpark --no-start` now rewrites a `suspended` or `stopping` spec to `crashed` (the existing "ended without a
  stop" state), journals `spec_reopened`, and leaves a `running`/`crashed` spec or no spec alone. Best effort:
  a failed write leaves the lane refused, the safe side. Also fixes the `install-task` help line that still
  said a non-dry-run `restore` is not built. Found in review of #92; unit-tested over all four spec states and
  the no-spec case, not run against a real lane. Regression from 0.15.3, which kept stopped lanes out of the plan
  without giving a reopen a way back.

## 0.15.5

- **New crate `launcher`: the decision engine of the launch-on-message service** (`docs/LAUNCHER-SERVICE.md`;
  PM task 2026-10-15, OverMind policy #145 @e4c6ae3 sections 2-4). Pure and trait-based: rules 1-7 in order, the
  section 3 mechanism mapping, the replay/rate/no-claim store with intent-before-call crash safety and an outbox,
  the R7 online observation and `launch-stalled` alarm, and the service-side `launch-clear` re-check. **No network,
  no process spawn and no Synapse dependency yet**: the daemon routes are synapse#175 (unbuilt at rc.77). The model
  is never read from a signal. 25 unit tests with stand-in daemon/mechanism/directory; five guards mutated out to
  see tests fail. Not a service yet; not published (`publish = false`).

## 0.15.4

- **The launcher caller is `agentlife-launcher`, not `overmind`** (PM ruling 2026-10-15). Synapse holds one
  session per role and a claim supersedes the other holder, so a service claiming `overmind` would knock the
  OverMind lane off its role. `lane-restart start` now accepts caller role `agentlife-launcher` (record
  `<state>/agentlife-launcher.launcher-caller`); the 0.15.2 name `overmind` and its record are **accepted for
  one more release** and removed after 0.16. A record must be filed under the name it carries. `stop` and a
  restart still refuse both. The caller is recorded as `launcher:<role>` (the `:` cannot appear in a lane's
  role), so a lane that shares the name, such as the OverMind lane, is not mistaken for the service (a
  hole in 0.15.2's gate, found in review of this change). `LAUNCHER_RECORD` stays as a deprecated alias.
  Threat note updated. No exit code or output changes; not run live (the service
  that writes the record does not exist yet).

## 0.15.3

- **`agentlife restore` no longer relaunches a lane that was stopped on purpose.** A `wanted` lane whose launch
  spec is `suspended` (written by `lane-restart stop` and `agentlife down`) is excluded from the plan with reason
  `SuspendedSpec` (routine, counted not listed); `lane-restart start` stays the resume path. Before, only
  `agentlife down` recorded `Closed` intent, so a `lane-restart stop`ped lane was `Wanted`, not running, and a
  restore candidate (found by the launcher design review, #92). A suspended spec that is not complete is also
  reported as suspended rather than falling back to the registry record, as is a `stopping` spec (the final
  `suspended` write failed) and a spec file that only partly reads but says `suspended`; another role's spec
  under a filename is not used. Not run against a real logon; the planner and loader are unit-tested, with the
  exclusion mutated out to see the test fail. **Residual, stated:** restore does not re-read the spec between
  building the plan and spawning, so a stop landing inside that window is still undone (not tested, not closed
  here); a pending restore frozen by 0.15.3 and read by an older exe would not parse the new `SuspendedSpec`
  reason (review finding, not run).

## Unreleased


- **Docs: `docs/LAUNCHER-SERVICE.md`, the design of the launch-on-message service** (`agentlife launcher`; PM task
  2026-10-15). Names what blocks it: `synapsed-client` is a `0.0.0` name reservation on crates.io (no code), the
  M5 grant store and board 216 are unbuilt, and `start-role` on the launcher path does not exist. Nothing built.

- **Docs: the launcher design now states how the two stop paths differ** (`docs/LAUNCHER-SERVICE.md`, Intent):
  `agentlife down` records `Closed` intent, `lane-restart stop` records only the `suspended` spec, and the
  launcher must read both. Code read at 3de10ab; nothing run.

## 0.15.2

- **`lane-restart start` accepts a second caller, role `overmind`, for the launch-on-message service** (PM task
  2026-10-15; authority: CireSnave "Synapse->OverMind->AgentLife launch", relayed by the PM). Identified by a
  record `<state>/overmind.launcher-caller` plus the direct parent's pid and start time, not by a `claude`
  ancestor; `start` only (`stop` and a restart still require the PM, now tested with `overmind` refused); requires
  `--signal-id <role>:<id>` naming the role started, else `refused: no-signal`; each call ledgered with the signal
  id. Threat note: `docs/LAUNCH-ON-MESSAGE-THREAT.md`. No service writes the record yet (PR3); not run live.
  **Stated limit:** the record is a same-user-forgeable local mirror of the Synapse `launcher_roles=overmind`
  credential, which `lane-restart` cannot check; the gate bounds what that identity may do (start existing,
  specced lanes only), not who can claim it. The rate cap lives in the service, not here.

## 0.15.1

- **`lane-restart start --json`** prints one line on stdout, `{"role","outcome","reason"[,"launch_to_live_secs"]}`,
  for a caller that must decide whether to retry (Synapse's `/v1/launch-result`, spec C4). `outcome` is
  `started` (reason `started` or `awaiting-dialog`), `already-running`, `refused` (terminal: `not-pm`, `pm-role`,
  `invalid-role`, `reason`, `model-not-pinned`, `no-spec`, `spec-unusable`) or `failed` (retryable:
  `duplicate-risk`, `process-table-unreadable`, `disk-floor`, `spawn-failed`); `dry-run` for `--dry-run`. Exit codes
  are unchanged (0/1/2/3). A confirmed start also records `launch-to-live:<n>s` (spawn to confirmed progress) in
  the ledger line and the JSON, so a measured `launch_wait` can accrue from real starts. Not run against a real
  lane; the outcome mapping is unit-tested.

## 0.15.0

- **BREAKING (exit code): `lane-restart start` exits 3 when the lane is already running.** Before, that refusal
  exited 1 like every other (no PM caller, no spec, a duplicate-risk process, a failed launch), so a caller that
  only wants "make sure this lane is up" could not tell a harmless no-op from a failure. Now: `0` started with
  progress confirmed, `2` started and awaiting the dev-channels dialog, `3` already running (a live claude matches
  the recorded pid and start time; nothing is launched or written, safe to call again), `1` any other refusal or a
  failed launch. `stop` keeps `4` (stopped, `suspended` mark not written). A `duplicate-risk` refusal stays `1`: a
  stuck or unrecorded session is not a confirmed live lane. Also in `RESTART-TOOL-DESIGN.md` 14.5. Not run against
  a real lane (`start` is PM-only); the mapping is unit-tested (`StartRefusal::exit_code`).

## 0.14.35

- **`agentlife install-task` off Windows prints a `systemd --user` logon unit** (`docs/PORT-LINUX-PLAN.md`, P8).
  Before, it refused outright (Task Scheduler exists only on Windows). Now it prints
  `agentlife-logon.service` (`Type=oneshot`, `ExecStart=<exe> restore --from-logon`, `WantedBy=default.target`, no
  start timeout because a restore waits for a person; `%` and `$` doubled and the path quoted when needed) and the
  `systemctl --user daemon-reload` / `enable` commands. **There is no unlock unit**: systemd has no trigger for a
  session unlock (logind announces it as a D-Bus `Unlock` signal that a listener would have to catch), so on Linux
  `agentlife pending --prompt` is run by hand or from the desktop's own unlock hook; this is printed, not hidden.
  `--register`/`--remove` stay Windows-only: nothing is written into the user's systemd directory and no
  `systemctl` runs. **Not verified against a real systemd.** The Windows path is unchanged.

## 0.14.34

- **`lane-restart attach --role <r> --watch`: a read-only view of a running direct lane host** (`docs/PORT-LINUX-PLAN.md`,
  P6b; approved by the PM 2026-10-14 as the only attach to build). The host serves a unix-domain socket
  `<state>/watch/host-<role>.sock` (the directory is created `0700` before the socket is bound in it). A watcher is
  given the current screen (the host's `vt100` model, as of the chunks relayed so far) and then every chunk the child
  writes, in order. **Nothing flows the other way**: the server has no read path, so the host's own stdin stays the
  only way into the lane, and a watcher's bytes are ignored. A watcher that falls behind (256 chunks) is dropped; the
  relay never waits for one. `--watch` is required: a bare `attach` is refused, because interactive attach is not
  built and not approved (plan row P6c, open: a threat-model note on keeping a same-user agent off the socket is
  owed). Unix only; the host's behaviour with no watcher is what it was, and Windows is unchanged.

## 0.14.33

- **A lane starts as a direct process off Windows** (`docs/PORT-LINUX-PLAN.md`, P6a; CireSnave 2026-10-14, verbatim:
  "Direct process first with tmux support on our roadmap to build later."). No terminal program, no tmux, no
  emulator. Windows is unchanged (`wt.exe`, then `conhost.exe`).
  - `lane-restart` (`start`, restart, `--self`): `relaunch::spawn_launch` runs the host itself through the pure
    `relaunch::direct_command`: the lane's directory, `LANE_ROLE` set to the launched role, the session identity
    removed, stdin the null device, stdout and stderr appended to `host-<role>.out` in the state directory.
  - `agentlife restore`: new `launch::DirectSpawner` and `launch::platform_spawner()` (Windows: `RealSpawner`,
    unchanged); `SpawnVia::Direct` is now the real direct launch, not only a test double.
  - The host makes itself a session leader (`setsid`) when its stdin is not a tty, so a launcher's terminal closing
    does not take every lane it started. A host a person runs in a terminal is left alone.
  - Not in this change: **attach**. Ruled by the PM 2026-10-14: read-only `--watch` is P6b; interactive attach is
    not approved until a threat-model note says how a same-user agent is kept off the socket.

## 0.14.32

- **Unix stop is TERM, grace, KILL to the process group** (`docs/PORT-LINUX-PLAN.md`, P5). `sysinfo`'s `kill()` is
  `SIGKILL` on Linux: no flush, and a lane's shells and helpers were left running. New
  `lane_state::platform::signal::terminate` (unix): `SIGTERM` to the target's process group when it leads one (a
  session leader on a pty does), else to the process alone; wait up to 5 s (`facts::KILL_GRACE`); then `SIGKILL`
  **only if the same process is still there**, asked again immediately before the signal (a pid that changed
  hands during the grace is never signalled; a zombie counts as gone). Used by `SysinfoFacts::kill_verified`
  (`lane-restart` restart and stop, `lane_stop`) and by `agentlife down`. **Behaviour change off Windows:** a stop
  can now take up to 5 s longer when the process ignores `SIGTERM`. Windows is unchanged (one `TerminateProcess`).
  New unix-only dependency `rustix` features `process` in `lane-state`.
- Docs: P9 is recorded as **not verified against a real systemd journal** in the plan and the ROADMAP.

## 0.14.31

- **Shutdown marker on Linux** (`docs/PORT-LINUX-PLAN.md`, P9). The reboot-vs-crash classifier (`intent`) had a
  marker only on Windows (`wevtutil`, events 1074/6006); elsewhere every ambiguous case became a candidate. On Linux
  it now reads the previous boot's closing journal entries (`journalctl --boot=-1 --output=short-unix --reverse`) and
  takes the newest `Shutting down.`, `systemd-shutdown` or `Journal stopped` line at or before this boot began. A
  crash or power cut leaves none, so it reads as no marker, and so does a journal that is not persistent
  (`journalctl` exits non-zero) or a host without systemd: the safe default is unchanged. `pick_journal_shutdown`
  is pure and tested on both OSes. **Not verified against a real journal** (the only Linux available here has no
  systemd); the ubuntu CI leg runs the parser, not `journalctl`.

## 0.14.30

- docs only (PM-allocated). `docs/PORT-LINUX-PLAN.md`: updated for the Q1 (direct host first, tmux later, no
  emulator), Q2 and Q3 rulings; P3 replaced by P3' and P11 marked already built; **the key-file tier is corrected
  from "parity with DPAPI user scope" to weaker than it** (its key sits beside the store and is copied with it;
  from OverMind's audit in `consent` `docs/LINUX-PORT-DESIGN.md`), and a security-key touch is noted as presence,
  not reading. `docs/ROADMAP.md`: a Linux port section with the tmux-viewer row, marked later. No code change.

## 0.14.29

- **Identity-mismatch diagnostic, no change to any decision** (`docs/PORT-LINUX-PLAN.md`, P3'; issue #14 has no
  failing log). New `lane_state::platform::diag::start_readings(pid)`: on Linux, the `/proc/<pid>/stat` start tick
  (boot-relative, so a stepped clock cannot move it), the `btime` `/proc/stat` reports now, and the epoch start they
  give together; off Linux it says it is unavailable. It is appended to the message when a kill is refused because
  the pid's identity no longer matches: `agentlife down` (`DownError::IdentityChanged`), and `lane-restart` restart
  and `stop` (`action failed: ... kill refused: ... (start readings: ...)`). A later clock step then shows in the
  ledger as `proc_start_ticks` stable while `btime` moved. The token redesign (old P3) stays out unless a log shows
  that.

## 0.14.28

- **`lane-restart host` on unix** (`docs/PORT-LINUX-PLAN.md`, slice P4). No change on Windows.
  - A raw-mode guard on the host's stdin (`rustix::termios`): a real tty goes raw (no line buffering or echo, so
    the terminal's cursor-position reply reaches the child) and its original mode is restored on every exit path,
    panic unwind included. A pipe, a file or a test's fake stdin is left alone. This is the unix counterpart of the
    Windows `ENABLE_VIRTUAL_TERMINAL_INPUT` fix.
  - `current_console_size()` reads the tty size (`TIOCGWINSZ`) instead of returning `None`, so the PTY is no longer
    always 80x25 on Linux and resize forwarding fires (the existing 500 ms poll; no SIGWINCH handler yet).
  - The 11 real-PTY host tests that were `cfg(windows)` now run on both OSes with an `sh -c` child on unix; three new
    tests exercise the guard and the size against a real pty. New dependency: `rustix` 1.1.5 (`termios`, `stdio`),
    unix only.

## 0.14.27

- **Linux port, slices P0 to P2** (`docs/PORT-LINUX-PLAN.md`, merged in #78 without a bump; PM-allocated number).
  **No behaviour change on Windows**: every Windows value is the literal it was.
  - **P0, fixed directories per OS.** New `lane_state::platform` (`state_dir`, `agentlife_home`, `hooks_dir`,
    `portfolio_root`, with fallible `try_*` forms and a testable `Env`). Off Windows they resolve under
    `XDG_STATE_HOME` (else `~/.local/state`): `lane-state/`, `agentlife/`; hooks under `~/.local/lib/agentlife/hooks`;
    portfolio root `~/Projects`. Before, off Windows `C:/Projects/.lane-state` was a *relative* directory created under
    the cwd. `lane_restart::state_dir()` and the two other copies in `host.rs` now use it; `Config::resolve`
    re-resolves `portfolio_root` and `lane_state_dir` for this OS (`Config::default()` stays the Windows layout);
    `Home::resolve`, `doctor` (`doctor::hook_exe()`), `worktrees --stale` and `install` use it too. The agentlife
    home is a sibling of the state directory, not inside it (a small change from the plan's wording).
  - **P1, naming.** `windows-sys` moves under `[target.'cfg(windows)'.dependencies]`; `install` names the installed
    file `lane-restart` (and `.bak`) off Windows. User-facing `wt.exe` text is left as is: it is accurate until a
    Linux launcher exists (P6).
  - **P2, one set of process names.** New `lane_state::platform::names` (`image_base`, `is_claude`, `is_shell`,
    `SHELLS`) replaces five private copies. **Behaviour change off Windows only:** `zsh`, `fish`, `dash` and `ksh`
    are accepted as shells above `claude` (a zsh user had every hook refused with `UnexpectedAncestor`), and count as
    "a live shell" below it. What Claude Code's process is called on Linux is still unmeasured, so `is_claude`
    accepts exactly `claude` / `claude.exe`.
  - Not changed, on purpose: `tab_close` shell and terminal lists (destructive, Windows-only until P10), `hook.rs`
    `TERMINALS` (P10), `pin-plan --root` default (a standalone crate with no dependency on `lane-state`).

## 0.14.26

- Double-launch guard (PM task 2026-10-14; the UniSQL case: a lane did `--self` at 09:03, the PM ran `start --role
  unisql` at 09:11 while the first host and claude were alive but stuck at a startup dialog, and no state file named
  either). `lane-restart start` and the restart / `--self` path now REFUSE while the process table holds a
  `lane-restart host --role <r>` or a claude whose launch arguments carry `--name <r>` (or the spec's own name),
  even when the state file says crashed, stale or absent; the pids are printed. A restart ignores only its own
  session and that session's host. An unreadable process table also refuses. `status` marks such a role
  `duplicate-risk: host pid N, claude pid M` (`duplicate_risk` in `--json`, absent for a normal row) and clears its
  `startable`. There is no override flag: close the stuck session, then retry. `lane_state::facts::ProcEntry` gains
  `cmd` (the full command line). The normal restart path is unchanged.

## 0.14.25

- docs/CI only (PM-allocated, portfolio hygiene from the CodeRipper triage 2026-10-14): `.github/CODEOWNERS`
  (`* @ciresnave`, WSP-001) and `docs/GLOSSARY.md` (DOC-009). No code change.

## 0.14.24

- `agentlife worktrees --stale [--json] [--root <dir> | --repo <path>...]` (PM task 2026-10-14, ROADMAP slice 3
  row (a)4): which registered worktrees belong to a merged branch, by the `CLAUDE.md` section 3/4 predicate: a MERGED
  PR whose head is the worktree's own HEAD, no open PR, `git ls-remote --heads origin <branch>` empty, and a clean
  tree. It prints the `git worktree remove` lines for the stale ones and **never removes anything**. Uncommitted
  files are `stale-dirty` (ask first), a branch reused after its merge is `merged-then-diverged`, a failed lookup is
  `unknown`; none of those is ever listed as stale. Worktrees come from `git worktree list --porcelain` per repo
  (main worktrees excluded); repos are the `--repo` paths, else the children of `--root` (default `C:/Projects`)
  that have a `.git` directory. Needs `git` and `gh`.

## 0.14.23

- `lane-restart status --peers` (PM ruling 2026-10-14, ROADMAP slice 3 row (a)3; `docs/PROPOSAL-STATUS-PEERS-JOIN.md`):
  per role, the claude-peers sessions whose MCP helper's **parent process** is the role's live `claude` (never joined
  by cwd, never by peer id), then the peers no role's live `claude` owns, with the cwd. `--json` gains `peers` per
  role, `peers_unmatched` and `peers_error`, all absent without the flag. A broker that cannot be read is
  `"unknown"` for every role, never an empty list. A peer's `summary` is printed (one line, control characters
  dropped, cut at 100 characters) only under `--peers`. Read-only: it sends nothing. `notify::Peer` gains
  `pid`, `summary` and `last_seen` (all defaulted) and `notify::list_all`; the notify send path is unchanged.

## 0.14.22

- docs only: `docs/PROPOSAL-STATUS-PEERS-JOIN.md`, the proposal for `lane-restart status --peers` (ROADMAP slice 3
  (a)3). No code change.

## 0.14.21

- `lane-restart install --from <exe> [--dry-run]` (PM only, even as a dry run; PM ruling 2026-10-14, ROADMAP slice 3
  row (a)2): installing a new `lane-restart.exe` was a hand ritual that left `.claude-hooks` with 13 `lane-restart*`
  files and a `lane-restart.sha256` that no longer described the exe. The candidate must run `--version` (first line
  `lane-restart x.y.z`) and `status` (exit 0) before anything changes. The installed exe is then renamed to the one
  `lane-restart.exe.bak` (an earlier `.bak` is replaced, so there is only ever one), the candidate is copied in and
  read back byte for byte, and `lane-restart.sha256` is rewritten (`<hex> *lane-restart.exe`) and read back. A
  failure after the rename puts the old exe back and says whether that worked. A ledger row of kind `install` is
  written for every attempt, including a refused non-PM caller. The directory is fixed
  (`C:/Projects/.claude-hooks`), like the state directory. It does not delete the older `.prev`/`.old*` files: those
  are the PM's to clear.

## 0.14.20

- `lane-restart --self` needs no `--role` (PM ruling 2026-10-14): the documented bare form in `CLAUDE.md` and
  `CIRESNAVE-EXPECTATIONS.md` 2.3b exited 1 with `--role <name> is required`. The role is `LANE_ROLE`, else the one
  role whose lane state records the current directory (case and separator insensitive); several matches are
  narrowed by the caller's claude pid, and if that does not leave exactly one the command refuses and names the
  candidates. An explicit `--role` is used as given, and every existing check on `--self` (the state file's pid
  must be the caller's claude) still runs on the result. Without `--self` the role is still required.

## 0.14.19

- docs only: `docs/ROADMAP.md` slice 3 (what the PM and lanes still do by hand around restart/restore/status, and
  where OverMind-split steps 6 and 7 stand, measured at OverMind `ca9e208` and consent `62b1788`). No code change.

## 0.14.18

- `status --quiet-over <secs>` (opt-in; no default, nothing enforced): marks each **live** role whose last hook
  write is older than `<secs>` as `QUIET>Ns` in text, and sets `activity.quiet_over` (and the report's
  `quiet_over_secs`) in `--json`. Without the flag the output is byte-for-byte what 0.14.17 printed. The PM's
  recommended value is **120**, a recommendation and not a default: a lane inside one long tool call is also
  quiet, so the mark is a prompt to look, not a stall verdict.

## 0.14.17

- `status` and `specs import` label a role whose recorded cwd is inside another role's cwd: `status` text
  `nested-in=<role>` and `nested_in` in JSON; an import row says `nested in <role>`. Report only: it changes
  neither `startable` nor what import writes, because a worktree under its repo is a legitimate nested lane.
  Why: a session that `cd`-ed into a sub-directory records the leaf name as its role (nine such roles on
  2026-10-14). Comparison is case-insensitive, `/` and `\` alike; equal cwds are not nesting.

## 0.14.16

- `status`: a role that is not idle says why: `idle=false(busy+shells_never_asserted) quiet=NNNs` and
  `activity.idle_blockers` in JSON (PM ruling 2026-10-14: a lane that never asserted shell-free is not idle).
  `quiet_secs` is the age of the lane's last hook write; no threshold is applied, the number is the claim.
- `status`: HANDOFF size per role against its ceiling (`handoff`: path, bytes, ceiling, over; text
  `handoff=NB/CB [OVER]`, `handoff=none` when no cwd or no file). Read-only, one `stat` per role, the same file
  `assert-idle` lints.
- `ledger --by-lane`: acted and refused counts per target role over the rows the other filters keep.

## 0.14.15

- `status` and `status --json` carry each **live** role's recorded activity (`activity`: `busy`,
  `subagents_running`, `no_background_shells`, `updated_at`, `updated_by_event`, `idle`). A role whose
  process is not live has no `activity` key, so a lane killed mid-turn cannot read as busy. Additive: the
  schema number is unchanged.
- One named idle predicate, `LaneState::is_idle` (PM ruling 2026-10-14: not busy, no subagents, no background
  shells asserted). `status` prints it and the restart gate asserts it, so the two cannot disagree. The
  ruling's fourth term, "no unread TASK", is not recorded in any state file, so it is **not** part of
  `idle`; a caller that needs it must read it elsewhere.

## 0.14.14

- Docs only: `docs/ROADMAP.md` gains "Next slice 2" (what the PM's sweep still does by hand: activity fields in
  `status --json`, one named idle predicate, HANDOFF size per role, a staleness mark, a ledger summary) and the
  measured breakdown of the 30 roles with no launch spec (18 cwd missing, 9 sub-directory leaf names, 3
  worktree or repo roots) with proposals for retiring or importing them. No code.

## 0.14.13

- Docs only: `docs/ROADMAP.md` slice item 4 is now a measurement item with its first measurement (this repo at
  `2ca6368`: 118 tracked files, 79 `.rs`, no `.py`/`.sh`/`.ts`/`.js`/`.ps1`) and the PM's ruling (2026-10-14
  11:48Z) on the open question: `tools/*.py` and `probe/` stay in OverMind, nothing is assigned here. No code.

## 0.14.12

- **`lane-restart assert-idle` warns when the HANDOFF is over its ceiling** (ROADMAP slice item 3, PM ruling
  2026-10-14: warn only, because refusing could block a mandatory restart). The ceiling is
  `CIRESNAVE-EXPECTATIONS.md` section 2.3c step 5: **8 KB for a lane's HANDOFF, 20 KB for the PM's**, measured in
  bytes (`wc -c`), "KB" read as 1024 bytes. The warning names the file, its size and the ceiling, goes to stderr
  and leaves the exit code alone. A missing HANDOFF is silent here (`stop` already refuses that).


## 0.14.11

- **`lane-restart status` says which roles still have no complete launch spec, and why** (ROADMAP slice item 5).
  After the human rows: `launch specs: N of M role(s) have a complete one; without:` and one line per reason with
  the roles under it: `launch spec invalid: ...`, `no launch spec; importable with specs import`, `no launch
  spec; cwd missing (...recreate the directory, or specs retire)`, `no launch spec; the state records no
  launch_args (not importable)`, `no lane state and no launch spec`. In `--json` the same is `coverage:
  {total, complete, gaps: [{why, roles}]}` (additive; `schema` stays 1). Every incomplete role is in exactly one
  gap. The reasons mirror what `specs import` checks (launch_args, cwd), read without writing anything.


## 0.14.10

- **`lane-restart ledger`** (read-only; ROADMAP slice item 2): prints `restart.log` oldest first, filtered by
  `--role`, `--kind`, `--since <RFC 3339|30m|2h|1d>`, `--acted`/`--refused`, `--last N`; `--json` prints the rows
  as JSON lines. A line that is not an entry is counted and named in the footer, never skipped silently; a line
  written before `kind` existed reads as a restart; unknown keys are ignored. It writes nothing (a missing log is
  an empty ledger and is not created). Answers "who restarted whom, and what was refused" without reading the
  file by hand.

## 0.14.9

- **`lane-restart status --json` carries each role's `model` and the disk floor** (ROADMAP slice item 1). `model`
  is what the launch spec records (`null` with no readable spec; Opus shown as recorded). `disk` is
  `{mount, free_bytes, floor_bytes, below_floor, unknown}` for the state volume; it was a stderr line before, and
  the stderr line stays. Additive: `schema` stays 1 and every existing field is unchanged.
  (The ROADMAP's earlier claim that `status` was human text only was wrong; `--json` has existed since 0.12.10.)


## 0.14.8

- **`lane-restart specs retire --role R [--role R]... [--dry-run]`** (PM only; PM-approved 2026-10-14). For a role
  whose recorded `cwd` is not a directory and whose recorded process is not live, **moves** its state file (and
  launch spec, if any) to `<state dir>/retired/<file>.<UTC stamp>`: never deletes, never overwrites, reads the
  move back, writes a ledger row. Role `pm`, a role with a directory (resume it with `start`) and a role with a
  live process are refused with the reason. Additive subcommand: proposed 0.14.x, not breaking.

## 0.14.7

- **`lane-restart specs import` reports unmistakably and groups what it refuses** (PM-approved 2026-10-14, ROADMAP
  "specs import: cwd-missing and Crashed roles", #57). The first and last lines say whether anything was written
  (`DRY RUN - nothing written` / `WROTE n spec(s)`). Roles refused for the same reason, notably `cwd missing`,
  share one line with a count and their names instead of one row each; a group of one keeps its full row, and
  `--verbose` restores one row per role. No role is dropped from the report, and what is written is unchanged.

## 0.14.6

- Docs only: `docs/ROADMAP.md` gains a proposed next slice (status `--json`, a ledger reader, a HANDOFF lint,
  specs coverage, the non-Rust pieces of the OverMind split) and the PM-approved `specs import` rulings
  (group `cwd missing` rows, `specs retire` PM-only, resume Crashed roles with `start`, first/last-line banner).

## 0.14.5

- **`pin-plan --target current|latest`** (with `--emit-patch`; default `current`, unchanged). `latest` pins a
  **tag** ref to the commit of the action's latest release tag (comment = that tag) instead of the commit the old
  ref names, which is how a node20 action reaches its node24 release. A **branch** ref is kept at the SHA it
  names now in either mode: a branch such as `dtolnay/rust-toolchain@stable` selects behaviour (the toolchain)
  that a release tag does not carry, and so is every ref of `dtolnay/rust-toolchain` (`@1.85.0` is a toolchain
  selector too; the `v1` release installs none). `DIR/ACTIONS.md` lists every distinct action with its runtime now (per ref)
  and at the latest release (`runs.using` at that commit), marks a major jump, and puts the actions whose
  latest release still declares node20 or older ("no node24 release yet") and those with no release first.

## 0.14.4

- **`pin-plan --emit-patch DIR`**: one `git apply`-able unified diff per repo (`DIR/<repo>.patch`) that pins each
  mutable `uses:` to the commit SHA its ref names now, the old ref kept as a comment (`@<sha> # v4`; an existing
  comment follows, CRLF kept). It writes only into `DIR`, refuses a `DIR` inside any planned repo, needs the
  network, and opens no PR; unresolvable refs are counted, not patched.
- **Dogfood test** (`crates/pin-plan/tests/own_workflows.rs`): this repo's workflow files, read from the index,
  hold 0 mutable refs and every pin carries its tag comment. `ci.yml` was already pinned (0.12.4, #36) and each
  SHA still equals what its commented ref names (read 2026-10-14); no workflow file changes here.

## 0.14.3

- **`pin-plan`: the SUP-008 read-only pin planner** (new workspace crate `crates/pin-plan`, not published). For
  every repo under `--root` (default `C:/Projects`) it prints one table per repo of each `uses:` that names a
  mutable tag or branch, with two SHAs and the runtime: the **commit the current ref names today** (pinning to
  it changes no behaviour; for a branch such as `dtolnay/rust-toolchain@stable` it is the only faithful pin), the
  **latest release** (tag and commit SHA; the highest plain-semver tag when the repo has no release; annotated
  tags dereferenced to the commit), and the **Node runtime** the action declares at each (`node20 -> node24`),
  flagging a current `node20` or older. It **writes nothing**: no file, no ref, no PR.
- Files are enumerated with `git ls-files` (never a disk walk) and their text is read from `origin/main`
  (`HEAD` if there is none), the ref printed per repo; a file in the index but not at that ref is named, not
  read. A child whose `.git` is a file (a linked worktree) is listed as skipped, so a repo is not planned twice.
  `uses:` lines inside `run: |` / `script: >` blocks are not read as references.
- Each distinct action is looked up once with `gh api`, however many files use it. `--no-network` lists the
  mutable refs without release data; `--all` also lists already pinned references; `--repo NAME` limits it.
  Anything that could not be resolved is printed as `UNRESOLVED:` with the reason and counted in the totals,
  never dropped.
- **Run for real** (2026-10-14, repos at their `origin/main` as fetched by each lane, `gh` as `ciresnave-bot`,
  206 s): 32 repos read (27 linked worktrees skipped), 51 files, 455 `uses:` references: 59 pinned, **393
  mutable** (313 tag, 80 branch) in 22 repos across 32 action repos; 1 unresolved (`NVIDIA/blossom-action@main`
  has no tags); 213 on node20 or older today (100 are `actions/checkout@v4`). This is a different population
  from CodeRipper's 235 pins in 18 repos: this counts every mutable reference in every repo here.
  The pure parts (classification, scanning, runtime and tag choice) are unit-tested.
## 0.14.2

- **Disk floor: nothing is launched while `C:` has less than 20 GB free** (PM task, incident 2026-10-14 04:45Z:
  `C:` hit 4 GB and builds failed in six lanes). New `lane_restart::disk` (`FLOOR_BYTES` = 20 GiB; the volume is
  the one holding the lane state directory, picked as the longest mount that is a path prefix, case and slash
  insensitive). It refuses, with exit 1 and the volume, free and floor named, in: `lane-restart start`
  (also `--dry-run`, which reports what would be refused), `lane-restart --role R --self|--yes` (checked
  **before** the kill, so a refusal never leaves a lane dead and not relaunched), `agentlife restore` and
  `agentlife pending approve` (checked **before** anyone is asked, so an approval is not spent on a refused
  run; `restore::preflight` repeats it as a fatal `disk floor` line at execution time). `restore --dry-run` is
  unaffected. `lane-restart status` prints a `C:\ free: N GB (floor 20.0 GB)` line first (stderr under `--json`,
  so the JSON on stdout keeps its shape).
- **An unreadable probe is a warning, not a refusal**: if no mount contains the state directory the launch goes
  ahead and says it did not check, so a broken probe cannot stop every lane restarting. Say so if you disagree.
- **Debug builds only** honour `LANE_DISK_FLOOR_BYTES` (tests set `0`) and `LANE_DISK_PROBE_PATH` (the path whose
  volume is measured, so Linux CI, where `C:/Projects/.lane-state` is on no mount, still exercises the refusal);
  a release build ignores both, so a lane cannot lower the floor.
- Not exercised for real: the refusal on this machine's actual `C:` (it has 130 GB free), and `lane-restart`'s
  start and restart paths end to end; those are covered by the pure `check`/`volume_for` tests, the restore
  preflight test and the `agentlife restore` e2e.

## 0.14.1

- **`lane-restart host` relays the child's last output instead of leaving it in the channel** (the quirk noted
  in 0.12.12 / #44: "host.rs main loop breaks on child exit without draining rx"). After the main loop stops and
  the ConPTY is closed, the reader can still deliver a final chunk (a ConPTY may flush its last frame only on
  close); it was sent into a channel nobody read any more. `host::drain_tail` now relays everything the reader
  sends until it ends or `READER_END_TIMEOUT` passes, and replaces the old "wait for the reader to end" poll.
  A write error on the outer side stops the relay without hanging the teardown.
- **Not reproduced end to end:** a real `cmd /c "echo ... & <nul set /p =TAIL"` child never lost its tail on this
  box (15 runs before the change, 5 per run after), so the new real-child test
  (`the_childs_final_output_reaches_the_outer_side_before_the_host_returns`) is a regression guard, not the
  proof. The proof is `drain_tail_relays_what_the_reader_sent_after_the_loop_stopped` (queued chunks, a late chunk,
  and a reader that never ends), which fails when the relay is removed.

## 0.14.0

- **`restore` launches a lane from its launch spec when it has a complete one; the registry record is the flagged
  fallback** (the PM's ruling "A", 2026-10-14; RESTART-TOOL-DESIGN.md 14, ROADMAP Next 4). A lane's
  `<role>.launch.json` (written by `lane-restart`, read through `lane_restart::spec`, used only when
  `SpecFile::check` passes) is turned into a recorded-style argv and given to the **same** rebuild as a
  registry record: the same allowlist, the explicit Sonnet/Haiku model pin (Opus is replaced, flagged
  `model-pinned:`), the bypass-is-PM-only refusal, the `;`/control-character refusal, and the portfolio-root and
  exists checks on the spec's directory. So `restore` and `lane-restart start` describe one launch, and a lane
  is no longer "not startable" for want of recorded arguments when it has a spec.
- **`plan::Entry.source`** (`spec` or `registry`) says which one a launch was rebuilt from. It is part of the plan
  hash, printed as `launch from:` in the plan text, and the approval summary adds a section naming every lane
  started from the registry. The role is the record's `LANE_ROLE`, else its launch directory's leaf.
- **Breaking** (second number, pre-1.0): the plan hash changes for every plan, so a frozen or pending plan built
  by 0.13.x no longer verifies and is re-planned, never approved as is. `plan::Inputs` has a new `specs` field.
- No spec is written or changed by `restore`; no live start was run.

## 0.13.1

- **A restore's report names every wanted agent it did not put in a batch, with why** (RESTORE-GAP-ANALYSIS 5b).
  `Report.unstarted` (and a `NOT IN ANY BATCH (n)` section of the text) lists agents the plan held (`held:max_running`,
  `held:memory`) or excluded (`excluded:Running`, `Unverifiable`, `DuplicateAlive`, `NoLaunchArgs`, `CwdMissing`,
  `CwdOutsideRoot`, `BypassNotPm`, `UnsafeArgument`), so a restore can no longer finish clean with a lane missing
  because it looked alive. `NotSelected` and `NotWanted` are the person's own choices and are not listed. Older report
  files still read (`#[serde(default)]`). Additive.
- `twelve_real_stand_ins_are_paced_...` now prints `held` and `running_now` as well as `excluded` when its entry count
  is wrong, so the known flake names its cause.

## 0.13.0

- **`agentlife park|stop` of a RUNNING lane now keeps `lane-restart stop`'s invariant: a lane is stopped only
  if it can be started again** (the PM's task of 2026-10-13: lane-stop and lane-start must keep every lane
  resumable). The lane's `<role>.launch.json` must exist and pass `lane_restart::spec::SpecFile::check`
  (complete, an explicit non-Opus model) twice: before the lane is asked to wrap up, and again just before
  the intent is recorded (the wait can be long). Without one nothing is asked, recorded or ended, and the
  refusal names `lane-restart specs import --role <role>`. After the process is gone the spec is marked
  `suspended` (best effort: a failure is journalled in `stopped` as `spec_error` and leaves the earlier
  complete spec, so the lane stays startable). Dry runs refuse too, so they cannot promise a stop that
  would be refused. **Breaking**: a `park|stop` that used to proceed on a lane with no spec is refused.
  A person who wants the old behaviour has no flag for it, on purpose.
- The role is the lane's self-claimed `LANE_ROLE`, else the leaf of its launch directory (the fallback the
  state file uses); a record with neither is refused.
- `agentlife` now depends on `lane-restart` (published, 0.12.16 on crates.io when read; by version plus
  workspace path, as `lane-state` is). This is what 0.12.15 said was missing.
- `docs/ROADMAP.md`: the priority-ordered list of what is built, what is unblocked and what waits on whom.
- Nothing is wired to a logon task and no live start/stop was run; tests use stand-in processes only.

## 0.12.16

- **The approval prompt's fourth thing: why (board 173, CireSnave 2026-10-12: *"four things, all prompts"*).**
  A restore's `Request.reason` is now `pending::restore_why`: the requester's words (cleaned, at most 60
  characters), `N lanes: <titles>`, and `plan <hash12>`, built from the same frozen plan as the hash, capped at
  200 characters with control and bidi characters stripped. It changes whenever the plan changes. The Hello
  channel shows it as `Reason given:` once `user-request` ships that line (OverMind); the wire field is the
  existing `Request.reason`. The stored pending record keeps the requester's own text. Supersedes the "three
  things" of decisions 134/150 and the logon-task caveat in `docs/CONSENT.md`.
- Nothing is wired to a logon task and no live start/stop was run.

## 0.12.15

- **PR 4 of RESTART-TOOL-DESIGN.md 14.13, the PM's option C: documentation only.** `agentlife park|stop`'s help
  and `src/down.rs`'s module docs now say that it ends the process without writing the lane's launch spec, so it
  does not keep `lane-restart stop`'s rule that a lane is suspended only if it can be started again, and that
  the PM stops a running lane with `lane-restart stop`. No behaviour changed.
- **Not done, and why:** refusing `park|stop`'s kill when no complete spec exists needs `lane_restart::spec`; the
  root crate depends on `lane-state` only, and `restore` launches from the registry, not the spec. A person-
  caller path for `lane-restart stop|start` was not specified (14.8: the PM session's alone), so agentlife does
  not call them.
- `tools/launch-lane.ps1` (the PM's) is named only in prose here (CHANGELOG, RESTART-TOOL-DESIGN.md,
  docs/RESTORE-GAP-ANALYSIS.md); no code, script or build step in this repository invokes it.

## 0.12.14

- **Restarting another lane is the PM session's alone, as `stop` is** (the PM's ruling of 2026-10-11, RESTART-TOOL-
  DESIGN.md 14.7 / 14.8). `lane-restart --role R` without `--self` is refused unless the caller's `claude`
  session is the one `pm`'s state file records (pid and start time; `LANE_ROLE` is not consulted), and the
  refusal is logged and happens before anything about the target is read, a dry run included. `--self` is
  unchanged. Before this, a restart of another lane was labelled `pm` in `restart.log` whoever ran it; all 475
  lines in `restart.log` at 2026-10-11 20:25Z carry `self` (453) or `pm` (22), so the log cannot show whether any
  `pm` line was a non-PM caller (the label was never checked), only that no other role was ever recorded.
  Breaking for anything that restarts another lane from outside the PM session, such as a person at a plain
  terminal, which has no `claude` ancestor to verify.
- `lane_stop::require_pm_for_other` is the one check `stop` and the restart share.

## 0.12.13

- **`lane-restart stop --role R --reason <text> [--self] [--dry-run] [--yes]`** (RESTART-TOOL-DESIGN.md 14.3,
  PR 3). Suspends a lane in a fixed order: every check, then the complete launch spec written durably and read
  back (`state: stopping`), then the verified kill, then the spec marked `suspended`. A lane is stopped only if
  it can be started again, so a spec that cannot be written leaves the process untouched. Another lane's stop is
  **PM only, by process** (the same ancestry check as `start`), needs the lane to be idle, and is a dry run
  without `--yes`; `--self` stops the calling lane. A HANDOFF is required, with no override: it must exist, be
  newer than the session being stopped, and (for another lane) not newer than the lane's idle assertion. Role
  `pm` is refused, because `start` refuses it and a suspended PM would have no way back. A lane on Opus (or an
  unknown model) is suspended with its model replaced by `sonnet`, and the ledger says `model-pinned:<was>`.
  Exit codes: 0 stopped and suspended, 1 refused or failed, 4 stopped but the `suspended` mark could not be
  written (the earlier spec is complete, so the lane stays startable).
- **A restart now launches from the spec it just wrote**, read back by the store, not from the state file it was
  derived from (14.7), and prepares that launch before the kill. A launch flag the spec's allowlist drops is
  therefore no longer carried into a restarted session.
- **`restart.log` `requested_by` is the verified caller** for a restart of another lane: the role whose `claude`
  session ran the command, or `unverified`, where it used to be the label `pm` whenever `--self` was absent.
  `caller_role` is set alongside it. A restart of another lane is not refused on it: the idle checks and `--yes`
  are unchanged.
- New public API: `lane_restart::lane_stop::{plan_stop, build_stop_plan, execute_stop, check_handoff,
  StopRequest, StopPlan, StopRefusal, StopError, StopDone}`; `spec::find_handoff` is public.
- Not in this change: PR 4 (agentlife's `park`/`stop`/`restore` consuming these, retiring `src/down.rs`'s kill
  and `tools/launch-lane.ps1`).

## 0.12.12

- **`lane-restart host` no longer abandons its helper threads.** The PTY reader sat blocked in `ReadFile` for the
  rest of the process and the resize thread, which owns the ConPTY master, never ended at all, so the pseudo
  console was never closed. The resize thread now stops when the child has exited; dropping the master closes
  the ConPTY and ends the reader, which the host waits for (3 s bound). `HostOutcome` gains
  `abandoned_threads`: the helpers still running at return, by name (a blocked outer stdin read cannot be
  cancelled through a generic `Read` and is reported as `outer-input`). Behaviour on the wire is unchanged.

## 0.12.11

- **`lane-restart start --role R --reason <text> [--model sonnet|haiku] [--dry-run]`**
  (RESTART-TOOL-DESIGN.md 14.5, PR 2). Starts a lane that is not running, from its launch spec, and writes the
  spec back as `running` once the session is confirmed. **PM only, by process**: the caller's `claude`
  ancestor must be the pid and start time recorded by `pm`'s state file (`LANE_ROLE` is never consulted).
  `--reason` is required (<= 200 characters, no control characters) and goes to the ledger, not the prompt.
  The model is `sonnet` or `haiku`; an Opus request is refused, a spec whose model is Opus, unknown or unset
  is launched on `sonnet` and the ledger says `model-pinned:<was>`, and `LANE_RESTART_ALLOW_OPUS` is not read.
  The permission mode is always `auto`; a start of role `pm` is refused and reported. The dead session's own
  state file is never mistaken for the new session coming up.
- **`lane-restart specs import [--role R] [--dry-run]`** (14.9, PM only): builds `<role>.launch.json` from a
  lane's state file for each role with no spec, labelled `written_by: "import"`, only when the spec is
  complete. The rest are listed with the reason; an existing spec is never overwritten and no state file is
  edited. This is what makes a lane that crashed before it ever launched under a spec-writing build (baracuda,
  2026-10-11) startable.
- `restart.log` lines gain `kind`, `caller_role`, `model` and `reason`, each omitted when unset, so existing
  lines and readers are unaffected. Restart-path lines now carry `kind: "restart"`.
- New public API: `lane_restart::start`, `lane_restart::spec_import`, `spec::pin_model`,
  `SpecFile::pinned`, `SpecState::Crashed`, `log::Entry::{kind, caller_role, model, reason}`.
- Not in this change (PR 3 onward): `lane-stop`, the `lane-restart` wrapper over stop + start, and the
  reader-thread fix in `host.rs`.

## 0.12.10

- **A restart writes the lane's launch spec before it kills anything** (RESTART-TOOL-DESIGN.md 14, PR 1).
  `C:/Projects/.lane-state/<role>.launch.json`: an allowlist serialisation of how the lane is launched
  (never the raw argv; inline-JSON `--mcp-config` / `--settings` are refused, not stored), written to a temp
  file, `sync_all`'d, renamed and read back. `kill_and_relaunch` refuses with `SpecNotWritten` when it
  cannot, and marks the spec `running` after a confirmed start, so a lane that later ends has a launch path.
  Invariant: a lane may be stopped only if it can be started again.
- **`lane-restart status [--json]`**: read-only; each role `running` / `suspended` / `crashed` /
  `never_run` / `unknown`, whether its spec is complete, whether it is startable. Liveness is the process
  table's (pid and start time), never a file's age.
- The `*.launch.json` files are skipped by `lane_state_writer::all_states`, `relaunch::models_report` and
  agentlife's `import::read_states` (they are not lane states).
- New public API: `lane_restart::spec`, `relaunch::kill_and_relaunch_persisting`,
  `RelaunchError::SpecNotWritten`. `kill_and_relaunch` and `kill_and_relaunch_with` keep their signatures.

## 0.12.9

- **Design: `RESTART-TOOL-DESIGN.md` section 14**, `lane-stop` / `lane-start` with `lane-restart` as the
  wrapper over them (PM task, CireSnave 2026-10-11). Proposal only: no code, nothing published. Acked by
  the PM 2026-10-11; the code PRs follow its section 14.13.

## 0.12.8

- **`persistant` 0.4.1 -> 0.4.4** (latest on crates.io; standing dependency rule). No code change.

## 0.12.7

- **CI: `actions/checkout` re-pinned to the current major** (v7.0.1, full SHA read from upstream). The
  other two pins (`dtolnay/rust-toolchain` stable, `cargo-deny-action` v2.1.1) were already the newest.
  `docs/PRE-PUBLISH.md` now records who owns `lane-state` on crates.io.

## 0.12.6

- **`docs/PRE-PUBLISH.md`**: the checklist for the unpublished crates (`lane-state`, `lane-restart`,
  `agentlife`): what must be true, the `cargo package` evidence, dependency order, version drift, and what
  only CireSnave decides. Nothing is published.

## 0.12.5

- **`agentlife doctor [--exe <path>]`** (R16, R17): read-only. Reports the installed `lane-restart` host
  (the configured `host_program`, resolved as a task would) and the `agentlife.exe` at the hook path
  (`C:/Projects/.claude-hooks/agentlife.exe`) with the version each reports against this build's, as OK,
  MISMATCH, MISSING or UNREADABLE; exits non-zero unless both are OK; installs nothing.

## 0.12.4

- **CI hardening.** A `deny` job (`cargo deny check advisories licenses bans sources`, `deny.toml`: a licence
  allow list read from `cargo deny list`, crates.io as the only source, wildcard requirements denied), and
  every action pinned to a full commit SHA read from its upstream repository.

## 0.12.3

- **The Hello prompt names who asked (#33).** `Asking` now carries the requester stored with the pending
  record, and the dialog shows that, not this process's: after a reboot the person saw the new pid while the
  grant kept the old one. `HelloPrompt::new()` no longer takes a requester.

## 0.12.2

- **`agentlife restore` runs.** Without `--dry-run` it freezes the plan, prints the agent list, asks the
  person (Windows Hello), and only if approved spends the one-use approval and starts the plan; the report
  goes to `<home>/reports`. New `agentlife::run` owns the order for `restore`, `pending approve` and
  `pending --prompt` (the last two run the plan at once on approval).
- **A lane cannot start agents:** an agent or unclear caller is refused (R10) before anything is stored,
  shown or asked. A person and the logon task pass.
- **The plan that runs is the plan that was approved:** it is rebuilt after the answer and before the
  spend; a different hash spends nothing and starts nothing.
- **A caveat, in `docs/CONSENT.md`:** the Hello text names only the plan hash, so a logon-task restore shows
  the person no agent list at the moment of approval.

## 0.12.1

- **The real consent backend.** `consent::installed()` returns `consent::real::UserRequestBackend`, over
  `user-request` 0.11.1 from crates.io (new dependency; it needs rust 1.95, which the workspace already
  does). It asks as agentlife, **not a registered lane** (role `agentlife`, empty session, own pid and start
  time), in the one store every user of `user-request` shares (`locate::dir()`, `head_copy()`, DPAPI),
  opened per call and dropped before a prompt is shown or a restore runs.
- `agentlife pending discard` now withdraws the request from that store. Nothing else a person sees
  changed: `pending approve` still says the prompt is not wired, and `restore` without `--dry-run` still
  refuses.
- **Fake corrected.** `RestorePlan` is `Scope::AnyRequester` in the real store (the plan hash is the
  binding), so the fake no longer narrows an approval by the requester's role, and the test that said
  another role "cannot spend" it now says the plan, not the spender, is what matches. Behaviour of
  `pending::spend_approval` against the real store is unchanged; only the fake's claim was wrong.
- New `consent::real::HelloPrompt` (`Prompt` over `user-request`'s Windows Hello channel). A channel
  `Refused` is `Unavailable`, which leaves the request pending. Not yet called by a command.
- Off Windows every consent call fails closed (the crate's DPAPI refuses), so nothing can be approved.

## 0.12.0

- **Breaking (library API):** the `Consent` trait takes the shape of `user-request` 0.11.1's store
  (RESTORE-GAP-ANALYSIS section 2, PR 3a). `spend_one_use(&mut self, kind, subject, &Requester)`
  returns `Result<String, String>`, the id spent; `Err` means nothing was spent and the restore must
  not run. A new `approved_at(kind, subject, &Requester)` reads when the approval was given, so
  freshness is still judged before the spend. `consent::Spent` and `consent::SpendError` are gone; a new
  `consent::Requester` carries who asks, supplied by the caller.
- `pending::spend_approval` takes a `&Requester`. A restore request's subject is now exactly
  `plan <64 lowercase hex>` (`consent::restore_plan_subject`, `parse_restore_plan_subject`), as the
  real crate requires; it used to be `restore N agents` (the count is still in the summary).
- The fake enforces the same: it refuses a restore-plan subject that is not `plan <hash>` or names
  another plan than the bound hash. (It also said the fake finds an approval by requester role; that
  was wrong for `RestorePlan`, a `Scope::AnyRequester` kind, and was retracted in 0.12.1: the plan hash
  is the binding, not who spends.)
- No real backend yet: `consent::installed()` still reports that none is installed, no dependency was
  added, and nothing a person sees or approves changed.

## 0.11.5

- New `agentlife import-lane-state [--write] [--json] [--since 48h] [--park a,b]`: seeds the registry
  from `.lane-state/<role>.json` (read only) so the first restore after a reboot does not have to
  wait for the hooks to fill it (RESTORE-GAP-ANALYSIS R3, R11, R12, R18). A dry run unless `--write`:
  it prints what it would write and what it skipped, and why.
  - Imported: files written within `--since` whose directory exists under the portfolio root and
    that recorded launch arguments; one record per name and directory (the newest file wins).
  - Skipped, each with its reason: too old, outside the root, missing directory, no launch
    arguments, test fixtures (`*restarttest*`), a name already in the registry, and files that are
    not lane states (`model-policy.json`).
  - The recorded `permission_mode` is carried as it was (so an `auto` lane comes back as `auto`);
    nothing is raised, and `restore` still refuses `bypassPermissions` for anyone but the PM.
  - The file's own session (pid and start time) is recorded, so a lane that is still running is
    seen as running, and one that died with the reboot is restored as a killed one.
  - `--park a,b` imports those agents as parked, so a restore leaves them down.
- `registry::Origin` gains `Imported` (`"imported"` in a record). A build older than 0.11.5 cannot
  read a record that carries it.

## 0.11.4

- A launched lane no longer inherits the spawner's `LANE_ROLE`. `lane-restart`'s role resolution
  lets `LANE_ROLE` win over the cwd leaf, so a lane started by a peer (the PM running
  `lane-restart --role x`, or `agentlife restore` from a lane's shell) used to write the
  spawner's role file instead of its own. `lane-restart`'s `spawn_launch` (both the `wt.exe` and
  the `conhost.exe` command) and agentlife's `RealSpawner` (via `TabLaunch.env_set`) now set
  `LANE_ROLE` to the launched role.

## 0.11.3

- `write_atomic` (used by the registry, the process index, pending restores, frozen plans and
  restore reports) now writes through `persistant` 0.4.1 (crates.io): its blocking `fs` store with
  a per-call scratch directory beside the target's directory, removed before the call returns, so
  an abandoned write cannot leave a temp file behind.
- Cost: each write starts persistant's worker thread and runtime. Measured at about 11 ms per
  write against about 4 ms for the previous temp-and-rename (release build, 1.5 KB files, 200
  writes per round, Windows). The most frequent caller, the registry, writes a few records per
  hook event, or about 20 in one fleet command, so no caller approaches one write per second.
- `is_temp_name` is kept only to skip `.<name>.tmp.<pid>.<n>` files the previous writer left
  beside their targets.
