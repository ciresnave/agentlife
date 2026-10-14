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
| `restore` launches from the lane's launch spec, registry as the flagged fallback | 0.14.0 |
| `host` relays the child's last output after the ConPTY closes (`drain_tail`) | 0.14.1 |
| disk floor: `lane-restart start`/restart and `agentlife restore` refuse below 20 GB free on the state volume; `status` prints it | 0.14.2 |
| `pin-plan` (SUP-008 planner, read-only): mutable `uses:` refs, SHA of the current ref, latest release SHA, Node runtime now and at latest | 0.14.3 |

## Next, in order (nothing below waits on CireSnave or the PM's hands)

1. ~~Restore report names every Wanted agent it did not start, with the reason~~ (RESTORE-GAP-ANALYSIS 5b): done in
   0.13.1 (`Report.unstarted`; the flaky test's message now prints `held` and `running_now`).
2. **`launch_e2e` flake and issue #14** (Linux `start_time` may differ by 1 s between reads): stay unproven
   until a failing log exists; the PM's ruling is to file an issue with the log, not to skip the test.
3. ~~`host.rs` main loop drops the final chunk when the child exits~~: `drain_tail` in 0.14.1. Not reproduced end to
   end (see CHANGELOG), so a lane that sees a missing last line should file it with the host log.
4. ~~One launch description, not two~~: done in 0.14.0 (the PM ruled option A): `restore` launches from the lane's
   complete `<role>.launch.json` when there is one and from the registry otherwise, and each plan entry says which
   (`source`). Remaining from it: nothing writes specs for lanes that have none except `lane-restart specs import`
   (PM only) and a lane's own next launch, so until the PM runs that import most plans still say `registry`.

## Portfolio items from the PM (2026-10-14), taken in order

| # | item | state |
|---|---|---|
| 1 | **Disk floor**: `lane-restart start`/restart/`restore` refuse (exit 1, named reason) when `C:` free < 20 GB; `lane-restart status` prints `C:` free. Incident 2026-10-14 04:45Z: `C:` hit 4 GB, builds failed in 6 lanes | built, 0.14.2 |
| 2 | **SUP-008 pin tool**: read-only planner. Workflows enumerated with `git ls-files` (never a disk walk) in every repo under `C:\Projects`; per `uses:` ref that is a mutable tag or branch, the latest release tag's SHA to pin and the action's Node runtime (flag node20); a per-repo table, no writes. Scale per CodeRipper: 235 pins in 18 repos | built, 0.14.3 (#54) |

### Finding from the first real `pin-plan` run (2026-10-14, each repo's `origin/main`)

455 `uses:` in 32 repos: 59 pinned, **393 mutable** (313 tag, 80 branch) in 22 repos. **213 run node20 or
older today** (100 are `actions/checkout@v4`; all declare node24 at their latest release), and **80 are
branch refs** (67 `dtolnay/rust-toolchain@stable`). CodeRipper's 235 pins in 18 repos is a different
population (see `crates/pin-plan/README.md`). Acting on it (writing the pins) is not this lane's: the tool
only plans.

## Next slice (proposed 2026-10-14 at `origin/main` = `0842e3e`, 0.14.5): what other lanes need from agentlife

Source: `PORTFOLIO-ROADMAP.md` (idle-lane signal; session restart tool; the OverMind split) and
`CIRESNAVE-EXPECTATIONS.md` 2.3b/2.3c (restart at every task boundary; HANDOFF size ceiling), read at the same
date. Every row is a **proposal for the PM to accept, reorder or strike**; none is started. Versions are the PM's.

| # | item | why a lane or the PM needs it (evidence) | size | waits on |
|---|---|---|---|---|
| 1 | **`lane-restart status --json`** (same facts as `status`: role, state, launch-spec complete, startable, model, `C:` free vs the floor) | The PM's idle sweep reads each lane's state to decide whether to assign work (`PORTFOLIO-ROADMAP.md`, "Idle-lane signal"); `status` is human text today (`lane-restart --help`), so every consumer parses prose | small | nothing |
| 2 | **`lane-restart ledger [--role R] [--since T]`**, read-only | Every start, stop, restart, import and refusal is already written by `log_ledger` to `restart.log`; there is no reader, and PM attribution of who restarted whom is done by hand (`CLAUDE.md` 8: "only the PM's ledger ... attribute") | small | nothing |
| 3 | **HANDOFF lint in `assert-idle`**: warn (not refuse) when the lane's HANDOFF is over 8 KB, or has no dated rewrite line | The ceiling is 20 KB (PM) / 8 KB (lane) by `CIRESNAVE-EXPECTATIONS.md` 2.3c and is measured by hand with `wc -c`; `PM-HANDOFF.md` reached 321 KB of stacked dated blocks | small | PM ruling: warn or refuse |
| 4 | **Port-or-retire of the non-Rust pieces of the OverMind split that land here** | CireSnave's caveat (2026-10-08, board 143): non-Rust pieces get "porting them to Rust" on the roadmap; `PORTFOLIO-ROADMAP.md` lists them under the split. This lane owns only what moves here | unknown: not measured | the split's step 3 onward (PM) |
| 5 | **Specs coverage**: after the PM's `specs import`, report which roles still have no complete spec and why, as one table in `status` | `restore` launches from the spec and falls back to the registry; until imports run most plans say `registry` (item 4 in "Next, in order") | small | the PM's `specs import` (dry run read first) |
| 6 | **Linux `start_time`** (issue #14) | Parked by the PM until a failing log exists | n/a | a failing log |

### Not proposed, on purpose

- Anything that starts, stops or restarts a running lane from this lane: PM-only by the tool's own check.
- M5/M6/M7: they wait on CireSnave and the first real restore (see "Waiting on someone else").
- New SUP-008 work: `pin-plan` is done at 0.14.5; applying the patches is each lane's PR.

### What I could not establish from `PORTFOLIO-ROADMAP.md`

The file is 55 KB; I searched it for `agentlife`, `lane-restart`, `lane-state`, `restart`, `ledger` and `status`
and read the hits (lines 486 to 505, 558 to 566). A request for agentlife that does not use those words would not
be found by that search, so this slice is a list of what the file says, not a census of what lanes want. Lanes'
own asks belong in a `[TASK]` or an issue.

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
