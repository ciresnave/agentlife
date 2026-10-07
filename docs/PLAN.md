# The restore planner (M3a)

What `agentlife restore --dry-run` computes, the rules it applies, and what it does **not** do. Code:
`src/plan.rs`. Design it implements: `DESIGN.md` §2, `DESIGN-REVISION-1.md` §3 and §6,
`DESIGN-REVISION-2.md` §3 and §5.

**It starts nothing and writes nothing.** A plan is a value. `restore --dry-run` prints one; plain
`restore` refuses ("starting agents is not built yet; it needs a person's consent (M4)"). The launcher
(wt.exe tabs, per-batch liveness, the report) is M3b; consent and the durable pending restore are M4.

## What is a candidate

Every registered agent is looked at once, in this order, and lands in exactly one of three places: the
plan, **held**, or **excluded** (with a reason). A test asserts no record is ever lost or counted twice.

| check | outcome |
|---|---|
| not named by `--only` | excluded `not_selected` |
| intent is not `wanted` (parked, exited, lazy) | excluded `not_wanted`; lazy agents wait for demand (M7) |
| alive (pid **and** start time match) | excluded `running`: an agent that is up is never in a plan |
| alive but the record has no start time | excluded `unverifiable`: it cannot be told from a stranger, so it is **not started** (never start a duplicate) |
| another live agent has the same name in the same directory | excluded `duplicate_alive` |
| no recorded launch arguments | excluded `no_launch_args` |
| `cwd` not under `portfolio_root` (text check, `..` refused) | excluded `cwd_outside_root` |
| `cwd` does not exist | excluded `cwd_missing` |
| mode is `bypassPermissions` and the agent is not the PM | excluded `bypass_not_pm`: **refused, never downgraded** |
| a passed value contains `;`, a control character, or looks like a flag | excluded `unsafe_argument` |

"The PM" is the agent the visible rule names (`pin_roles` and `portfolio_root`). An explicit pin does
**not** make a lane the PM.

## The launch arguments are rebuilt, not replayed

The recorded argv is walked through an allowlist and rebuilt in one canonical order:
`--name`, `--model`, `--permission-mode`, `--remote-control`, and `--dangerously-load-development-channels
server:claude-peers`. Anything else (including a prompt, `--add-dir`, `--mcp-config`) is **dropped and
listed** on the entry (`dropped_args`) and in the text output, never silently. A channel other than
`server:claude-peers` is dropped and flagged. The mode is the **observed** one when the record has it, else
the launch flag, so a restart never widens (a lane launched with the bypass flag but seen running in
`auto` comes back as `auto`). `plan` and unknown modes are flagged `mode-needs-a-person`.

Whether restore should add a start-up prompt ("read your HANDOFF and continue") is a launcher question
(M3b); the plan carries no prompt.

## Order, caps, batches, placement

1. **Order**: the PM first and alone; then `--priority` names in the order given; then the most recently
   active (last session start or end); then name; then id. The same registry always gives the same
   order, whatever order the registry lists its files in (a property test shuffles it).
2. **Caps**: `max_running` counts agents **already running**; free memory below `free_ram_floor_gb`
   holds everything. Held agents are reported as held (`max_running` or `memory`), not as failed. The
   memory reading is a snapshot at plan time; the launcher (M3b) re-checks before every batch.
3. **Batches**: the PM is batch 0 alone; the rest in chunks of `batch_size`. With no PM, batches start at 0.
4. **Placement**: window `agentlife-<k>`, `k = position ÷ tabs_per_window`, tab = position mod it. Assigned
   here, recorded only for the report, never read back as a requirement.

Defaults and flags are the ones in `DESIGN-REVISION-2.md` §5 (`--batch`, `--delay`, `--max-running`,
`--tabs-per-window`, `--free-ram-floor`); the pacing numbers are defaults, not ceilings.

## The hash and the frozen plan

`Plan.hash` is a SHA-256 over the schema, the parameters, every entry and every held agent. It is **not**
over free memory, the running count or the exclusion list: those change without changing what would
start. So an unchanged registry rebuilds to the same hash, and any change to what would be launched
changes it (tests change a launch field, a cwd and a parameter, and shuffle the input).

`plan::freeze` writes `<home>/plans/<UTC>-<hash prefix>.json` atomically; `plan::load_frozen` refuses a
file whose content no longer matches its recorded hash; `plan::check_unchanged` compares a frozen plan to
one rebuilt from the registry now and names each agent that was added, removed or changed. **No command
freezes anything in M3a**: freezing belongs to the consent step, which is M4 (`bound_hash` is this hash).

## Tests

- `src/plan.rs`: one test per rule above, each with its negative control.
- `tests/plan_properties.rs`: 400 generated registries (0 to 60 agents, random intents, liveness, modes,
  hostile arguments, missing directories, caps and memory) check the invariants between the rules; 150
  more check **idempotence** (mark everything a plan names as running; the next plan never names it
  again); fleet runs at **40** and **1,000** agents check batch and window counts, order and a time bound.
- `tests/hook_e2e.rs`: the real registry, written by real stand-in `claude` processes through the real
  hook, read by the real `agentlife restore --dry-run --json`: the rebuilt argv, the running lane left
  alone, the same registry giving the same hash, and the whole home directory **byte-identical** before
  and after (a dry run writes nothing). Plain `restore` refuses and writes nothing.

## Not here

The launcher, the per-batch liveness check, the report and the real spawn through `wt.exe` (M3b); the
consent, the durable pending restore and the first real restore (M4); `deferred` selection in the review
(M4); standing grants (M5).
