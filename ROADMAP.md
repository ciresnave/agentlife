# agentlife roadmap

Written 2026-11-16 at `origin/main` d4c7323. This is the short, priority-ordered list. The long history,
measurements and incident notes are in `docs/ROADMAP.md` (rows 1 to 9 and the portfolio items); designs are in
`docs/`. A done row names the test or command that proves it; a row with no such name is not done.
Versions are allocated by the PM at gate time. "Who" is the only thing that can move a row.

## Done (each names its proof)

| Row | What | Proof |
|---|---|---|
| D1 | `idle_stop::verdict`, the pure dry-run rule for the idle soft stop (SUPERVISION-DESIGN 8a) | `cargo test -p lane-restart idle_stop` (6 pass at d4c7323) |
| D2 | Native Windows `gpu-run.exe`, the `gpu-run.ps1` v3 protocol (#289, #290) | `cargo test -p gpu-run` (`crates/gpu-run/tests/harness.rs`); `scripts/gpu-run-native-test.sh <exe>` (no PowerShell) |
| D3 | Runners clean up after every job: job-completed hook, apply and measure scripts (#287) | `tests/runner_cleanup_hook.rs`; `scripts/tests/runner-cleanup-apply.tests.ps1`, `scripts/tests/runner-job-cleanup-hook.tests.ps1` |
| D4 | A runner account per runner, `-Rearm`, ephemeral registration (docs/ROADMAP.md rows 8, 9) | `tests/runner_setup_account.rs`, `tests/runner_setup_rearm.rs`, `tests/runner_setup_ephemeral.rs` |
| D5 | `pin-plan` read-only SUP-008 planner | `cargo test -p pin-plan` |
| D6 | `lane-restart lane-new`, slices 1 to 3 (`--no-start`, dry run by default) | `cargo test --locked -p lane-restart --lib lane_new` (56 pass at d4c7323) |
| D7 | `lane-new` and `lane-provision` make the daemon know the role: roster read first, never claim a known role, 4 tries with back-off, read back | `cargo test --locked -p lane-restart --lib step_claim`, `epoch_from_list`, `retry_claim` (and the 38 lane_new/lane_provision tests); NOT run against the real daemon |
| D8 | A lane started by the launcher for mail gets a start prompt that says so and sends it to its inbox first | `cargo test --locked -p lane-restart --lib a_launcher_start_carries_the_mail_prompt_and_a_pm_start_the_generic_one` |
| D9 | The launcher log names each lane and outcome (`cycle: role=<r> outcome=<...>`) | `cargo test --locked -p agentlife-launcher a_result_is_logged_as_the_lane_and_its_outcome` |

## Open, in order

| Row | What | State | Who / blocker |
|---|---|---|---|
| O1 | Confirm the 0.47.5 portfolio findings of #292 are gone | #291 (dccf255) and #292 (0647588) merged. Read from the Portfolio job of CI run 44034835545 at 0647588: SEC-012 0, WSP-013 0 (gone); **SUP-016 1** (a different one: `LICENSE-MIT names a person as the copyright holder`, Info) and **ERR-012 13** (other discarded return values, e.g. `handle` 20 of 51 at crates/launcher/src/lib.rs:367) remain; GOV-010 28 and TYP-007 25 unchanged | lane: take ERR-012; SUP-016 is a licence-holder question for CireSnave/PM |
| O2 | Interleave the native `gpu-run.exe` with a real `gpu-run.ps1` holder: `scripts/gpu-run-native-test.sh C:/gpu-run/bin/gpu-run.exe <path to baracuda/scripts/gpu-run.ps1>` | UNMEASURED; PowerShell starts again (1.9 s on 2026-11-16) but the script must not run while a real GPU job holds `Global\gpu-run` | lane, when the lock is free |
| O3 | Triage of hung powershell/pwsh left by the Defender wedge (only those older than 12 h holding nothing under `C:\cb` or `C:\actions-runners`) | not done | lane |
| O4 | Idle soft stop: input gatherer, ledger writer, per-lane N/M adaptation, a self-idle call for lanes without hooks; all thresholds UNMEASURED | only `verdict` exists | PM go-ahead |
| O5 | SUPERVISION-DESIGN slice 1: kind-free core behind an adapter trait; proof is tests green plus byte-identical `status --json` and `<role>.launch.json` | not started | PM go-ahead |
| O6 | Wake-on-message through the `agentlife-launcher` task | task registered (Ready, At logon); launch routes accepted since 2026-11-17 09:42Z. MEASURED N=1 (one lane kiss, one sender pm, one try): soft stop 15:48:02Z, PM notice 15:48:49Z, launcher start 15:49:39.84Z, launch-to-live 7 s (`.lane-state/restart.log`), 2026-11-19; NOT a general result. False alarm `started-never-claimed` because the launcher lacks `see` on that lane (docs/DURABLE-TIMERS.md B2) | PM: one `see` grant for the launcher over all lanes; CireSnave: `SYNAPSE_LAUNCH_SENDERS` widening for timer mail (B1) |
| O7 | Elevated `runner-measure` result and the `ghr-baracuda.TUF-LAPTOP.00x` profile question | wait on a UAC approval; no profile is deleted | CireSnave |
| O8 | Portfolio style findings GOV-010 (28) and TYP-007 (25), read at 0647588 | left for later by the PM | lane |
| O9 | Issues #228 and #284 (close review 2026-12-02, act on a second sample), #14 (declined; reopen on a Linux failure log with `proc_start_ticks` stable while `btime` moved) | decided | weekly sweep |
| O10 | Move off `chrono` (RUSTSEC-2026-0543, unmaintained, all versions) to `jiff` or `time`; it is ours directly (agentlife, lane-restart, lane-state) and via synapse, auto-discovery, user-request | `deny.toml` ignore expires 2026-12-17 (PM ruling 2026-11-17); done when `cargo deny check advisories` passes with the ignore removed | lane; the synapse-side uses wait on those releases |
| O11 | After the Synapse lane publishes `email` as opt-in (outside `native-base`, one PR planned 2026-11-20): bump `synapse` and `synapsed-client`, drop lettre from our tree | proof when done: `cargo tree --locked -i lettre` prints nothing (today it comes via synapse rc.123 `native-base`, which lists `email`); cleared RUSTSEC-2026-0560 to -0565 in #295 already | wait on the Synapse release |
| O12 | Runner plain cancel not delivered to the humboldt runner (2026-11-19, 01:50Z) | CLOSED 2026-11-19: design `docs/RUNNER-CANCEL-TEST.md`, run by the Humboldt lane (push-trigger form; the dispatch form cannot work off the default branch). Running run: cancel delivered <= 2 s, kill ~8 s, child gone ~5 s after CTRL_BREAK, 3+3 repeats, clean negative control; queued run: cancelled in ~2 s, 3/3; the incident NOT reproduced (N=3, so only "not reproduced"; reading: GitHub-side transient); mitigation `timeout-minutes` (humboldt #216). Reopen only if it recurs, keeping the `_diag` lines | none |
| O13 | Durable lane timers: `waits-poll` also covers soft-stopped lanes, runs from a scheduled task, idempotent message id; the sender must be allowed to wake | DESIGN only, `docs/DURABLE-TIMERS.md`; proof when done: the `launchtest` plan in that file, N=5 | PM/CireSnave decide the sender widening and the task; lane builds after the answer |
| O14 | Sub-lanes: a primary lane creates and removes only its own `<primary>-<topic>` lanes (EXPECTATIONS 6.29) | DESIGN only, `docs/SUB-LANES.md`; open question on who may issue grants and retire the Synapse role | PM + Synapse lane |
