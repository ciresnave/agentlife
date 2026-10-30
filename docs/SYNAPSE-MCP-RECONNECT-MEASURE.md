# Does the Synapse MCP adapter reconnect after a `synapsed` restart? (scratch measurement)

Measured 2026-10-30 (PM TASK, HANDOFF item 2). Installed binaries: `synapsed` 6.0.0-rc.179, `synapse` CLI
6.0.0-rc.182; the `synapse-claude-channel` adapter's version was not recorded. **Everything ran on a scratch
home (`%LOCALAPPDATA%\synapse-reconnect-scratch`, account `reconntest`) and scratch port 127.0.0.1:7941.** The live
`synapsed` (pid 15536) was never started, stopped or contacted; it was still running at the end. The probe
(`docs/spike/synapse_reconnect_probe.py`) refuses to run unless `SYNAPSE_HOME` contains `reconnect-scratch` and the port
is 7941.

## What was run

The adapter (`synapse-claude-channel --role probe`) was driven over stdio by a script acting as the MCP client
(`initialize`, `notifications/initialized`, `tools/list`, then it only reads). A second role (`sender`) sent one
message every ~2 s with `synapse send`. The scratch daemon was killed with `taskkill /F` on its own pid. Each run logs
every `notifications/claude/channel` push with a timestamp.

**This is the adapter level, not a Claude session.** No `claude` process was started (the development-channels dialog
needs a human, and a live process is not a working session). "What the session sees" is therefore what the adapter
emits to its MCP client, not what a model displayed.

## Results

| run | what | result |
|---|---|---|
| `ctl` (positive control) | daemon left up, 66 s | adapter alive; all 25 messages pushed 0.8 to 1.0 s after send; un-acked messages pushed again after the 30 s lease (up to 3 times), as the soak procedure says |
| `ctl2` (second control) | same | **discard**: it started while the previous run's adapter-spawned daemon still held the mail store, so it did not run on its own daemon; its first push came at 41 s and 6 of 25 ids were never pushed |
| `g5` | daemon killed, my restart 6.7 s later | adapter stayed alive and stayed connected to the MCP client (no error, no exit); first push 0.26 s after the daemon started; no daemon came back by itself in those 6.7 s |
| `g45`, `g240` | killed, intended gaps 45 s and 240 s | **gap was 3.2 s and 2.4 s, not 45 and 240**: the port was open again about 2 s after the kill with no action from me; first push 0.77 s and 0.51 s after |
| `b60` | killed, intended gap 60 s with a dummy listener on the port | **invalid run**: see below |

### The finding that matters: the adapter replaces the daemon itself

In `g45`, `g240` (and `b60`) a daemon was listening again about 2 s after the kill. I had not started it. Evidence it
was the adapter: it wrote `synapsed-start.err` (`synapsed: listening on 127.0.0.1:7941`) in the scratch home at the
moment, and the daemon my script then started failed with `another process holds the mail store (...mailbox.sqlite-full.lock)`.
The docs say as much (`RESTARTTEST_SOAK_PROCEDURE.md` 2.2: the adapter auto-starts a daemon, from its own environment).
So "the adapter reconnected" is, in these runs, "the adapter noticed, started a replacement daemon within ~2 s, and
reconnected to it". Consequences:

1. **No reconnect over a gap longer than about 2 s was measured**, because nothing here could hold the gap open. The
   `b60` attempt used a dummy listener bound with `SO_REUSEADDR`; on Windows that lets a second socket bind a port that is
   already listening, so it did not exclude the respawned daemon (69 pushes arrived "during the gap"). Discard `b60`.
2. The respawned daemon is **detached from the adapter**: pid 8628 outlived the probe and the adapter's kill, and I had to
   stop it by hand (verified it owned port 7941 and had the start time of the `.err` file before killing it).
3. The respawn uses the **adapter's environment**. On the live system, if a lane's adapter lacks `SYNAPSE_HOME` or
   `SYNAPSE_ADDR` the replacement opens the default home; and a daemon the adapter starts is not the scheduled-task daemon
   (no `SYNAPSE_LAUNCH_SENDERS`, no task wrapper env). **Inference, not measured on the live daemon**, but it means
   stopping the live `synapsed` is not a clean "gap": within seconds each lane's adapter may start its own daemon
   and the lanes may end up on different daemons.

### What the adapter does once a daemon is reachable

- Delivery resumes fast: first push 0.26 to 0.87 s after the daemon is up (all runs).
- **Lease state does not survive.** Straight after each restart the adapter pushed again every message from earlier runs
  that nobody acked (24 and more in about 2 s, spread over 3 s). The mailbox persisted, the leases did not. A session
  must treat those as repeats by `message_id` (the existing rule), and there can be a burst.
- Checked by id against the recorded events: in `ctl`, `g5`, `g45` and `b60` every sent id was pushed at least once. In
  `g240`, `g240-022` (sent about 6 s before the probe stopped) was **not** pushed while `-021` and `-023` were; I did not
  establish whether it was lost or only late. So "no message lost" is not claimed.

## Not measured

- A real Claude session (what a model displays, and whether Claude Code drops the MCP server when its stdio peer is
  fine but the daemon is not): needs the dialog answered by a human on a scratch lane.
- A genuine gap of 30 s or more. Needs a way to stop the adapter respawning (for example an exclusive bind, which on
  Windows means `SO_EXCLUSIVEADDRUSE`, or an adapter run with a `PATH` that has no `synapsed`).
- A message sent during the gap (`synapse send` also auto-starts a daemon; the probe avoids sending then).
- Anything about the live daemon or the lanes' real adapters.
- n is 1 to 2 per case; no variance.

## Suggested next step (not done)

Re-run with an exclusive bind (`SO_EXCLUSIVEADDRUSE`) held from just after the kill, with the daemon's port otherwise
refused, to get a real 30 to 120 s gap and see whether the adapter ever gives up. Before anyone stops the live
`synapsed` for the weekly provisioning run, decide whether an adapter-spawned replacement is acceptable or whether the
lanes' adapters should be pinned not to auto-start (a Synapse-lane question).
