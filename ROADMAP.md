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

## Open, in order

| Row | What | State | Who / blocker |
|---|---|---|---|
| O1 | #291 pin move to coderipper c72a6e0 (0.47.5), then #292 (SEC-012 Debug redaction, SUP-016 SPDX x9, WSP-013 patent addendum, ERR-012 test helper) | both pushed, CI green; #292 is rebased on `main` after #291 merges | PM gate |
| O2 | Interleave the native `gpu-run.exe` with a real `gpu-run.ps1` holder: `scripts/gpu-run-native-test.sh C:/gpu-run/bin/gpu-run.exe <path to baracuda/scripts/gpu-run.ps1>` | UNMEASURED; PowerShell starts again (1.9 s on 2026-11-16) but the script must not run while a real GPU job holds `Global\gpu-run` | lane, when the lock is free |
| O3 | Triage of hung powershell/pwsh left by the Defender wedge (only those older than 12 h holding nothing under `C:\cb` or `C:\actions-runners`) | not done | lane |
| O4 | Idle soft stop: input gatherer, ledger writer, per-lane N/M adaptation, a self-idle call for lanes without hooks; all thresholds UNMEASURED | only `verdict` exists | PM go-ahead |
| O5 | SUPERVISION-DESIGN slice 1: kind-free core behind an adapter trait; proof is tests green plus byte-identical `status --json` and `<role>.launch.json` | not started | PM go-ahead |
| O6 | Register the `agentlife-launcher` scheduled task (`agentlife install-task`); wake-on-message proven for 2 of 5 lanes | `schtasks /Query /TN agentlife-launcher` returns rc=1; `see` grants pending | CireSnave / PM |
| O7 | Elevated `runner-measure` result and the `ghr-baracuda.TUF-LAPTOP.00x` profile question | wait on a UAC approval; no profile is deleted | CireSnave |
| O8 | Portfolio 0.47.5 style findings GOV-010 (28) and TYP-007 (25) | left for later by the PM | lane |
| O9 | Issues #228 and #284 (close review 2026-12-02, act on a second sample), #14 (declined; reopen on a Linux failure log with `proc_start_ticks` stable while `btime` moved) | decided | weekly sweep |
