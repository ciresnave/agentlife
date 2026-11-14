kind: the launcher re-reads `lazy_enabled` every cycle and takes a `synapse_home` key
---
Measured 2026-11-13: the scheduled launcher task has no `SYNAPSE_HOME`, so it exited 1 until a wrapper script set it, and the wake
switch was read once at start, so flipping it needed a kill and restart. New config key `synapse_home` (`<home>/config.json`, empty =
`SYNAPSE_HOME`, else the platform default) is used by `launcher` and `launcher --clear`. The wake switch (`lazy_enabled`) is re-read every
cycle: a change is logged, an unreadable config reads as OFF, and while it is off the real mechanism (now always built) answers `wake-off`
and spawns nothing. Verified: a test that turns the switch on and off again and drives the switched mechanism, the config key tables, clippy
`--workspace --all-targets`; not run live. `docs/LAUNCHER-SERVICE.md` lists the grants the launcher role still lacks.

— lane: agentlife
