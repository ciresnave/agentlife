# pin-plan

Planner for SUP-008 (pin `uses:` references to commit SHAs). It writes nothing into any repo.

```
pin-plan [--root DIR] [--repo NAME]... [--no-network] [--all] [--emit-patch DIR]
```

Per repo under the root it lists every `uses:` that is a mutable tag or branch, with the commit SHA the
current ref names (pin to this and nothing changes), the latest release's tag and SHA, and the Node runtime
the action declares now and at that release (`node20 -> node24`, node20 and older flagged).

## `--emit-patch DIR`

Writes one `DIR/<repo>.patch` per repo that has something to pin: a unified diff, against that repo's
`origin/main` text, that `git apply` takes. Each mutable ref becomes **the SHA it names now** (so behaviour
does not change) with the old ref left as a comment (`uses: actions/checkout@<sha> # v4`; an existing trailing
comment follows: `# v4 - note`). Line endings are kept. Nothing is written into a repository, and a `DIR`
inside one is refused before anything is created. References that could not be resolved (or have no `@ref`)
are not patched and are counted in the totals. The PM routes each patch to its repo's lane; this tool opens
no PR.

### `--target latest`

`--target current` (default) pins to the commit the ref names now. `--target latest` pins a *tag* ref to the
commit of the action's latest release (comment = that tag): the way off node20 when the latest release declares
node24, at the price of a possible major jump (its inputs may differ; CI in the repo is the proof). A *branch*
ref is kept at its current commit in both modes, since a branch like `dtolnay/rust-toolchain@stable` selects
behaviour a release tag does not carry. `DIR/ACTIONS.md` is the per-action table (runtime now and at latest,
major jump, "no node24 release yet" first).

## Dogfood

`tests/own_workflows.rs` fails when any workflow or action file in this repo's index holds a mutable ref, or a
pinned ref with no comment naming its tag.

## Reconciling counts

CodeRipper's SUP-008 figure is **235 pins in 18 repos**. A full run of this tool (2026-10-14, each repo's
`origin/main`) found **455 `uses:` references in 32 repos: 59 already pinned, 393 mutable (313 tag, 80
branch) in 22 repos**. The two are different populations, not a disagreement: this tool counts every
mutable reference in every repo under `C:/Projects` (linked worktrees skipped), per occurrence, whatever the
action. The population behind the 235 (which repos, which kinds of reference, per occurrence or per
distinct action) is CodeRipper's to state; this tool's numbers should not be expected to match it.

## Findings from that run

- **213 references run node20 or older today**; 100 of them are `actions/checkout@v4`. Every one of those
  actions declares node24 at its latest release.
- **80 are branch refs** (67 are `dtolnay/rust-toolchain@stable`), which move with every push. For a branch
  the faithful pin is the SHA the branch names now, not the latest release tag.
- 1 reference could not be resolved (`NVIDIA/blossom-action@main`: the repo has no tags).
