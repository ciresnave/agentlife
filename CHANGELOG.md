# Changelog

One version for the whole workspace (`agentlife`, `lane-restart`, `lane-state`).

## 0.12.2

- **`agentlife restore` runs.** Without `--dry-run` it freezes the plan, prints the agent list, asks the
  person (Windows Hello), and only if approved spends the one-use approval and starts the plan; the report
  goes to `<home>/reports`. New `agentlife::run` owns the order for `restore`, `pending approve` and
  `pending --prompt` (the last two run the plan at once on approval).
- **A lane cannot start agents:** an agent or unclear caller is refused (R10) before anything is stored,
  shown or asked. A person and the logon task pass.
- **The plan that runs is the plan that was approved:** it is rebuilt after the answer and before the
  spend; a different hash spends nothing and starts nothing.
- **A caveat, in `docs/CONSENT.md`:** the Hello text names only the plan hash, so a logon-task restore shows
  the person no agent list at the moment of approval.

## 0.12.1

- **The real consent backend.** `consent::installed()` returns `consent::real::UserRequestBackend`, over
  `user-request` 0.11.1 from crates.io (new dependency; it needs rust 1.95, which the workspace already
  does). It asks as agentlife, **not a registered lane** (role `agentlife`, empty session, own pid and start
  time), in the one store every user of `user-request` shares (`locate::dir()`, `head_copy()`, DPAPI),
  opened per call and dropped before a prompt is shown or a restore runs.
- `agentlife pending discard` now withdraws the request from that store. Nothing else a person sees
  changed: `pending approve` still says the prompt is not wired, and `restore` without `--dry-run` still
  refuses.
- **Fake corrected.** `RestorePlan` is `Scope::AnyRequester` in the real store (the plan hash is the
  binding), so the fake no longer narrows an approval by the requester's role, and the test that said
  another role "cannot spend" it now says the plan, not the spender, is what matches. Behaviour of
  `pending::spend_approval` against the real store is unchanged; only the fake's claim was wrong.
- New `consent::real::HelloPrompt` (`Prompt` over `user-request`'s Windows Hello channel). A channel
  `Refused` is `Unavailable`, which leaves the request pending. Not yet called by a command.
- Off Windows every consent call fails closed (the crate's DPAPI refuses), so nothing can be approved.

## 0.12.0

- **Breaking (library API):** the `Consent` trait takes the shape of `user-request` 0.11.1's store
  (RESTORE-GAP-ANALYSIS section 2, PR 3a). `spend_one_use(&mut self, kind, subject, &Requester)`
  returns `Result<String, String>`, the id spent; `Err` means nothing was spent and the restore must
  not run. A new `approved_at(kind, subject, &Requester)` reads when the approval was given, so
  freshness is still judged before the spend. `consent::Spent` and `consent::SpendError` are gone; a new
  `consent::Requester` carries who asks, supplied by the caller.
- `pending::spend_approval` takes a `&Requester`. A restore request's subject is now exactly
  `plan <64 lowercase hex>` (`consent::restore_plan_subject`, `parse_restore_plan_subject`), as the
  real crate requires; it used to be `restore N agents` (the count is still in the summary).
- The fake enforces the same: it refuses a restore-plan subject that is not `plan <hash>` or names
  another plan than the bound hash. (It also said the fake finds an approval by requester role; that
  was wrong for `RestorePlan`, a `Scope::AnyRequester` kind, and was retracted in 0.12.1: the plan hash
  is the binding, not who spends.)
- No real backend yet: `consent::installed()` still reports that none is installed, no dependency was
  added, and nothing a person sees or approves changed.

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
