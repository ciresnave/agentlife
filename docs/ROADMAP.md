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
| 1 | **`lane-restart status --json`** (exists since 0.12.10: role, state, spec, startable, since, stop_reason, detail). **Correction (2026-10-14): the earlier text here said `status` was human text only; that was wrong.** Gap closed in 0.14.9 (#60): each role's `model` and a `disk` object (mount, free_bytes, floor_bytes, below_floor) are now in the JSON (they were a stderr line / absent) | The PM's idle sweep reads each lane's state to decide whether to assign work (`PORTFOLIO-ROADMAP.md`, "Idle-lane signal") | small | built, this change |
| 2 | **`lane-restart ledger [--role R] [--since T]`**, read-only | Every start, stop, restart, import and refusal is already written by `log_ledger` to `restart.log`; there is no reader, and PM attribution of who restarted whom is done by hand (`CLAUDE.md` 8: "only the PM's ledger ... attribute") | small | built, 0.14.10 (`lane-restart ledger`) |
| 3 | **HANDOFF lint in `assert-idle`**: warn (not refuse) when the lane's HANDOFF is over 8 KB, or has no dated rewrite line | The ceiling is 20 KB (PM) / 8 KB (lane) by `CIRESNAVE-EXPECTATIONS.md` 2.3c and is measured by hand with `wc -c`; `PM-HANDOFF.md` reached 321 KB of stacked dated blocks | small | built (warn-only; PM ruling 2026-10-14) |
| 4 | **Port-or-retire of the non-Rust pieces of the OverMind split that land here: a MEASUREMENT, no code.** **Measured 2026-10-14 at `origin/main` = `2ca6368`** (`git ls-tree -r --name-only`, by extension): 118 tracked files, 79 `.rs`, 24 `.md`, 5 `.toml`, 1 each `.yml` `.lock` `.json` `.gitignore`, 6 with no extension, and **0 `.py` `.sh` `.ts` `.js` `.ps1`**; positive control: the same listing finds the 79 `.rs`. So this repo holds no non-Rust code to port today. **Still unmeasured:** which non-Rust OverMind pieces are slated to land here. `PORTFOLIO-ROADMAP.md` (the OverMind-split entry, 2026-10-08) names a destination for `lane-restart` (here) and for `user-request`/`with-secret` (their own repository), but not for `tools/spdx.py`, `tools/preflight.py`, `tools/gate_fleet.py` or `probe/` (31 Python/shell/TypeScript/JSON files, "moving out"). **Ruling (the PM, 2026-10-14 11:48Z, by peer message, not CireSnave): "leave in place (not Rust code, not ours to move without a roadmap decision)".** `tools/*.py` and `probe/` stay in OverMind; nothing is assigned to this repo, so there is nothing here to port. Closed unless a roadmap decision moves them | CireSnave's caveat (2026-10-08, board 143): non-Rust pieces get "porting them to Rust" on the roadmap | none | nothing: closed by the PM ruling above |
| 5 | **Specs coverage**: after the PM's `specs import`, report which roles still have no complete spec and why, as one table in `status` | `restore` launches from the spec and falls back to the registry; until imports run most plans say `registry` (item 4 in "Next, in order") | small | built, 0.14.11 |
| 6 | **Linux `start_time`** (issue #14) | Parked by the PM until a failing log exists | n/a | a failing log |

### `specs import`: cwd-missing and Crashed roles (PM question 2026-10-14 10:37Z)

What the code does at `0842e3e` (`crates/lane-restart/src/spec_import.rs`, `import_one`), read, not run: a role is
**refused with its reason** when it has no `launch_args`, when its `cwd` is not a directory (`cwd missing: <path>`),
when the model resolves to Opus, or when the spec fails its own check; otherwise it is **written**, as `Running`
if its recorded process is live and as **`Crashed` if not**. So a Crashed role whose `cwd` exists *is* written
(startable by the PM's `start`); a role that is only Crashed-and-not-written must have been refused for one of the
reasons above or already have a spec. The 11 "Crashed, not written" roles in the PM's report therefore need their
printed reason rows read; I have not seen them.

Proposals (PM to rule):

1. **cwd missing: keep listing it, group it.** Dropping a row silently breaks the rule that every refusal is
   listed (the module header says "never dropped"); a quiet skip would also hide a role whose directory was
   moved. Instead print the refused rows **grouped by reason**, one line per reason with a count and the role
   names, so ~30 vanished mlmf worktrees read as one line, not 30. Exit code and the per-role rows stay available
   (`--verbose`). Small, no behaviour change to what is written.
2. **cwd missing means prune, never resume.** A spec for a directory that is gone cannot start. The honest
   outcomes are *recreate the worktree* (then re-import) or *retire the role*. Retiring is an edit to the lane
   state directory, which `specs import` promises never to make, so it belongs in a **separate PM-only**
   `lane-restart specs retire --role R [--dry-run]` that **moves** `<role>.json` to `.lane-state/retired/` (never
   deletes) and writes a ledger row. Not built; needs the PM's ruling that retiring is wanted.
3. **Crashed with a live cwd: resume, not prune.** It is imported as `Crashed` and is what `start` is for; the
   PM decides per role, the tool does not auto-start.
4. **Import should say what it did, first and last.** The PM's 2026-10-14 10:36Z sequence was `specs import
   --dry-run` (all roles, "would write" for five), then `specs import --role <r>` for those five, which wrote
   them (`written_by: import`, `written_at 10:36:14Z` to `10:36:16Z`); the labels were correct. (An earlier
   revision of this paragraph read that run as a mislabelled dry run; that was wrong, retracted on the PM's
   command log.) The proposal stands without it: with ~40 rows, a relayed report should be unmistakable about
   whether anything was written, so print `DRY RUN - nothing written` or `WROTE n spec(s)` as the **first and
   last line**.

### Not proposed, on purpose

- Anything that starts, stops or restarts a running lane from this lane: PM-only by the tool's own check.
- M5/M6/M7: they wait on CireSnave and the first real restore (see "Waiting on someone else").
- New SUP-008 work: `pin-plan` is done at 0.14.5; applying the patches is each lane's PR.

### What I could not establish from `PORTFOLIO-ROADMAP.md`

The file is 55 KB; I searched it for `agentlife`, `lane-restart`, `lane-state`, `restart`, `ledger` and `status`
and read the hits (lines 486 to 505, 558 to 566). A request for agentlife that does not use those words would not
be found by that search, so this slice is a list of what the file says, not a census of what lanes want. Lanes'
own asks belong in a `[TASK]` or an issue.

## Next slice 2 (proposed 2026-10-14 at `origin/main` = `ad11f58`, 0.14.13): what the PM's sweep still does by hand

Sources, read at the same date: `PORTFOLIO-ROADMAP.md` (the "Idle-lane signal" entry, section 4 "When your board hits
zero", the session-restart-tool entry) and `CIRESNAVE-EXPECTATIONS.md` 2.3b/2.3c as cited above; the code at
`ad11f58` (`crates/lane-state/src/state.rs`, `crates/lane-restart/src/spec.rs`, `main.rs`). Slice 1 above is all
built or closed. Every row is a **proposal for the PM to accept, reorder or strike**; none is started; versions
are the PM's. This is a list of what those documents and that code show, not a census of what the PM does by
hand: the PM's own sweep script (`tools/pm-status.py`) is not in this repo and I have not read it.

| # | item | evidence | size | waits on |
|---|---|---|---|---|
| 1 | **`status --json` carries each role's activity**, read from its lane-state file: `busy`, `subagents_running`, `no_background_shells`, `updated_at`, `updated_by_event`. Only `busy` and `subagents_running` are reported for a role whose recorded process is live (`state_process_is_live`); a dead process reports none, so a lane killed mid-turn cannot read as busy. Raw fields, no verdict | `LaneState` already records all five (`state.rs`, written by the hooks) and `RoleStatus` exposes none of them (`spec.rs`: role, state, spec, startable, since, stop_reason, detail, model). The PM's idle sweep reads a lane's free-text `set_summary` line, and `PORTFOLIO-ROADMAP.md` section 4 says that "a stale summary is indistinguishable from an idle lane from outside". The hook-written fields are the one idle signal that is not self-reported | small | the PM: does the sweep want these fields, and which "idle" it means (below) |
| 2 | **An `idle` verdict only if the PM names the rule.** `authorize.rs` already decides "may another lane be restarted" from `busy`, `subagents_running` and `no_background_shells`; `status` could print that same decision as one boolean per role, so the sweep and the restart gate cannot disagree | the roadmap's idle rule is `doing=idle ... waiting=-` on the summary line, a different predicate from the gate's. Two definitions of idle is the defect "a number standing in for a set" names | small | the PM's ruling on which predicate is "idle"; I will not pick it |
| 3 | **HANDOFF size per role in `status`**: bytes of the role's HANDOFF file against its ceiling (8 KB lane, 20 KB PM), as a column and in `--json` | 0.14.12 warns only the lane itself, at its own `assert-idle`; `PM-HANDOFF.md` reached 321 KB of stacked blocks with nobody measuring it from outside (`PORTFOLIO-ROADMAP.md`, `CIRESNAVE-EXPECTATIONS.md` 2.3c). Read-only, one `stat` per role | small | the file name per role is **unverified**: this lane's is `HANDOFF.md` in its cwd and the PM's is `PM-HANDOFF.md`; I have not read another lane's tree. A role with no such file reports "none", never 0 |
| 4 | **`status --since` or a staleness mark**: a role recorded `Running` whose `updated_at` is older than a threshold is flagged `quiet for N h` | the sweep's backstop exists "for expired subscriptions and silent stalls" (idle-lane entry); a stall is the one case the hook fields cannot show by themselves, because no event fires | small | the threshold is the PM's; derived from item 1, no new data |
| 5 | **`ledger --by-lane` summary**: counts of acted and refused rows per role over `--since`, one table | `ledger` (0.14.10) prints rows; the PM's attribution of "who restarted whom" is a count over them. Not built because I have not seen the PM do this count; it may already be a one-liner over `--json` | small | evidence that a table is wanted: drop this row if the PM says no |

### Importing the roles that have no launch spec (PM task 2026-10-14 12:03Z)

**Measured 2026-10-14 ~12:05Z, read-only**: the installed `lane-restart` 0.14.8 `status` (so no coverage
table) plus a read of every `<role>.json` in `C:/Projects/.lane-state` that has no `<role>.launch.json`, testing
`launch_args` and `os.path.isdir(cwd)` the way `spec_import::import_one` is described above. I did not run
`specs import` (PM only). **Population: 30 roles, all `Crashed`, none with a spec** (the same script skips the
roles that do have one, so it is not counting everything). The PM's relayed "11 Crashed, ~30 cwd missing" is a
different predicate at an earlier time (after the 10:36Z write of 5); these are the numbers now.

| group | n | roles | what `specs import` would do |
|---|---|---|---|
| `cwd` missing | 18 | the `mlmf` worktrees: `codacy-refactor`, `exl2-spec-correction`, `fuel-gap-plan`, `fuel-migration-inventory`, `generation-config-special-tokens`, `gguf-config`, `gguf-type-code`, `hf-tokenizer`, `metadata-entries`, `mlmf-awq`, `mlmf-awq-plan`, `mlmf-exl2`, `mlmf-gguf-arch`, `mlmf-gptq`, `mlmf-gptq-quant-method-fix`, `mmap-zero-copy`, `quant-formats-design`, `source-hub` | refuse each: `cwd missing`, one grouped line |
| `cwd` exists, **a sub-directory of another project** | 9 | `api`, `web`, `shared` (under `ThinkersJournal-Community`); `backend`, `frontend`, `routes`, `worker` (under the `HumboldtUnifiedKidTracker` `backend-foundation` worktree); `docs` (`agentlife/docs`); `mlmf-gguf` (`mlmf/crates/mlmf-gguf`) | **write** a spec and mark it startable |
| `cwd` exists, a worktree or repo root | 3 | `hf-config`, `humboldtunifiedkidtracker`, `synapse-restarttest-lane` | write a spec, startable |

**The 9 sub-directory roles are probably not lanes, and importing them is the risky half.** The state hook
falls back to "the current directory's leaf name" for a role when `LANE_ROLE` is unset (`assert-idle`'s own
usage text says so), and every name above is the leaf of a sub-directory a session happened to `cd` into
(`apps/api`, `packages/shared`, `agentlife/docs`). `specs import` cannot tell that from a lane, so it would mark
all 9 `startable`, and a `start` would launch a second session in a directory whose project already has a lane
(`docs` is inside this repo's tree; `web`, `api` and `shared` are inside `thinkersjournal-community`'s). This is
inference from the names and cwd paths, **not read from any session**; I have not asked those lanes. Proposals
for the PM to rule:

1. **Retire the 18 `cwd missing` roles** (`specs retire`, which moves the state file to `retired/`, writes a
   ledger row, and has a `--dry-run`). A directory that is gone cannot start; recreating 18 worktrees to
   resume sessions last active 2026-10-02 to 03 is a larger decision than an import. I have not seen the real
   `retire --dry-run` output (HANDOFF: unverified).
2. **Do not import the 9 sub-directory roles; retire them too**, after the PM (or each project's lane) confirms
   none is a session someone wants resumed. Ask first, because a retire is the same reversible move but a
   wrong import is a duplicate session.
3. **The 3 worktree/repo-root roles**: PM decides per role. `synapse-restarttest-lane` has a live peer at that
   cwd right now (a `claude-peers` row, `list_peers` 12:00Z) while its state file says `Crashed`, so its pid is
   not the live session's: read that before any `start`.
4. **A code change worth making either way (small): `specs import` and `status` print a fourth group,
   "cwd is inside another role's cwd"** next to `cwd missing`, so the sub-directory roles are named as a class
   in the report and the PM does not have to find them by reading paths. It would only *report*; legitimate
   nested lanes (`hf-config` is a worktree under `mlmf`) are in the same class, so it cannot decide, which is
   why it is a label and not a refusal.

### Not proposed, on purpose

- **Subscribing to lanes, or any `SendMessage notify_when_idle` wiring**: that is the PM's session tooling, not
  a state-directory reader; this repo does not send messages to peers.
- **Anything that writes another lane's state or HANDOFF**: items 1 to 4 read only.
- **Assigning work to idle lanes**: a decision, not a tool; the PM's.

## Next slice 3 (proposed 2026-10-14 at `origin/main` = `d396313`, 0.14.18): what is still done by hand, and the OverMind split's last step

PM task 2026-10-14 13:50Z: (a) what lanes and the PM still do by hand around restart/restore/status after
0.14.18, (b) OverMind split steps 6-7. Every row is a **proposal for the PM to accept, reorder or strike**;
none is started; versions are the PM's. Sources were read at the refs named; this is a list of what they show,
not a census of what the PM does.

### (a) By hand today, around restart / restore / status

| # | item | evidence (ref, what was run) | size | waits on |
|---|---|---|---|---|
| 1 | **`lane-restart --self` as documented does not run.** `C:\Projects\CLAUDE.md` and `CIRESNAVE-EXPECTATIONS.md` 2.3b say a lane runs `lane-restart --self`; the argument parser requires `--role <name>` (`ArgError::RoleRequired`, `main.rs`), so the bare form exits 1 with `--role <name> is required`. `assert-idle` resolves the role itself (`--role`, then `LANE_ROLE`, then the cwd's leaf). Fix either way: `--self` resolves the role the same way `assert-idle` does, or the instruction becomes `--role <me> --self` | measured once, 2026-10-14 ~13:05Z, this lane, installed 0.14.15 exe, from this lane's cwd with `LANE_ROLE` unset (the error text above). **I did not measure a lane whose `LANE_ROLE` is set**, so "fails for every lane" is not claimed. A role file `agentlife.json`/`agentlife.launch.json` exists, so the earlier belief that this lane has no role was wrong: the call was wrong | small | the PM: which side changes (the tool, or `CLAUDE.md`, which is the PM's) |
| 2 | **Installing a new `lane-restart.exe` is a hand ritual.** The PM builds, renames the running exe aside and copies the new one (spec 5.3: "rename-then-copy, the old kept as `<name>.<version>.prev`") | `C:\Projects\.claude-hooks` listing 2026-10-14: 13 `lane-restart*` files under seven naming patterns (`.bak`, `.prev`, `.old`, `.old-in-use`, `.old-0.12.x`, `lane-restart.<ver>.exe`, `.sha256`) and a `lane-restart.sha256` last written Oct 7, which no longer describes `lane-restart.exe` (Oct 14 05:35). `agentlife doctor` already compares hook and host binaries to the build | medium | **ruled yes by the PM 2026-10-14; built, 0.14.21** (the one kept file is `lane-restart.exe.bak`, not `<version>.prev`, so there is never more than one): a PM-only `lane-restart install --from <exe>` that would keep exactly one backup, verify the candidate by `--version` and `status` before replacing, and rewrite the sha file. It would touch `.claude-hooks`, so it is PM-only like `start` |
| 3 | **Role and peer are joined by eye.** `status` knows each role's cwd and activity; the broker knows each live session's cwd and free-text summary; the PM's sweep reads both | 2026-10-14 ~12:00Z: `synapse-restarttest-lane` was `Crashed` in `status` while `list_peers` showed a live session at the same cwd. `agentlife` already has a broker client (`peers.rs`, used by `restore`) | small | whether the broker is stable enough to depend on from a read-only `status --peers` (it is read-only, fails soft to "unknown"); built in 0.14.23 as `status --peers` (PM ruling 2026-10-14; measurements in `docs/PROPOSAL-STATUS-PEERS-JOIN.md`) |
| 4 | **Merged-branch and worktree cleanup is a three-step ritual per PR** (`CLAUDE.md` 3: merge, delete ref, verify absent; then `git worktree remove` and `prune`) | `C:\Projects` holds 37 directories whose names start `fuel-`, `tjc-`, `kiss-`, `auth-framework-` or `synapse-wt`, or contain `-wt-` (`ls -d */`, 2026-10-14); **I did not check which are registered worktrees or merged**, so how many are stale is unmeasured. `CLAUDE.md` 3 itself cites 54 registered worktrees in one repo (measured 2026-10-02 by the PM) | small, read-only | not agentlife's by charter (it is neither restart, restore nor status). Listed because the PM asked what is done by hand; a report `agentlife worktrees --stale` using the section 4 predicate (`gh pr list --head <b> --state merged` plus `git ls-remote`) would never delete anything. Strike unless wanted |
| 5 | **The logon and unlock tasks are not registered** (R8) | Roadmap "Waiting on someone else"; built 0.12.x, unregistered | n/a | CireSnave approves, the PM operates |

Not proposed: anything that acts on another lane's session or state; `restore` automation beyond what M4 built.

### (b) OverMind split: where steps 6 and 7 stand (read at OverMind `origin/main` = `ca9e208`, consent `origin/main` = `62b1788`)

**Step 6 is already done.** `ARCHITECTURE-LAYERS-SPEC.md` 5.4 step 6 says so and the history agrees: `ciresnave/consent`
was created with `git filter-repo` (the imported commits, for example `83ad6cf`, are followed by the scaffold `177d306`; `45a6856` 0.12.0
verified the import byte-equal to OverMind `57fae94`), and OverMind#137 (`56764a2`, 0.13.0) deleted
`crates/user-request`, `crates/with-secret` and the `rust` CI job. Two crates, one repo, one version, history kept.
**What is left of step 6 is not a move:** `with-secret` is unpublished and waits on CireSnave's per-crate approval
(board 173-A, per the spec and consent's README); the OverMind README points at the new home. So a plan for
"steps 6-7" is a plan for step 7 only.

**Step 7: the licence-tooling sources and the measurements home.** Measured by `git ls-tree` and `git show | wc -c`
at `ca9e208` (the byte counts are of the blobs):

| piece | files | size | goes with |
|---|---|---|---|
| `tools/spdx.py`, `tools/preflight.py`, `tools/gate_fleet.py` | 3 Python | 23,182 + 28,836 + 11,628 B | licence tooling |
| `.github/workflows/gate-fleet.yml` | 1 | 4,002 B | licence tooling (it runs `OverMind/tools/gate_fleet.py`) |
| `tests/test_spdx.py`, `tests/test_gate_fleet.py` | 2 | not measured | licence tooling (both `import` the tool by bare module name) |
| `.github/spdx_gate.py` | 1 | 36,612 B | **stays** (OverMind's own deployment; `ci.yml` `spdx` job) |
| `probe/` (34 files: 30 `.py`, 2 `.ts`, 1 `.sh`, 1 `.json`), `research/` (2), `MEASUREMENTS.md` | 37 | `MEASUREMENTS.md` 142,888 B | measurements home |
| the six probes that import `overmind` (`daily_limits`, `free_capacity`, `ledger_context`, `local_throughput`, `p1_bench`, `paper_summaries`) | 6 of the 34 | | **stay** (ruling 4, 2026-10-08) |
| `tests/test_desktop_grade.py`, `tests/test_paper_summaries.py` | 2 | not measured | `test_paper_summaries.py` tests `probe/paper_summaries.py`, one of the six that stay, so it stays too; `test_desktop_grade.py` goes with the probe home |

Positive control for "six probes import overmind": the same `git grep -l "import overmind\|from overmind" -- probe`
lists exactly those six names; no other file under `probe/` matches.

**Finding that changes the plan: the gate fleet is already five, not four.** `.github/spdx_gate.py` exists at
`origin/main` in `OverMind`, `consent`, `kiss-ref`, `synapse` and `vulkane` (the same listing over every
`C:\Projects\<dir>` with a `.git`; `synapse-wt-redb` is a worktree of synapse and is not counted). `gate-fleet.yml`
checks out OverMind, kiss-ref, vulkane and synapse: **consent is not compared.** `gate_fleet.py`'s own docstring names
the trigger: "when a FIFTH repo gets this gate ... hand-copying stops being defensible". Only repos with a local
clone were listed, so a sixth copy in a repo with no clone here would not show. This should be settled before the
tooling moves, because the move rewrites that workflow.

**Proposal: two destinations, as the spec's 5.2 already says, named now.**

| piece | proposed repo | why that name |
|---|---|---|
| licence tooling | `ciresnave/licence-tools` (crate-less; Python, as it is today) | it is the sweeper (`spdx.py`), its pre-flight (`preflight.py`) and the fleet check; it is not OverMind's job. The CLI names stay (`spdx.py`, `preflight.py`, `gate_fleet.py`) so every recorded command line keeps working |
| measurements | `ciresnave/measurements` | evidence, not product; `MEASUREMENTS.md` is cited by section number from OverMind's own source (25 lines in 13 files under `src/overmind` and `tests`, `git grep -c MEASUREMENTS`), so the file name and section numbers must not change |

Version rule (ruling 3, the higher of source and destination): both repos are new, so each takes OverMind's current
**0.13.2** only if the PM wants the number to stay readable; neither ships a binary, so there is no installed
number to keep. That is the PM's call; I propose each starts at **0.1.0** because nothing installed carries the
OverMind number. (If the PM prefers ruling 3 read literally, 0.13.2.)

**Move order, history preserved, nothing executed:**

1. **Settle the fleet first (in OverMind, before any move).** Either add consent to `gate-fleet.yml` (one more
   checkout and argument; the planted-divergence control stays) or rule that consent's copy is checked another
   way. This is the one step that changes behaviour, so it goes alone and first.
2. **Create `licence-tools`** from OverMind with `git filter-repo --path tools/spdx.py --path tools/preflight.py
   --path tools/gate_fleet.py --path tests/test_spdx.py --path tests/test_gate_fleet.py --path
   .github/workflows/gate-fleet.yml`, plus scaffold: licences, CI (the python tests), its own SPDX gate as a
   required context from day one (spec 5.3: "each new repo should require its own"), branch protection that
   requires CI before the second PR. The two test files `sys.path`-insert `tools/`; each inserts `../tools` into `sys.path`, so one path edit each.
   Verify as consent was: the copied files byte-equal OverMind `ca9e208`.
3. **Repoint the fleet workflow** in `licence-tools` (it runs daily and on changes to the gate): `OverMind/tools/
   gate_fleet.py` becomes the repo's own `tools/gate_fleet.py`, and the four (five) deployments are checked out
   beside it. Until this lands and runs green once, OverMind keeps its copy and its workflow (rollback is "do
   nothing").
4. **One OverMind PR deletes** the three tools, two tests, the workflow and leaves a one-line pointer in README
   (spec 5.3). The branch-protection note in 5.3 applies if `gate-fleet`'s job name is a required context: **not
   measured**, read the protection before the PR.
5. **Create `measurements`** the same way (`probe/` minus the six, `research/`, `MEASUREMENTS.md`,
   `tests/test_desktop_grade.py`). Two blockers named by the spec: `probe/desktop_grade.py` runs through the installed
   `with-secret.exe` and reads the runbook at pinned `REF` `3bf513f` (`CHANGELOG` 0.13.0), so it needs the
   runbook's new address in consent; the pinned ref is an OverMind commit and **has not been checked to exist in consent's
   history** (consent's runbook is byte-identical per its README, but the commit id differs).
6. **OverMind's source comments** cite `MEASUREMENTS.md §n` and `probe/ledger_context.py`: after step 5 they cite a file that
   is elsewhere. A one-line pointer in README is not enough for those; either leave the file in OverMind with a
   pointer, or accept dangling section citations. **Open question for the PM**, not a default.

**[ASK]-ready (for the PM to put on the board): `spdx_gate.py` is deployed in five repositories and the fleet check
compares four.**

- *Situation* (measured as above, `origin/main` of each local clone): OverMind, consent, kiss-ref, synapse, vulkane.
  `gate-fleet.yml` checks out four of them; consent is not compared. `gate_fleet.py` was built because on 2026-09-11
  four copies all printed PASS while one lagged, and its own docstring names the trigger for ending hand-copying:
  "when a FIFTH repo gets this gate". consent got its copy on 2026-10-12 (scaffold `177d306`); nothing has
  compared it since.
- *Options.* **A**, add consent to `gate-fleet.yml` (one checkout, one argument) and keep hand-copying; the
  detector covers five again, and the docstring trigger is acknowledged, not acted on. **B**, publish the gate as a
  package each repo pins by version, which the docstring calls closing it fully; this needs a registry choice (the
  gate is Python, so not crates.io) and touches CireSnave's rule on depending on published versions rather than
  GitHub. **C**, leave it: consent's copy is checked by nothing.
- *Recommend* **A now**, **B** when a sixth repo takes the gate or any `DIVERGENT` entry needs a second reason (the
  docstring's own second trigger). A is a one-line workflow edit, reversible, and restores the detector today.
- *Blocks:* nothing in agentlife; the licence-tools move (step 1 above) wants A first.

Rollback per step: 1 revert; 2 and 5 delete the new repo (nothing depends on it yet); 3 revert; 4 revert (the files are
still in `licence-tools`). No installed binary is involved anywhere in step 7.

Out of scope here, per the PM's stop line: creating either repo, moving anything, publishing.

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
