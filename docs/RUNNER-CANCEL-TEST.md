# Runner cancel test (design, for the Humboldt lane to run)

Status: DESIGN ONLY. Nothing here has been run. Written 2026-11-19 (agentlife lane) on the PM's plan: agentlife designs, the
Humboldt lane runs it on a scratch branch of ITS repo (`ciresnave/HumboldtUnifiedKidTracker`, the repo the runner
`C:\actions-runners\humboldt` is registered to), and reports the timings. agentlife writes to no other repo.

## What is being tested

Finding (agentlife, 2026-11-19, logs in `C:\actions-runners\humboldt\_diag`): on 2026-11-19 a plain cancel at about 01:50Z
never reached the runner listener. `Runner_20261116-223430-utc.log` shows `Successfully renew job ... valid till` every
60 s up to 01:56:03Z and no cancel line; the first one is 01:56:22Z (`We are not yet checking the state of jobrequest ...
Cancel running worker right away`, `Send job cancellation message to worker`) when the PM force-cancelled. Once told, the
Worker killed fast (`Sending CTRL_C` 01:56:23, `did not honor CTRL_C within 7.5 seconds`, `CTRL_BREAK` 01:56:30, `Process
terminate successfully`, step Canceled 01:56:31), and the stdout/stderr streams closed only at 01:57:59 (about 88 s of
pipe-held tail). Same shape at 2026-11-17 10:56, 11:01, 11:04 and 2026-11-18 00:14.

Questions: (1) does a plain user cancel reach the listener, and how late? (2) does a second run that supersedes the first
through a concurrency group with `cancel-in-progress` reach it? (3) after the kill, how long does the `cargo test` child
tree live? The answer decides: a runner/broker bug to report (runner `useV2Flow`, broker delivery; whether to report to
`actions/runner` or GitHub support is CireSnave's call) or `timeout-minutes` as the mitigation.

## The probe (a throwaway file on a scratch branch, never merged)

`.github/workflows/cancel-probe.yml`, `workflow_dispatch` with one input `case` (`plain` or `supersede`):

```yaml
name: cancel-probe
on:
  workflow_dispatch:
    inputs:
      case: { description: plain or supersede, default: plain }
concurrency:
  group: cancel-probe
  cancel-in-progress: ${{ inputs.case == 'supersede' }}
jobs:
  probe:
    runs-on: [self-hosted, windows]   # use the labels the humboldt runner really has
    timeout-minutes: 15               # a safety net for the experiment only
    steps:
      - uses: actions/checkout@v4
      - shell: bash
        run: |
          echo "step-start $(date -u +%T)"
          cargo test --test cancel_probe -- --nocapture
```

`tests/cancel_probe.rs` (scratch branch only), a child that behaves like a long `cargo test`:

```rust
#[test]
fn sleeps_ten_minutes() {
    println!("child pid {}", std::process::id());
    std::thread::sleep(std::time::Duration::from_secs(600));
}
```

## Procedure (3 repeats per case, same machine, one run at a time)

Case (a) plain cancel: `gh workflow run cancel-probe.yml -f case=plain`; wait until the step prints `child pid`; note
T0 = UTC time just before `gh run cancel <id>` (no `--force`).

Case (b) supersede: dispatch run 1 with `case=supersede`; wait for `child pid`; dispatch run 2 (`case=supersede`) and note
T0 = run 2's `created_at`. Run 2 is then cancelled by hand and waited out.

Record per repeat, all UTC, from `_diag` (`Runner_*.log`, `Worker_*.log`) and the process table:

| field | where |
|---|---|
| T0 cancel issued | the dispatcher's clock |
| T_listener | `Runner_*.log`: `Send job cancellation message to worker` |
| T_ctrlc / T_break / T_term | `Worker_*.log`: `Sending CTRL_C`, `Sending CTRL_BREAK`, `Process terminate successfully` |
| T_conclusion | `gh run view <id> --json status,conclusion,updatedAt` polled every 5 s |
| T_child_gone | `Get-Process cancel_probe*` on the runner polled every 5 s (the test binary is `cancel_probe-<hash>.exe`) |

Also copy the lines of the `Runner_*.log` between T0 and T_listener (renewals only, or something else) into the report.

Pass/fail, per case: **delivery** is fine if T_listener - T0 <= 30 s in all 3 repeats; a gap over 60 s in any repeat is the
bug class. **Tail** is T_child_gone - T_term (the baseline from the incident is about 88 s). Negative control: one run
left alone to the end of its 10 minutes must conclude `success` with no cancel lines, so the log search is not matching
everything.

## Decision table

| (a) plain | (b) supersede | meaning | action |
|---|---|---|---|
| delivered <= 30 s | delivered <= 30 s | the 01:50Z miss is not reproducible here | keep `timeout-minutes`, note N=3 each |
| late or never | late or never | broker/runner delivery bug | CireSnave decides the report; `timeout-minutes` meanwhile |
| delivered | late or never | concurrency supersede does not signal the runner | report, and do not rely on `cancel-in-progress` on this runner |
| tail > 30 s | any | child tree outlives the step shell | run the test step so the child is in the shell's kill tree (a job object) or accept the tail |

UNMEASURED until run. Three repeats per case cannot prove an intermittent fault absent; a clean result is "not reproduced
in 3", nothing stronger.
