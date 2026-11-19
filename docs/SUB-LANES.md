# Sub-lanes: a primary lane creates and removes its own (design, no code)

Status: DESIGN. Written 2026-11-19 (agentlife lane). Source: CireSnave 2026-11-19, `CIRESNAVE-EXPECTATIONS.md` 6.29, verbatim:
*"In situations where a single project benefits from multiple lanes, there should a primary lane for the project which can
then create as many other lanes as necessary for separate pieces of work. ... Once the need for the sub-lanes are finished,
the primary lane should be able to remove its sub-lanes and should do so."* Until this exists the primary lane asks the PM.
Nothing here was run.

## What exists and what stops a primary lane today

| Command | Today | Gate |
|---|---|---|
| `lane-restart lane-new <role> <repo>` (new repo and lane) | dry run by default, `--apply --plan <hash>` | PM only |
| `lane-restart lane-new-worktree <role> <repo> <worktree-path> [--model M]` (a lane in a worktree of an existing repo) | same shape, model defaults to `sonnet` | PM only |
| `lane-restart lane-provision <role>` (identity, MCP config, channels for an existing spec) | dry run, `--apply --plan` | PM only |
| `lane-restart specs retire --role <r>` | refused for any non-PM session ("only the PM session may retire roles"; measured 2026-11-16) | PM only |
| Synapse `retire` / `revive` | needs the `role-admin` grant | grant |

A model other than sonnet or haiku is already refused by `validate_model_request`; that stays.

## Rules (proposed)

1. **Who is a primary lane.** The one lane whose role equals the project's name (`auth-framework`, `agentlife`). The
   tool derives it from the repo, never from a flag, and the caller is identified the way `start` identifies one (the
   `claude` ancestor and the lane state file, not an argument).
2. **Naming.** A sub-lane's role is `<primary>-<topic>`: `topic` is `[a-z0-9]+(-[a-z0-9]+)*`, 2 to 24 characters, no
   reserved word (`pm`, `launcher`, `launchtest`). The role must not already exist in the spec directory or in the Synapse
   roster. The worktree is `C:\Projects\<repo>-wt-<topic>` (the existing convention), the branch `<primary>/<topic>`.
3. **Ownership is recorded, not inferred.** The sub-lane's spec gets `created_by: <primary>` (new field, signed with the
   rest of the spec so it cannot be edited into another lane's). Retire accepts a role only if `created_by == caller`.
   A primary can therefore never touch another project's lanes, the PM, or a lane the PM created for it.
4. **Caps.** At most 3 live sub-lanes per primary and at most 12 sub-lanes in the whole fleet (a counter beside the lane
   state, re-read on every create, never cached). Why a fleet cap: each Synapse grant grew the daemon's `access.json` by
   about 35 KB and the daemon answered in 15 s with 57 lanes being made at once (2026-11-19, measured once). Over a cap the
   create is refused with the count, not queued.
5. **Model.** `sonnet` or `haiku` only, pinned in the spec; there is no flag that can ask for anything else, and no Opus
   permit is honoured for a sub-lane. A sub-lane inherits nothing from its primary's model.
6. **Grants, minimal.** A sub-lane gets exactly: `send` and `see` to its primary and to the PM, and the primary and the PM
   get `send` and `see` to it. Nothing toward any other lane, no room, no `grant`, no `role-admin`. Because grants have
   no wildcard, each is issued per object by an authorised issuer; see "Open question 1".
7. **Start.** Created `suspended`, started only by `lane-restart start` by the primary, which already refuses Opus and a
   lane over the disk floor. The launcher's `see` over the new role is one grant covering all sub-lanes, issued by the PM
   once (`DURABLE-TIMERS.md`, B2), not one per lane.
8. **Dry run first, plan hash second** (as `lane-new`): `--apply` is refused without the hash of the same dry run.

## Create and remove, step by step (each step reads back, as `lane-new` does)

Create: validate (caller is the primary, name, caps, no collision) -> worktree off `origin/main` -> spec with `created_by` ->
MCP config and identity (`lane-provision`, the claim step added on this branch) -> grants of rule 6 -> read the roster
(`epoch >= 1`) -> report. Nothing is started.

Remove (`lane-restart sub-lane retire <role>`, by the primary, only after the sub-lane's work is merged or abandoned):
`gh pr list --head <branch> --state merged` or an explicit `--abandon`; `git status --porcelain` in the worktree must be
empty (a dirty tree asks, never forces); stop the lane if live (soft stop, never kill); `git worktree remove` and `prune`;
delete the local branch and the remote branch with a read-back (`git ls-remote --heads origin <b>` plus a `main` control);
retire the spec; revoke the grants of rule 6; retire the Synapse role. Idempotent: a rerun on a half-removed lane finishes
what is left and says what it found.

## Open questions (need an answer before code)

1. **Who may issue the grants and retire the Synapse role?** `access grant` is the account holder's, or a holder of a subset
   passing it on; `retire` needs `role-admin`. A primary lane must not hold `role-admin` over the fleet. Options: (A) the PM
   gives each primary a grant it may pass on, limited to `see`/`send` over objects named `<primary>-*` (Synapse has no
   wildcard today, so this needs a Synapse change); (B) the tool queues the grants and retire as a request the PM's session
   executes (a second step, not instant); (C) a narrow broker that only the PM runs. Recommended: (B) now, (A) when Synapse
   supports prefixes. Asked of the PM and the Synapse lane.
2. **Where the caps live** (a file beside the lane state is proposed; the PM may prefer the roadmap).
3. **Does `specs retire` need a parent check on the PM side too**, so the PM cannot delete a lane a primary still uses
   without a warning? Proposed: a warning line, not a block.

## Test plan (nothing runs for real until a `launchtest`-style dry run passes)

- Unit, with the fake host and fake Synapse: a primary creates `<primary>-topic` (dry run changes nothing; the apply
  needs the hash); a primary asking for another project's prefix, the PM's name, a reserved word, a fourth live sub-lane,
  a thirteenth fleet sub-lane, `--model opus`, or a name that collides: each refused with its reason; a retire of a lane
  with another `created_by` or none: refused; a dirty worktree: asks, removes nothing; a rerun after a failure at each
  step converges.
- One real run on a scratch project and role by the PM, dry run first, then `--apply`; read back worktree, branch (local
  and remote), spec, roster epoch, grants; then retire and read back that all are gone, `access.json` size before and after.
