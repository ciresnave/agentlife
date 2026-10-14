# pin-plan

Read-only planner for SUP-008 (pin `uses:` references to commit SHAs). It writes nothing.

```
pin-plan [--root DIR] [--repo NAME]... [--no-network] [--all]
```

Per repo under the root it lists every `uses:` that is a mutable tag or branch, with the commit SHA the
current ref names (pin to this and nothing changes), the latest release's tag and SHA, and the Node runtime
the action declares now and at that release (`node20 -> node24`, node20 and older flagged).

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
