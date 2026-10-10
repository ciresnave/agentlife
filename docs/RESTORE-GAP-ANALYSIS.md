# What `agentlife restore` lacks to bring the fleet back after a reboot, unattended

Task from the PM, 2026-10-10 (CireSnave asked for it). **Analysis only: nothing was launched, installed
or written outside this worktree.** Ref for every "at" below: `origin/main` = `037c7fe` (0.11.3). Live-box
facts were read 2026-10-10 about 14:00-14:40Z, read-only.

**Ground truth.** After the overnight reboot the PM restored 9 lanes by hand with one ~20-line PowerShell
script (`launch-lane.ps1`: `wt.exe -w new -d <cwd> lane-restart host --role <role> -- claude "read <role>
HANDOFF and continue" --name <name> --model sonnet [--permission-mode m] --dangerously-load-development-channels
server:claude-peers`, session-identity env vars removed, 25 s apart). All 9 reached `list_peers` within about
60 s on Sonnet. So the **launch half is proven**. What agentlife lacks is everything around it: knowing
whom to start, being allowed to, being started by the machine, and not starting what should stay down.

## 1. Requirements

Status: **built** (works, reachable), **built-unreachable** (code and tests exist, no command or
wiring reaches it), **missing**. Size S/M/L is relative, as in `MILESTONES.md`.

| # | requirement | status at 037c7fe | evidence | size | needs CireSnave? |
|---|---|---|---|---|---|
| R1 | A roster to read: registry of agents with cwd, name, mode, launch args | **missing on this box**; code built | `C:\Projects\.agentlife` does not exist (`Test-Path` False). The same session finds `C:\Projects\.lane-state` (46 files with a cwd) so the probe works. Registry code: `src/registry.rs`; filled only by `hook SessionStart` (`src/hook.rs:285-301`) | see R2/R3 | no |
| R2 | Hooks installed so the registry fills itself | **missing** (`HOOK-INSTALL.md` is a procedure, nothing applied) | PM's grep of `~/.claude/settings.json` and `C:\Projects\.claude\settings.json` found no agentlife entry; `C:\Projects\.claude-hooks` holds no `agentlife.exe` (`ls`, with `lane-restart.exe` listed beside it as control). Hooks only fire on a **new** session, so a running lane is not recorded until its next restart | S (apply) | **yes**: settings are his; `MILESTONES.md` M1 gate says one lane first |
| R3 | Bootstrap the registry from `.lane-state` instead of waiting for R2 | **missing** | `grep -n "lane_state" src/*.rs`: only `config.rs`, `down.rs`, `readiness.rs` (all read a session's *current* state for a stop); no importer. `lane_state_dir` is read by `readiness::read_lane_state` and nothing builds an `AgentRecord` from it | M | no (reads his files, writes agentlife's own home) |
| R4 | Restore must not depend on the state file being fresh | **built in agentlife; broken in `lane-restart --role`** | `crates/lane-restart/src/authorize.rs:18` `STALE_STATE_AFTER = 120 s`, enforced at `:163` for any `Target::Other` or self restart; reproduced: `refused: overmind: state file is 52101s old, refusing as stale` (`restart.log` 13:52:01Z). agentlife's own paths have no age check: `plan::build` reads the registry and process table only (`src/plan.rs:337-`), `launch::build_tab` and `start_settled` take no state file (`src/launch.rs:142`, `:365`); `grep -n "updated_at\|age\|stale" src/launch.rs src/restore.rs src/readiness.rs` hits in launch.rs and restore.rs are all the substring of "agent"; only `readiness.rs` reads `updated_at`, for the stop sequence | S (nothing to change in agentlife; do NOT route restore through `lane-restart --role`) | no |
| R5 | A real command that starts the plan | **built-unreachable** | `src/main.rs:179-184` prints "starting agents is not built yet" and exits 1 before reading anything; `restore::execute` exists (`src/restore.rs:368`) and is called by no command | M | no |
| R6 | A real consent backend | **built-unreachable**, now unblocked | `src/consent.rs:232` `installed()` returns `Err(NO_BACKEND)`. `user-request` **0.11.1 is on crates.io**: `cargo info user-request` prints it; crates.io API: one version, created `2026-10-09T21:09:56Z`, not yanked, rust-version 1.95. `KindId::RestorePlan` exists (`request.rs:60`), max grant `OneUse` (`:86`), scope `AnyRequester` (`:97`). Detail in section 2 | M | no |
| R7 | The person sees what they approve | **gap in the design as it will wire** | the crate's prompt is three lines: `Who: ...`, `Wants: restore lanes: plan <64-hex>`, `Duration: ...` (`channel.rs:83-88`). `summary` is **not** in it. The list of agents is only in `agentlife pending --prompt` | S | **yes**: is approving a hash alone acceptable, or must the control-tab text be shown first? |
| R8 | The logon task and its preconditions | **built-unreachable** | `src/task.rs`, `docs/TASKS.md`; registration and `schtasks /Create` probe measured 2026-10-08. No task exists: `Get-ScheduledTask` returns 214 tasks, 17 matching `Update` (positive control), **0** matching `agentlife\|lane\|claude\|Projects`. The PM's empty result was real | S (register) | **yes**: creates a logon task |
| R9 | Hello can be asked from a logon task | **unverified** | `hello.rs` defaults to `Owner::Foreground` (`GetForegroundWindow`); measured only "from a lane's Bash tool GetConsoleWindow is null" (its own comment). Whether Hello shows, and has a foreground owner, from a task at logon before the desktop settles is not known | canary | **yes** (he must be present) |
| R10 | Restart the PM too, but never because a lane asked | **partly built** | plan puts the PM first and alone (`plan.rs:462`); the PM is the entry named `pm` whose launch cwd is `C:\Projects` (`marks.rs:37-44`, `config.rs:190`). The logon task is not a lane, so it is allowed to ask. **Not built:** agentlife refusing `restore` when the caller is a lane (`CONSENT.md` "Who asks" says so; `run_restore` has no `caller::detect`). A person at a terminal and the task are the only legitimate callers | S | no |
| R11 | "Restore on reboot: yes/no" per agent | **built**, unused | intent `park` / `unpark` (`src/cli.rs:342`, `:383`); `plan.rs` skips every non-`Wanted` agent with reason `NotWanted` (test `closed_and_lazy_agents_are_never_candidates...`). No new field needed; what is missing is **someone running `park`** once, and an import (R3) that can set it | S | no (his choice of which lanes) |
| R12 | Filter out non-project sessions | **built** for the plan; **missing** for an import | plan refuses a cwd that is missing or outside the portfolio root (`plan.rs` test `cwd_must_exist_and_be_under_the_portfolio_root`, `:1395`). Census of `.lane-state` below: 1 of 16 recent files is outside the root (`synapse-restarttest-lane`, cwd under `AppData`). The importer must filter before it writes | in R3 | no |
| R13 | Two sessions in one cwd and one role | **built for agentlife**, lost for any `.lane-state` import | agentlife keys a record by name + cwd and rejoins a stopped one (`hook.rs:169-205`, rule 3), so `auth-framework` and `auth-framework-deps` are two records. `.lane-state` is one file per **role**, so a second session in the same role overwrites the first: only one of the two can ever be imported. The `deps` session (private local patches) is invisible to R3 | n/a | **yes**: which session is the lane, and do the patches survive a reboot? (his files, not ours) |
| R14 | The launch must not inherit a lane's role | **missing, newly found** | see section 3 | S | no |
| R15 | Resolve program paths for a task's environment | **likely fine, unverified** | defaults are bare `lane-restart`, `claude`, `wt` (`config.rs:196-197`); on this box `Get-Command` finds `C:\Projects\.claude-hooks\lane-restart.exe`, `C:\Users\cires\.local\bin\claude.exe` and `WindowsApps\wt.exe` on the user PATH. A task runs with the user's PATH, but I could not prove it without running one | canary | no |
| R16 | Installed host is the one agentlife is built for | **unverified** | on disk `lane-restart.exe` reports `0.7.1`; this workspace's crate is 0.11.x. `host` is what answers the dev-channels dialog (the PM saw no human typed). Nothing in agentlife pins a host version | S | no |
| R17 | The agentlife binary installed at a fixed path | **missing** | no `agentlife.exe` in `.claude-hooks`; no release build in the shared tree (`ls target/release/agentlife.exe`: not found). The task and hooks both point at `C:/Projects/.claude-hooks/agentlife.exe` (`HOOK-INSTALL.md`) | S | no (install by rename is built, `src/install.rs`) |

### The roster census (what R3 would import), 2026-10-10 14:30Z

`C:\Projects\.lane-state\*.json`, read-only, one parse per file (script kept in the scratchpad):
46 files carry a `cwd` (`model-policy.json` is the one without, a shared setting, not a lane).
**16 were touched within 48 h** of 14:30Z; 15 of those are under `C:\Projects` with an existing cwd
(`synapse-restarttest-lane` is the one that is not). All 46 have `launch_args`. 11 more files are older
than 48 h but still point at an existing directory (`mlmf-awq`, `fuel-gap-plan`, task-named leftovers of
worktree sessions): an age window is therefore **not** optional in the import, and "touched in the last
48 h" is a filter on *recent activity*, not on liveness (every one of these processes is dead after a
reboot; the 120 s rule in R4 is about a different question, "is this the process I am about to kill").

## 2. Wiring `user-request` 0.11.1: what is left

Read from the published tarball (`user-request-0.11.1/src`), not from the OverMind tree.

| piece | agentlife's trait today (`src/consent.rs`) | the real crate | what must change |
|---|---|---|---|
| spend | `spend_one_use(&mut self, id, bound_hash) -> Result<Spent{approved_at, bound_hash}, SpendError>` | `Store::spend_one_use(&mut self, kind: KindId, subject: &str, requester: &Requester) -> Result<String, String>` (`store.rs:807`). `Err` means nothing was spent and the action must not run | Re-shape the trait (a **breaking** change to the library API: second number). Implementation: `find(kind, subject, requester)` first to read `approval.approved_at` for the 5-minute freshness rule (`pending.rs:40`, `:465`), then `spend_one_use`, both under the store lock the open `Store` holds. The real return is an id only; `Spent.bound_hash` is the subject's hash and must be re-derived from `subject` (`parse_restore_plan_subject`) |
| subject | free string | exactly `plan <hash>`, lowercase hex, `restore_plan_subject()` refuses anything else (`request.rs:148-165`) | `pending::create` must build it with the crate's function, not by hand |
| requester | `role: String` | `Requester{role, session_id, claude_pid, claude_start_secs, managed}` supplied **by the caller**: grep for `Requester {` in `src` finds only tests, one `nobody()` for store-made attempts and the type itself, so there is **no** constructor from the process table in 0.11.1 despite the README's wording | agentlife builds it: role `agentlife`, `session_id` empty, its own pid and start time, `managed: false`. The prompt then reads `agentlife (pid N) - NOT a registered lane` (`channel.rs:81`); whether that is acceptable, or agentlife should be registered, is the PM's (CONSENT.md already says so) |
| store | one fake | `Store::open(dir, &Protector, head_copy)` with `locate::dir()` = `%LOCALAPPDATA%\OverMind\user-request`, `locate::head_copy()`, a DPAPI `Protector` (`dpapi.rs`). The store is **shared** with `lane-restart` host's approvals and `with-secret` | open **the same** store, never a private one, so `revoke --all` covers restore. This adds `windows`-family crates to the build; CI's Linux leg needs the `cfg(windows)` split `user-request` already makes |
| prompt | trait `Prompt` | `HelloChannel<HelloConsent>`, `Channel::present(&Request, &Grant, wait) -> Outcome` | map `Outcome::{Approved, Denied, TimedOut, Unavailable, Refused}` onto the trait's four; `Refused` has no counterpart (treat as `Unavailable` and say why) |
| cooldown | fake has one | a Cancel or a time-out starts a 10-minute gate; 20 requests per role, 100 in all | nothing; report "not asked now: ..." (`AnswerError::Gate`) in the task's output |
| rust-version | 1.95 | `rust-version: 1.95` | CireSnave raised minimums are permitted (2026-10-08); check the workspace MSRV |
| dependency | n/a | `user-request = "0.11.1"` from crates.io | a crates.io dependency, as the Sources rule requires |

**One more mismatch worth a ruling:** `docs/CONSENT.md` and `pending.rs` treat an approval older than 5 minutes
as stale and spend it anyway. With an unattended flow that is *correct*: the person answers, the task
spends at once. It is only a problem if the task waits on the network between the answer and the spend,
so R5 must order it **answer -> spend -> then wait for anything else**.

## 3. Newly found while doing this task: restored lanes inherit `LANE_ROLE=pm`

`echo $LANE_ROLE` in this very session prints `pm`. `lane_state_writer.rs:26` says `LANE_ROLE`, when set,
**always wins** over the cwd leaf. The hand script removes the ten session-identity variables but not
`LANE_ROLE`, and the PM's shell had it set. Measured: `.lane-state/pm.json` is this lane's session
(`session_id` = this session, cwd `C:\Projects\agentlife`) and its `updated_at` advanced from 14:01:30Z to
14:01:34Z across two of my tool calls; `agentlife.json` is 28.9 h old and `overmind.json` (a restored lane)
has not been written since 2026-10-09T23:23Z. Consequences:

* the PM's real launch record is gone from `.lane-state`; an import would restore "pm" in the wrong cwd;
* every lane started by the script writes `pm.json`, and none writes its own file, so
  `lane-restart --role <x>` refuses each as stale (R4's message, for a different reason);
* agentlife's launcher strips the same ten variables (`lane_state::claude_proc::SESSION_IDENTITY_ENV_VARS`)
  and sets `AGENTLIFE_AGENT_ID` only (`src/launch.rs:175`). It would repeat this from a shell that has
  `LANE_ROLE` set. A restore run **by the logon task** would not (the task has no such variable), a run by
  a person from a lane's shell would. The launcher must set `LANE_ROLE` per entry or remove it.

## 4. Shortest path: "reboot, log in, the fleet returns with one Hello approval"

Versions are proposals; the PM allocates at gate time. Pre-1.0 the second number is the breaking one.
Order is chosen so nothing acts on a real lane before the last step.

| PR | delivers | rides | gate |
|---|---|---|---|
| 1 | **Launcher env**: set `LANE_ROLE=<role>` (or remove it) per launched entry; test that a spawned child sees the intended value and never the spawner's (`src/launch.rs`, the existing `both_commands_strip_the_session_identity...` test is the model) | 0.11.4 | CI |
| 2 | **`agentlife import-lane-state`** (R3, R11, R12): read-only on `.lane-state`; `--since 48h` (default), cwd must exist under the portfolio root, one record per `(name, cwd)` with the newest file winning, skip names in a deny list (`*restarttest*`), `--park a,b,c` sets intent closed; dry-run is the default and prints what it would write and what it skipped with why; writes only `<home>/agents`. Records get a new `Origin::Imported` | 0.11.5 | CI; one real dry-run on this box, output pasted into the PR |
| 3 | **Consent wiring** (R6, section 2): trait re-shaped, `user-request = "0.11.1"`, `installed()` returns the real backend; fake updated to the new shape; mutation tests on spend-before-run and on the freshness bound | **0.12.0** (library API change) | CI both legs; a test against a temp store via `USER_REQUEST_DIR` (debug builds only) |
| 4 | **`restore` runs**: answer -> `spend_approval` -> `execute`; caller guard (R10: a lane caller is refused); report to `<home>/reports`; `pending --prompt` shows the agent list then asks (R7, pending the ruling) | 0.12.1 | CI; real run on stand-ins only (the M3b fake `claude`) |
| 5 | **Install** (R2, R8, R17): release binary to `.claude-hooks` by rename; `install-task --register`; hook entries on **one** lane per `HOOK-INSTALL.md` | no version (operations) | **CireSnave**: approves settings and the task |
| 6 | **Canary** (R9, R15, R16): one reboot-less rehearsal, then one real reboot, section 5 | no version | **CireSnave present** |

PR 1 and 2 are independent of consent and can start now. PR 3 touches the only security surface and
should be gated by OverMind's review of the wiring. Nothing here requires the hooks (R2): the import
replaces them for the **first** restore, and the hooks keep the registry current afterwards. Without the
hooks, every later restore reads a registry as old as the last import, so PR 5's hook entries are
required for the second reboot, not the first.

## 5. What I could not verify without launching a real lane (candidates for a canary)

1. That `wt.exe -w agentlife-<k> new-tab ...` (agentlife's shape: a **named** window, one `wt` per
   agent) behaves on this box's Windows Terminal. The proven shape is `-w new -d <cwd>` (one new window
   per lane). agentlife's was measured only with stand-ins and the `conhost` fallback.
2. That a task launched at logon finds `wt.exe` (a `WindowsApps` alias), `claude` and `lane-restart` on
   its PATH and has a desktop for the tabs. `TASKS.md` measured task *creation* only (it says the firing is M6).
3. That Windows Hello's `Foreground` owner shows a dialog, and can be answered, from a logon task shortly
   after the desktop appears (R9), and what the Hello owner is when the console is not foreground.
4. That the `SessionStart` hook records a lane launched through `lane-restart host` with its **full**
   launch args (needed for R2's steady state; the import covers the first restore without it).
5. Whether `SessionEnd` fires at a real shutdown (M6 canary; the design does not depend on it, and an
   unrecorded end looks like a kill, which is restored).
6. That the installed host (`lane-restart` 0.7.1) answers the dev-channels dialog for lanes agentlife
   starts, exactly as it did for the PM's script (it should: same argv; unverified for the named-window
   shape and for the env change in PR 1).
7. That 9 to 15 lanes starting 45 s per batch of 3 stay inside `free_ram_floor_gb = 8` on this box
   (`config.rs:174`); the PM's 25 s serial launch worked, and the estimate is a lower bound.
8. The `auth-framework-deps` session and its private local patches (R13): I cannot tell from files
   whether a reboot lost uncommitted work there, and no restore can bring it back.
9. The Hello prompt text as a person sees it for a restore (R7); I read the crate's template, I did
   not render it.

## 6. What this document does not claim

* Nothing was run on a real lane, task or store. Every "built" is code plus tests at `037c7fe`, read, not
  re-run; I did not re-run the test suite for a docs-only change.
* `user-request` 0.11.1 was read from the crates.io tarball; I did not build it.
* The PM's script is ground truth for the launch; I did not run it.
