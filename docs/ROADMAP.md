# agentlife roadmap

Written 2026-10-13 at `origin/main` = `8236bce` (0.12.16); the PR that adds this file is 0.13.0. `PLAN.md` is
the restore planner's reference, not a roadmap, so this is the priority list. Sources: `MILESTONES.md`,
`RESTORE-GAP-ANALYSIS.md` (requirement ids R1 to R19), `RESTART-TOOL-DESIGN.md` 14.13, `CHANGELOG.md`.
Versions are proposals; the PM allocates them. A row's "who" is the only thing that can move it.

## Built (the read-back is the CHANGELOG entry and its tests, not this line)

| area | state |
|---|---|
| M0 to M3 (config, journal, registry, hooks, intent, graceful `park`/`stop`, planner, launcher, `restore --dry-run`) | built, 0.2.x |
| M4 consent: durable pending restore, `pending`, logon/unlock tasks (`install-task`), **real backend over `user-request` 0.11.1**, restore that asks, spends, executes and reports | built, 0.12.1 to 0.12.3; the logon task is **not registered** |
| R3/R11/R12 `import-lane-state` | built, 0.11.5 |
| R14 `LANE_ROLE` on every launch (both launchers) | built, 0.11.4 |
| R16/R17 `doctor` (host and hook binary vs this build) | built, 0.12.5 |
| `lane-restart`: launch spec before any kill, `status`, `start`, `specs import`, `stop`, PM-only restart of another lane, host thread fix | built, 0.12.10 to 0.12.14 |
| the approval prompt's fourth thing (why) | built, 0.12.16 |
| `park`/`stop` of a running lane refused unless the lane can be started again | 0.13.0 (#49) |
| restore report lists every wanted agent not in a batch, with why | 0.13.1 |

## Next, in order (nothing below waits on CireSnave or the PM's hands)

1. ~~Restore report names every Wanted agent it did not start, with the reason~~ (RESTORE-GAP-ANALYSIS 5b): done in
   0.13.1 (`Report.unstarted`; the flaky test's message now prints `held` and `running_now`).
2. **`launch_e2e` flake and issue #14** (Linux `start_time` may differ by 1 s between reads): stay unproven
   until a failing log exists; the PM's ruling is to file an issue with the log, not to skip the test.
3. **`host.rs` main loop drops the final chunk when the child exits** (noted in #44, unfixed): drain `rx`
   before breaking. Size S; needs a ConPTY test (see the quirk in HANDOFF: the fake outer input must carry the
   CPR reply).
4. **One launch description, not two.** `restore` launches from the registry record; `lane-restart start`
   launches from `<role>.launch.json`. They agree today only because both rebuild through an allowlist and pin
   the model. A single resolver (spec first, registry as the fallback) would keep a lane resumable by either
   path with one set of rules. Needs the PM's ruling on which is authoritative before any code (a design
   question, not a lane's to settle); size M.

## Waiting on someone else

| item | who | why it waits |
|---|---|---|
| R2 hook entries on **one** lane, then widened | CireSnave (settings are his) | `HOOK-INSTALL.md`; M1 gate says one lane first |
| R8 register the logon and unlock tasks; R17 install the binary at `.claude-hooks` | CireSnave approves, PM operates | creates a logon task |
| R9 Hello from a logon task (unverified), R15 PATH inside a task, restore canary, real reboot (M6) | CireSnave present | board 173: rows 5 and 6 of the gap analysis |
| R13 two sessions in one role (`auth-framework-deps`) | CireSnave | his files; no restore can bring back uncommitted patches |
| publishing `lane-state`/`lane-restart`/`agentlife`, OverMind's cut-over to the published crate | PM (6.11) | PM's publish authority |
| M5 standing grants, M6 acceptance runs | after the canary | M5 follows M4's first real restore; M6 needs the reboot |
| M7 lazy start | Synapse S-1, the channel adapter, M9 cutover | external dependencies |
| the FAM message-path fixes only | n/a | FAM is to be decommissioned: no features |

## Rules this list obeys

- No live `lane-restart start|stop`, `specs import`, non-dry-run `restore`, or `pending approve|--prompt|discard`
  from a lane. Tests use stand-in processes and `USER_REQUEST_DIR`.
- Every pushed change bumps the version; a breaking change bumps the second number pre-1.0.
- Every launch names Sonnet or Haiku explicitly; Opus is refused, never downgraded silently (the spec and the
  plan both record `model-pinned:<was>`).
