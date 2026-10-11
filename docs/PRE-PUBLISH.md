# Before publishing the agentlife workspace to crates.io

**Nothing in this file publishes anything.** It records what must be true before a person runs
`cargo publish`, what was measured, and what only CireSnave decides. Facts below were read on
**2026-10-11** from `origin/main` at `01640b1` (workspace version **0.12.2**) and from the crates.io API
(`https://crates.io/api/v1/crates/<name>`); re-read them at the ref you publish from, because every merged
PR moves the version.

## What is unpublished

| Crate | On crates.io (2026-10-11) | On `main` | Depends on (workspace) |
|---|---|---|---|
| `lane-state` | **0.11.1** (repository `ciresnave/OverMind`) | 0.12.2 | none |
| `lane-restart` | does not exist | 0.12.2 | `lane-state` |
| `agentlife` | does not exist | 0.12.2 | `lane-state` |

The workspace has one version number (`Cargo.toml`, `[workspace.package]`), so all three move together.
`lane-state` 0.11.1 on crates.io is the OverMind-era publish; main's `lane-state` is a later line (0.12.x),
which will be its first publish from this repository.

### Which consent-crate version agentlife depends on

agentlife depends on **`user-request` = 0.11.1** (`Cargo.toml`: `user-request = "0.11.1"`; `Cargo.lock`
resolves `0.11.1`). That is also the **newest** `user-request` on crates.io (max version 0.11.1, created
2026-10-09, repository `ciresnave/consent`). It is a crates.io dependency, not a path or git one, so the
publish does not wait on it. agentlife needs rust 1.95 for it, and the workspace already requires that.

Not the consent crate, but read in the same pass: `persistant` is `0.4.1` in `Cargo.lock` and in
`Cargo.toml`'s `"0.4.1"` requirement, while crates.io has **0.4.4**. CireSnave's standing rule is every
dependency on its latest version; `cargo update -p persistant` is a change of its own (its own PR, its own gate),
and it should land **before** a publish, not after.

## What must be true

1. **`main` is the thing published.** CI is green on both legs on the commit being published, including the
   `deny` job (advisories, licences, bans, sources), and there are no unresolved review threads.
2. **The version is final.** The PM has allocated it; `CHANGELOG.md` has a section for exactly it; `Cargo.toml`
   `[workspace.package].version` and the `lane-state` line under `[workspace.dependencies]` carry the same
   number (the latter is what `cargo package` writes into the dependents).
3. **The version is not on crates.io.** A published version cannot be replaced, only yanked. `lane-state`
   must be **above 0.11.1**; main already is.
4. **Dependencies are the latest crates.io versions** (the `persistant` note above) and **none is a path or
   git dependency** except the workspace's own `lane-state`, which cargo rewrites to a version.
5. **The packages build from their own tarballs.** `cargo package --workspace` is the evidence below.
6. **The tag and the publish commit are the same commit**, and the tree is clean (`git status --porcelain`
   empty; `cargo publish` refuses a dirty tree without `--allow-dirty`, and nobody passes it).
7. **The ownership is settled** (see "Only CireSnave decides").

## The evidence: `cargo package`

Run from a clean checkout of `origin/main` (cargo 1.98.1). **Per crate**, each from its own directory:

```
cargo package -p lane-state      # Packaged 14 files, 109.9KiB (32.9KiB compressed); verify build ok
cargo package -p lane-restart    # error: failed to select a version for the requirement `lane-state = "^0.12.2"`
cargo package -p agentlife       # the same error
```

The second and third fail **by design, for now**: they require `lane-state` 0.12.2 to exist in the crates.io
index, and only 0.11.1 does. That error disappears the moment `lane-state` is published, and not before.
It is not a defect in either crate, but it also means a per-crate `cargo package` on those two proves
nothing until then.

**The whole workspace**, which cargo orders and verifies against a local registry holding the
not-yet-published members:

```
cargo package --workspace
   Packaged 14 files, 109.9KiB (32.9KiB compressed)     lane-state   v0.12.2
   Packaged 71 files, 1.2MiB (340.9KiB compressed)      agentlife    v0.12.2
   Packaged 24 files, 468.8KiB (114.1KiB compressed)    lane-restart v0.12.2
   Verifying lane-state / agentlife / lane-restart ... Finished  (exit 0)
```

That is the evidence that counts: all three packages build from the packaged sources, with
`lane-state` resolved from the local registry. Re-run it at the publish commit and paste the output into
the publish record; numbers above are for 0.12.2 and will differ.

### Things the package listing shows (decide, do not discover at upload)

* **`agentlife` ships its design notes and tests.** The 71 files include `docs/*.md` (16 files at 0.12.2),
  `RESTART-TOOL-DESIGN.md`, `.github/workflows/ci.yml`, `examples/` and `tests/`: 1.2 MiB uncompressed
  (340.9 KiB compressed). Either accept it or add an `exclude`/`include` list to `[package]`. Read the
  design notes once for anything that should not be public before choosing; this checklist did not.
* **`agentlife` declares no `rust-version`.** Its dependency `lane-state` declares 1.95, so an older
  toolchain fails inside a dependency with a message that does not name agentlife. `lane-state` has the
  field; `agentlife` and `lane-restart` do not.
* **`lane-restart` has no `readme`, `keywords` or `categories`**; crates.io accepts that, but the page is bare.
* **`lane-restart` depends on `windows-sys`** unconditionally. It builds on Linux CI today
  (`windows-sys` compiles to nothing there); whether a Windows-only tool belongs on crates.io is a
  decision below.

## Dependency order

1. `lane-state` (no workspace dependency).
2. `lane-restart` and `agentlife`, in either order or together: both depend only on `lane-state`, and
   neither depends on the other.

After `lane-state` is **visible in the index** (wait for `cargo search lane-state` / the API to show the new
version; the index lags the upload by seconds to minutes), a per-crate `cargo package -p <crate>` for the
other two stops failing, and that is the second piece of evidence worth recording. `cargo publish
--workspace` does the ordering and the waiting itself.

## Only CireSnave decides

* **Whether to publish at all, and when.** The standing instruction is that the PM brings the publish; a
  lane never runs `cargo publish`.
* **Which crates.** `agentlife` and `lane-restart` are free names (both returned "does not exist" on
  2026-10-11). Publishing `lane-state` alone, or `lane-state` plus `agentlife` without the Windows-only
  `lane-restart`, are both coherent.
* **The first version number** of each crate on crates.io. `agentlife` and `lane-restart` would first appear at
  0.12.x because the project keeps one number (his rule); a published version cannot be replaced.
* **Whose account owns the crates**, and who else is added as an owner. Ownership and the API token are
  his; a lane never holds either.
* **`lane-state`'s move from the OverMind repository to this one.** The 0.11.1 record on crates.io names
  `ciresnave/OverMind` as its repository; the next version names this workspace's. Whether that continues
  the same crate (it does by name) is a provenance matter for him, per his standing rule on credit.
* **Yank policy** if something published is wrong: yanking stops new resolutions but does not delete.

## What is not in scope

No publish, no tag, no `Cargo.toml` edit for the version, and no change to `persistant`. This file is a
checklist and a measurement; running it is the PM's, and the upload is CireSnave's.
