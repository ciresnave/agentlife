# Graceful stop of a running agent (M2b)

`agentlife park <agent>` and `agentlife stop <agent>` on an agent that is **running**. This is the first
code in agentlife that ends a process, so this page says exactly what it does, what it refuses, and
what is not built. Design: `DESIGN.md` §3, `DESIGN-REVISION-2.md` §3 to §4, `DESIGN-REVISION-3.md` §4.2.

## What happens, in order

1. **Authorize.** Only **a person** may stop a RUNNING agent (`down.rs`, the `only a person may stop a
   RUNNING agent` check right after the liveness match). The PM agent and every other agent are
   refused (`the_pm_is_denied_against_a_running_agent_and_nothing_is_asked_or_killed`, and end to end
   `the_pm_agent_cannot_stop_a_running_lane`): the PM may park a *stopped* lane (M2a), but ending a live
   session waits for a person's consent (M4). `control::authorize` alone would let the PM through;
   this extra check is what refuses it. **A lane cannot
   stop itself**: that kills the process that is running the command, and is a separate change.
   A pinned agent also needs `--confirm <its name>`.
2. **Identify what would be stopped.** The agent's last session must carry a process **start time**.
   "pid 4242" alone could be a different process by now; without a start time the stop is refused.
3. **Without `--yes`, print the plan and do nothing** (exit code 2: not success, not refusal).
4. **Ask the lane to wrap up** over the claude-peers broker (`127.0.0.1:7899`, loopback only): a
   `[TASK]` that tells it to write its HANDOFF, run `lane-restart assert-idle` **last**, and stop; and
   how to decline (do not run assert-idle). The peer is found by joining the broker's peer list to
   the agent's `claude` **by parent process** (the peer's `pid` is the MCP helper; its parent is the
   lane), never by `cwd` (two live lanes share one) and never by an id (ids rotate). If the lane
   cannot be asked, **nothing is changed**.
5. **Wait, up to `--timeout` (default 600 s), until it is provably ready.** All of, using evidence
   newer than the request: a **HANDOFF** written after the request (`<launch dir>/HANDOFF.md`, or
   `<launch dir>/<NAME>-HANDOFF.md`); a `lane-restart` state record, found by **session id** (not role
   name; the phantom-file defect), saying it **asserted idle after the request**, is **not busy**
   and has **no subagents**; and **no live shell** below its `claude` in the process tree. **Unknown
   counts as unsafe.** On timeout the agent is **left running** and every blocker is reported.
6. **Record the intent, then stop.** `Closed` and a `closed` journal entry are written **before** the
   process is touched. Immediately before the kill the process is re-read and compared (pid **and**
   start time). If the kill is refused, the process does not go, or the pid now belongs to something
   else, the intent is **put back** and `close-failed` is journalled.

Journal events, in order for a normal stop: `down-requested`, `closed`, `stopped`.

## What it never does

* Write `.lane-state` (it only **reads** it) or any file outside agentlife's home.
* Kill a process it did not verify by pid and start time, or one that merely holds a recycled pid.
* Stop an agent because a message "looks" handled: only the lane's own HANDOFF and idle claim count.
* Act without `--yes` on a running agent.

## Not built (stated, not hidden)

* **A lane stopping itself** (above).
* **Closing the leftover terminal tab.** OverMind's `tab_close` closes the shell tab a hand-started
  lane leaves behind; not copied here. A hand-started lane's tab stays at a prompt.
* **Consent for anyone else** (M4): another agent is simply refused.
* **Restore** (M3/M4): `unpark` without `--no-start` is still refused.
* It depends on claude-peers being up and the lane having the `lane-restart` hooks installed; if
  either is missing the stop is refused rather than guessed.
* The wrap-up is an instruction to a model. A lane may ignore it; then it is left running.

## Tested how

Unit tests with scripted fakes cover each branch of the sequence and its **order** (the intent is
asserted to be recorded *at the moment of the kill*), plus the real terminator against a real child
(wrong start time refused and the child left alive; right identity ended). End-to-end tests start a
stand-in `claude` as the target lane, a fake claude-peers broker on **real loopback**, and a person
running the real `agentlife park` (the PM stand-in is used only to show it is refused); a test thread plays the lane's side. They show
the real graceful stop (the process really ends, the helper below it is untouched), the dry run, a lane
that never wraps up (left running), a live shell below the lane (blocks, and the journal names it), and a
lane not found among the peers (not asked, not stopped).

## The risk surface (non-test code), measured

One process kill: `src/down.rs:105` (`proc_.kill()`, at 7baf4e0 plus the person-only check), reached only through `down::stop_running`, which
only `main.rs` (`run_down`) calls. **It is a hard end** (TerminateProcess / SIGKILL), not a polite close. It is reached only after a
person asked with `--yes`, the lane wrote its HANDOFF and asserted idle, and nothing is running below it. A lane that never
asserts idle is **left running**: nothing is killed, the journal records `down-timeout` with the blockers, and the intent is
unchanged. One network use: `src/peers.rs` connects to the configured broker
address, which the configuration refuses unless it is loopback. New reads: `.lane-state/*.json` and
HANDOFF modification times. No new spawns, removals or writes.
