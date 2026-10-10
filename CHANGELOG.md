# Changelog

One version for the whole workspace (`agentlife`, `lane-restart`, `lane-state`).

## 0.11.5

- New `agentlife import-lane-state [--write] [--json] [--since 48h] [--park a,b]`: seeds the registry
  from `.lane-state/<role>.json` (read only) so the first restore after a reboot does not have to
  wait for the hooks to fill it (RESTORE-GAP-ANALYSIS R3, R11, R12, R18). A dry run unless `--write`:
  it prints what it would write and what it skipped, and why.
  - Imported: files written within `--since` whose directory exists under the portfolio root and
    that recorded launch arguments; one record per name and directory (the newest file wins).
  - Skipped, each with its reason: too old, outside the root, missing directory, no launch
    arguments, test fixtures (`*restarttest*`), a name already in the registry, and files that are
    not lane states (`model-policy.json`).
  - The recorded `permission_mode` is carried as it was (so an `auto` lane comes back as `auto`);
    nothing is raised, and `restore` still refuses `bypassPermissions` for anyone but the PM.
  - The file's own session (pid and start time) is recorded, so a lane that is still running is
    seen as running, and one that died with the reboot is restored as a killed one.
  - `--park a,b` imports those agents as parked, so a restore leaves them down.
- `registry::Origin` gains `Imported` (`"imported"` in a record). A build older than 0.11.5 cannot
  read a record that carries it.

## 0.11.4

- A launched lane no longer inherits the spawner's `LANE_ROLE`. `lane-restart`'s role resolution
  lets `LANE_ROLE` win over the cwd leaf, so a lane started by a peer (the PM running
  `lane-restart --role x`, or `agentlife restore` from a lane's shell) used to write the
  spawner's role file instead of its own. `lane-restart`'s `spawn_launch` (both the `wt.exe` and
  the `conhost.exe` command) and agentlife's `RealSpawner` (via `TabLaunch.env_set`) now set
  `LANE_ROLE` to the launched role.

## 0.11.3

- `write_atomic` (used by the registry, the process index, pending restores, frozen plans and
  restore reports) now writes through `persistant` 0.4.1 (crates.io): its blocking `fs` store with
  a per-call scratch directory beside the target's directory, removed before the call returns, so
  an abandoned write cannot leave a temp file behind.
- Cost: each write starts persistant's worker thread and runtime. Measured at about 11 ms per
  write against about 4 ms for the previous temp-and-rename (release build, 1.5 KB files, 200
  writes per round, Windows). The most frequent caller, the registry, writes a few records per
  hook event, or about 20 in one fleet command, so no caller approaches one write per second.
- `is_temp_name` is kept only to skip `.<name>.tmp.<pid>.<n>` files the previous writer left
  beside their targets.
