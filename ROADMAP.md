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
| O1 | Confirm the 0.47.5 portfolio findings of #292 are gone | #291 (dccf255) and #292 (0647588) merged. Read from the Portfolio job of CI run 44034835545 at 0647588: SEC-012 0, WSP-013 0 (gone); **SUP-016 1** (a different one: `LICENSE-MIT names a person as the copyright holder`, Info) and **ERR-012 13** (other discarded return values, e.g. `handle` 20 of 51 at crates/launcher/src/lib.rs:367) remain; GOV-010 28 and TYP-007 25 unchanged | lane: take ERR-012; SUP-016 is a licence-holder question for CireSnave/PM |
| O2 | Interleave the native `gpu-run.exe` with a real `gpu-run.ps1` holder: `scripts/gpu-run-native-test.sh C:/gpu-run/bin/gpu-run.exe <path to baracuda/scripts/gpu-run.ps1>` | UNMEASURED; PowerShell starts again (1.9 s on 2026-11-16) but the script must not run while a real GPU job holds `Global\gpu-run` | lane, when the lock is free |
| O3 | Triage of hung powershell/pwsh left by the Defender wedge (only those older than 12 h holding nothing under `C:\cb` or `C:\actions-runners`) | not done | lane |
| O4 | Idle soft stop: input gatherer, ledger writer, per-lane N/M adaptation, a self-idle call for lanes without hooks; all thresholds UNMEASURED | only `verdict` exists | PM go-ahead |
| O5 | SUPERVISION-DESIGN slice 1: kind-free core behind an adapter trait; proof is tests green plus byte-identical `status --json` and `<role>.launch.json` | not started | PM go-ahead |
| O6 | Wake-on-message through the `agentlife-launcher` task | task registered (`schtasks /Query /TN agentlife-launcher` shows Ready, logon trigger, running pid 57172 on 2026-11-17) and `lazy_enabled` is already true in `<home>/config.json`; BUT the daemon refuses it: `forbidden: this role may not use the launch routes` (launcher.log 2026-11-17 09:36Z on); wake-on-message proven earlier for 2 of 5 lanes | PM / Synapse lane: restore launch-route access for `agentlife-launcher@ciresnave`; CireSnave: `see` grants |
| O7 | Elevated `runner-measure` result and the `ghr-baracuda.TUF-LAPTOP.00x` profile question | wait on a UAC approval; no profile is deleted | CireSnave |
| O8 | Portfolio style findings GOV-010 (28) and TYP-007 (25), read at 0647588 | left for later by the PM | lane |
| O9 | Issues #228 and #284 (close review 2026-12-02, act on a second sample), #14 (declined; reopen on a Linux failure log with `proc_start_ticks` stable while `btime` moved) | decided | weekly sweep |
| O10 | Move off `chrono` (RUSTSEC-2026-0543, unmaintained, all versions) to `jiff` or `time`; it is ours directly (agentlife, lane-restart, lane-state) and via synapse, auto-discovery, user-request | `deny.toml` ignore expires 2026-12-17 (PM ruling 2026-11-17); done when `cargo deny check advisories` passes with the ignore removed | lane; the synapse-side uses wait on those releases |
