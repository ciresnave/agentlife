kind: `launcher --clear <role>` re-raises a refused launch signal; the start line says whether the wake path is on
---
Measured 2026-11-13: Synapse keeps a refused launch signal terminal (its id is the role plus the enqueue time of the
oldest queued message), so mail sent behind it raises nothing; only the launcher role can re-raise it, with
`POST /v1/launch-clear`. `agentlife launcher --clear <role>` (repeatable) does exactly that as the launcher role, prints the
result and exits, needs no instance lock and starts nothing. The launcher's start line no longer says "notifier-only" whatever
the config says: it prints whether the wake path is ON or OFF and where `lazy_enabled` comes from (flag, `AGENTLIFE_LAZY_ENABLED`
or `<home>/config.json`, not `~/.overmind/lane-restart.json`). An unreadable registry now answers the conditional refusal `registry-unreadable` (the launcher clears it itself once the registry reads
again) instead of the terminal `unknown-role`. Verified: parser tests for `--clear`; the live call is run by
the lane that measures the wake. Not done here: re-reading `lazy_enabled` each cycle and a `synapse_home` key.

— lane: agentlife
