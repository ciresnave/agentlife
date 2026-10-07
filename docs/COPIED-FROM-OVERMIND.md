# Code copied from OverMind (temporary)

`src/claude_proc.rs` is a **temporary copy** of a few items from OverMind's `lane-restart` library.
This file is its provenance record (`C:/Projects/CIRESNAVE-EXPECTATIONS.md` §6.4a: trace, verify against
the origin, keep the licence and credit).

## Why it exists

The portfolio rule (`C:/Projects/CLAUDE.md` §9 "Sources", 2026-10-07; `CIRESNAVE-EXPECTATIONS.md` §6.8)
is that cross-repo dependencies are crates.io versions, and a `git =` dependency is allowed only as a
temporary state with a named blocker and an owner. M1 first used `lane-restart` by git rev
(`c3935ba`). `lane-restart` is **not published**: the crates.io API answers **404** for it, and **200**
for `sysinfo` as a control, on 2026-10-07. The PM ruled (2026-10-07): copy the few items, drop the git
dependency, credit OverMind, label the copy temporary.

| | |
|---|---|
| **Blocker** | OverMind extracts a small crate containing these items, and CireSnave clears its publish |
| **Owner** | the OverMind lane |
| **End state** | `src/claude_proc.rs` is **deleted**; `hook.rs` depends on the published crate |
| **PM sign-off** | given in the PM's ruling of 2026-10-07 (provenance §6.4a satisfied) |

## Origin

* Repository: <https://github.com/ciresnave/OverMind>
* Files: `crates/lane-restart/src/lane_state_writer.rs`, `crates/lane-restart/src/paths.rs`
* Commit: `c3935babb0ae90b7a75f84eaaf58506cb782bad3` (`c3935ba`)
* Same owner as this crate, same licence: `MIT OR Apache-2.0`
* OverMind's doc comments are kept verbatim as credit and as the record of why each rule exists.

## What was copied, and the hash of each item

Each item is the text from its doc comments and attributes through its closing brace, with CRLF
normalised to LF, hashed with SHA-256 (first 12 hex digits). Line numbers are at `c3935ba`.

| item | file | lines | sha-256 (12) | changed here? |
|---|---|---|---|---|
| `enum ModelField` | `lane_state_writer.rs` | 21–35 | `561fd6420f56` | no |
| `impl ModelField` | `lane_state_writer.rs` | 37–44 | `501e4e69f9b8` | **yes (1)** |
| `struct HookInput` | `lane_state_writer.rs` | 46–70 | `6c0fa2013ef0` | no |
| `enum PidError` | `lane_state_writer.rs` | 88–97 | `1be7106f6f89` | no |
| `impl Display for PidError` | `lane_state_writer.rs` | 99–118 | `5e6d500d440c` | no |
| `trait ParentProcess` | `lane_state_writer.rs` | 120–138 | `73da22dba155` | no |
| `struct ClaudeCliFlags` | `lane_state_writer.rs` | 140–171 | `4b9123e4209d` | no |
| `fn parse_claude_cli_flags` | `lane_state_writer.rs` | 173–213 | `b90b698ab02b` | no |
| `const SHELL_ANCESTOR_NAMES` | `lane_state_writer.rs` | 215–222 | `6426bd0279f7` | no |
| `const MAX_ANCESTRY_HOPS` | `lane_state_writer.rs` | 224–227 | `d722ea247239` | no |
| `fn image_base` | `lane_state_writer.rs` | 229–232 | `af82ff848aa2` | no |
| `fn claude_parent_pid` | `lane_state_writer.rs` | 234–253 | `2e151ad83f5d` | no |
| `fn transcript_project_dir` | `lane_state_writer.rs` | 364–369 | `d7b3fc582e57` | no |
| `fn recorded_cwd` | `lane_state_writer.rs` | 371–401 | `7a296a4e67e1` | **yes (2, 3)** |
| `fn project_dir_name` | `paths.rs` | 52–60 | `93124f4801df` | no |

## Every difference from the original (all marked `ADAPTED` in the code)

1. **`ModelField::into_string` is `pub`.** In OverMind it is private to its module; the hook calls it
   from another module.
2. **`recorded_cwd` takes `Option<&KnownLaunch>`** instead of `Option<&LaneState>`. `LaneState` is
   OverMind's state-file type, which agentlife does not have. `KnownLaunch` (new, defined in the copy)
   carries the only two fields the body reads, `pid` and `cwd`, so the body is otherwise unchanged.
3. **`recorded_cwd` calls the local `project_dir_name`** instead of `crate::paths::project_dir_name`
   (that function is copied alongside it, unchanged).

Nothing else differs. `KnownLaunch` itself is the only new type.

## Verified how

* **Against the origin, at copy time (2026-10-07):** the file was *generated* by extracting each item's
  text from `git show origin/main:<path>` (not retyped), then applying the three adaptations above, each
  asserted to apply exactly once. The six functions and types with the same bodies were also hashed
  at `c3935ba`, at OverMind `main` `8fbd1db`, and at the head of the open speed-up PR #112 (`bcc5d51`):
  `ParentProcess`, `claude_parent_pid`, `parse_claude_cli_flags`, `recorded_cwd`, `HookInput`,
  `ModelField` are **identical at all three**. That PR changes only `RealParentProcess` and adds new
  helpers, none of which are copied.
* **Behaviour, by tests:** OverMind's own tests for these items are ported verbatim (the ancestry walk:
  8 cases; the flag parser: 7 cases; the `HookInput` payload parse). Its `recorded_cwd` scenarios,
  which in OverMind go through its state writer, are re-expressed against the copied function with the
  same inputs and expected answers. Added here: the `-n` / `--name` / `--name=` spellings and a
  trailing `-n`, both shapes of `model`, and the project-directory encoding.

## To re-verify later

```
git -C <OverMind> show <rev>:crates/lane-restart/src/lane_state_writer.rs
```

then extract each item named above and compare its text (CRLF normalised) with `src/claude_proc.rs`;
the only expected differences are the three listed. If OverMind has changed an item since `c3935ba`,
that is the moment to decide between re-copying and finishing the extraction.

## What is deliberately not copied

`RealParentProcess` (the per-hop full scan; `hook.rs` has its own one-snapshot implementation of the
copied `ParentProcess` trait, measured at ~80 ms against ~860 ms per hop), `LaneState`, the lock, and the
state writer. Everything copied is pure logic over injected data.
