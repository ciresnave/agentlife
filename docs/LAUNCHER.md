# The launcher (M3b)

How a plan (`docs/PLAN.md`) would be executed, and what was measured while building it. Code:
`src/launch.rs` (one agent becomes one terminal tab), `src/restore.rs` (batches, liveness, report).
Design: `DESIGN.md` §2.2, §2.3, §2.5, §6, with the revisions in `DESIGN-REVISION-1.md` §3 and §6 and
`DESIGN-REVISION-2.md` §5.

**Nothing calls the launcher from a command.** `agentlife restore` without `--dry-run` still refuses:
starting agents needs a person's consent (M4). The only things that ever started a process in M3b are tests,
and every process they start is a stand-in they built themselves, killed at the end.

## One agent, one tab

`wt.exe -w agentlife-<k> new-tab --title <title> -d <cwd> <host> host --role <role> -- <claude> <prompt>
<flags...>`, and only if `wt.exe` **does not exist** (a `NotFound` spawn error, never a guess from `PATH`)
`conhost.exe <host> host ...` with the working directory set. Each point is a past failure of OverMind's:

* **A host, a real terminal.** `claude` started with inherited pipes runs as one-shot print mode and exits.
  The host (`lane-restart host`) owns a ConPTY and answers the development-channels dialog.
* **The prompt first, the flags after.** `--dangerously-load-development-channels` is variadic and would
  swallow a trailing prompt. The prompt is `read <role> HANDOFF and continue`, or, with no HANDOFF,
  `there is no HANDOFF for <role>, so say so and wait for instructions`: never a skip. **Never `--resume`.**
* **The `;` hazard.** `wt.exe` reads `;` as its own command separator. Every element that reaches it
  (window, title, cwd, host, claude, prompt, every flag) is checked, and a `;` or a control character
  refuses that agent **before anything starts**. The role must be `[A-Za-z0-9_-]{1,64}`.
* **The session-identity environment is stripped** (ten names, copied from OverMind; provenance in
  `docs/COPIED-FROM-OVERMIND.md`). The agent's id goes in `AGENTLIFE_AGENT_ID`.

## The run

1. The plan's hash must still match its content, else nothing starts. A **fatal preflight** failure (the host
   program does not exist) starts nothing: it would fail every agent identically. A missing `claude` on
   this `PATH` only warns (the host may find it).
2. Batches in order; **batch 0, the PM alone, settles before anything else starts**, and a PM that fails does
   not stop the others (its failure leads the report). Before each batch, free memory below the floor holds
   everything not yet started, reported as held for memory, not as failed.
3. Just before each launch the agent is **re-checked**; if it is running by now it is skipped.
4. Spawns within a batch are **`spawn_gap_ms` apart** (default 1,500; never before the first).
5. An agent is judged by **its own new session**, found in the registry the hook writes (a session that is not
   the one it had before, begun no more than 5 s before the launch, whose process is alive by pid **and**
   start time): **Working** when its lane state shows progress past `SessionStart`; **AwaitingDialog** when it
   is alive and shows none after `progress_timeout_secs` and carries the development-channels flag;
   **Started** when it is alive at the deadline without that evidence; **Failed** when its process ended,
   never appeared (`liveness_timeout_secs`), or could not be started. The next batch starts when every agent
   in this one has settled and `batch_delay_secs` has passed.
6. After `stop_after_failed_batches` batches in a row in which nothing came up, the run **halts** and starts
   nothing more.
7. One restore at a time: `restore.lock` names its owner by pid and process start time; a second run is told
   who holds it; a lock whose owner is gone (or whose pid is now a different process) is reclaimed.
8. The report is written atomically to `<home>/reports/restore-<UTC>-<hash>.json`, and the journal records
   `restore-started`, `restore-launched` and `restore-settled` per agent, `restore-halted`, `restore-finished`.

## What was measured, and what it changed (2026-10-07, this machine, Windows 11, CPU near 100% from other work)

* **`wt.exe` into one named window loses back-to-back tabs.** Three tabs started back to back into
  `agentlife-0`: the first ran, the others never did (3 runs). With 1.5 s and with 3 s between them **all three
  ran** (once each). Hence `spawn_gap_ms`. In the same runs `AGENTLIFE_AGENT_ID` **reached every tab's
  process**, so the variable does survive `wt.exe` on this machine (the hook also joins by name and directory,
  so it does not depend on this). Whether a window close, a kill or a shutdown still delivers `SessionEnd`
  is the M6 canary and is still unknown.
* **`conhost.exe` is unreliable as a fallback here.** Back to back it started its command about half the
  time; failures exited within 0.2 to 0.8 s, or showed the host and lost it within 200 ms, leaving no trace
  of it having run. In some periods it failed five attempts in a row (each `conhost.exe` exiting after about
  270 ms). So a conhost start is only believed when the host has been seen as its child **for a full
  second**; otherwise it is ended and started again (up to five attempts); and if every attempt fails the
  agent is reported **Failed** with that reason rather than silently missing. The cause of `conhost.exe` not
  running its command was **not established**.
* Neither failure shows when tests start only one process at a time, which is why the pacing and the
  verification exist in the product and not only in the test.

## Tests

* `src/launch.rs`: the argv shape (prompt first), one tab per `wt` command, the `;`/control-character
  refusal on every element, the role check, that both `wt` and `conhost` commands remove all ten
  session-identity variables and set the agent id (and `conhost` sets the directory), that a `wt.exe` that
  exists but cannot run is a failure and **never** a reason to try `conhost`, and the retry logic against real
  processes (exits at once: started again; lives but never starts its job: ended and started again; always
  fails: given up after the attempts; a spawn error is final).
* `src/restore.rs`, with a fake clock: PM first and alone; each batch only after the last settled and the
  delay; no more than `batch_size` launched-but-unsettled at once; a PM that never appears; halting after
  repeated failed batches and a good batch resetting the count; held for memory; fatal and non-fatal
  preflight; a tampered plan; an agent already running; a spawn error and an unsafe argument failing that
  agent only; AwaitingDialog versus Started; a process that ends after starting; the spawn gap; the journal and
  the report round trip; the registry observer over a real registry (the old session is never progress; a
  session older than the launch is not ours; the 5 s slack); the lock. **Fleets of 40 and 1,000 stand-ins**
  paced to the second.
* `tests/launch_e2e.rs`, real processes: a plan built from the **real registry** (written by stand-in
  `claude` processes through the real hook) is executed through a stand-in `lane-restart host`; the new session
  registers through the **real hook**, joins the **same** record (no duplicate agent), has the argv
  `[claude, prompt, flags...]`, got its own id and none of the identity variables; one agent shows lane-state
  progress and is Working, the others are AwaitingDialog; the report and journal are on disk; and the next
  plan is empty (idempotence against the real process table). Twelve stand-ins, PM first, batches of 5 with 1 s
  delay, never more than a batch at once. The real spawner through **`conhost.exe`** (Windows): CI requires it
  (`AGENTLIFE_REQUIRE_CONHOST=1`); a developer machine where `conhost.exe` will not run a command skips that one
  test and says so. The real **`wt.exe`** test opens real Windows Terminal tabs, so it runs only with
  `AGENTLIFE_REAL_WT=1`.

## Not here

Consent, the durable pending restore, the control tab and the first real restore (M4); standing grants
(M5); the canary experiments (M6). `lane-restart host`'s own behaviour is OverMind's and is only ever a stand-in
here.
