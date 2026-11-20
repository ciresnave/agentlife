# Runner cancel test (design, for the Humboldt lane to run)

Status: RUN 2026-11-19 by the Humboldt lane; results at the end. The design below was corrected after the first attempt to
run it (the trigger, see "Trigger correction"). Written 2026-11-19 (agentlife lane) on the PM's plan: agentlife designs, the
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

## Trigger correction (found by the first attempt to run this design, 2026-11-19)

The first version of this document used `on: workflow_dispatch` and `gh workflow run` from a scratch branch. That cannot work:
GitHub dispatches a `workflow_dispatch` workflow only if it exists on the DEFAULT branch, and the probe must never be merged.
The Humboldt lane found this when it tried to run it (its FINDING, relayed by the PM) and ran the probe with a push trigger on
scratch branches instead: `on: push: branches: [scratch/cancel-probe-*]`. A run is started by pushing a commit to such a branch.
The YAML below is the push form; "dispatch" in the procedure means "push a commit to a `scratch/cancel-probe-*` branch".

## The probe (a throwaway file on a scratch branch, never merged)

`.github/workflows/cancel-probe.yml`, triggered by a push to `scratch/cancel-probe-*` (the supersede case is a second push
to the same branch, which the concurrency group then supersedes):

```yaml
name: cancel-probe
on:
  push:
    branches: ['scratch/cancel-probe-*']
concurrency:
  group: cancel-probe
  cancel-in-progress: ${{ endsWith(github.ref_name, '-supersede') }}
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

Case (a) plain cancel: push to `scratch/cancel-probe-plain`; wait until the step prints `child pid`; note
T0 = UTC time just before `gh run cancel <id>` (no `--force`).

Case (b) supersede: push to `scratch/cancel-probe-supersede` (run 1); wait for `child pid`; push again (run 2) and note
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

## Results (measured 2026-11-19 by the Humboldt lane; relayed by the PM from humboldt's FINDINGs, not seen by this lane; the run ids are checkable with `gh run view` in the humboldt repo)

Probe: push-triggered scratch branches, a 600 s sleeping cargo-test child; runner humboldt or humboldt-2. T_list = Runner log
`Send job cancellation message to worker`; term = Worker log CTRL_BREAK terminate. Times UTC.

| Case | Run (old/new) | T0 | T_list | term | concluded |
|---|---|---|---|---|---|
| (a) plain cancel, running | 44463795443 | 05:24:42.9 | +1.1 s | 05:24:52 | 05:25:04 |
| (a) | 44465189578 | 05:27:16.6 | +1.4 s | 05:27:25 | 05:27:38 |
| (a) | 44465378488 | 05:28:53.4 | +1.6 s | 05:29:02 | 05:29:15 |
| (b) supersede | 44465557302 / 44465660188 | 05:30:41 | +1 s | 05:30:50 | 05:31:07 |
| (b) | 44465877485 / 44465961643 | 05:33:18 | +1 s | 05:33:26 | 05:33:38 |
| (b) | 44466160233 / 44466247229 | 05:35:43 | +1 s | 05:35:52 | 05:36:06 |
| (c) plain cancel of a QUEUED run | 44467872558 / 44467791256 | 05:50:19.6 | - | - | cancelled 05:50:21 |
| (c) | 44468044785 / 44467961877 | 05:51:51.4 | - | - | cancelled 05:51:53 |
| (c) | 44468213878 / 44468129991 | 05:53:20.6 | - | - | cancelled 05:53:22 |
| negative control | 44466444392 | left alone | - | - | success, 0 cancel lines, child exited by itself |

- A cancel of a running run reached the listener in at most 2 s, the step shell was killed about 8 s later (CTRL_C ignored for
  7.5 s, then CTRL_BREAK), and the child was gone about 5 s after CTRL_BREAK. 3 + 3 repeats, a clean negative control.
- A cancel of a queued (group-pending) run concluded in about 2 s, 3 of 3.
- **The 01:50Z incident was NOT reproduced.** With N = 3 per case that means only "not reproduced", nothing stronger. The
  reading is a GitHub-side transient: the incident's run was a PR event waiting on a run whose cancel the runner never saw.
- **Not tested:** a PR-event supersede during the Backend `cargo test` step, and the real pipe-holding cargo tree (the
  incident's ~88 s tail).
- **Mitigation stands:** `timeout-minutes` on the job (humboldt #216). No further probes unless it recurs; then keep the
  `_diag` Runner and Worker lines for the window.

