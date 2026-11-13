kind: `install-task` also registers the launcher task
---
`agentlife install-task --register` (and `--remove`, and the printed XML and systemd unit) now include a third task,
`agentlife-launcher`: `agentlife launcher` from logon, restarted every minute on failure (999 attempts, the scheduler's
maximum), a second instance ignored. `--only <logon|unlock|launcher>` limits `--register`, `--remove` and the printout to one task (default unchanged: all three),
so the launcher can be registered without the restore-at-logon tasks. Until now the launch-on-message service had no task and ran only when started by
hand. `docs/LAUNCHER-SERVICE.md` records CireSnave's 2026-11-13 standing consent for soft-stopped lanes, the
`lazy_enabled` switch and the `SYNAPSE_LAUNCH_SENDERS` allowlist. Verified: a unit test for the XML (logon trigger,
`launcher` argument, restart settings, no restore, the two restore tasks not restarted). The task itself has not been
registered or run.

— lane: agentlife
