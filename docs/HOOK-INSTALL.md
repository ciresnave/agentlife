# Installing the agentlife hook (M1)

For the PM, who installs it (the settings are CireSnave's; DESIGN-REVISION-2 §9, MILESTONES.md gates).
**Nothing in this PR installs anything.** This is the exact block, the sequence, and what was measured.

## What it is

`agentlife.exe hook SessionStart` and `agentlife.exe hook SessionEnd --reason <r>`: two events, never
the per-tool-call hook. It records the session in `C:/Projects/.agentlife/` (the registry and the
journal) and writes one line per run to `C:/Projects/.agentlife/hook.log`. It **always exits 0**: a
failing hook is silent to Claude Code, so `hook.log` is the only place a broken install shows up.

## The block

Shell form, one command string, **no `args` key** (OverMind found Claude Code does not deliver `args`
to a command hook). **Append** to each event's existing array; never replace the `hooks` key (the
`run_wrap_hidden.vbs` entry stays). The binary goes at one fixed path, installed by rename, never
overwritten (see "Install the binary").

```json
"SessionStart": [
  { "hooks": [{ "type": "command",
    "command": "C:/Projects/.claude-hooks/agentlife.exe hook SessionStart" }] }
],
"SessionEnd": [
  { "matcher": "prompt_input_exit", "hooks": [{ "type": "command", "timeout": 10,
    "command": "C:/Projects/.claude-hooks/agentlife.exe hook SessionEnd --reason prompt_input_exit" }] },
  { "matcher": "logout", "hooks": [{ "type": "command", "timeout": 10,
    "command": "C:/Projects/.claude-hooks/agentlife.exe hook SessionEnd --reason logout" }] },
  { "matcher": "other", "hooks": [{ "type": "command", "timeout": 10,
    "command": "C:/Projects/.claude-hooks/agentlife.exe hook SessionEnd --reason other" }] },
  { "matcher": "clear", "hooks": [{ "type": "command", "timeout": 10,
    "command": "C:/Projects/.claude-hooks/agentlife.exe hook SessionEnd --reason clear" }] },
  { "matcher": "resume", "hooks": [{ "type": "command", "timeout": 10,
    "command": "C:/Projects/.claude-hooks/agentlife.exe hook SessionEnd --reason resume" }] }
]
```

One `SessionEnd` entry per documented matcher passes the **reason** on the command line, so nothing
depends on the payload carrying a `reason` field (unverified). The matcher values are the documented
ones (`clear`, `resume`, `logout`, `prompt_input_exit`, `other`).

**New since DESIGN-REVISION-2 §9.1: `"timeout": 10` on the `SessionEnd` entries.** The hooks docs say
`SessionEnd` hooks share a **1.5 s** budget, and that a longer per-hook `timeout` raises it (up to 60 s).
Measured below, the hook's own work is small, but starting a process on this machine occasionally takes
longer than 1.5 s all by itself, so the default budget would sometimes kill a healthy hook. I did not
verify the unit of `timeout` against a live Claude Code (the docs excerpt I read says only that it raises
the budget); the PM should confirm on the one-lane trial that a 10 does not mean 10 ms or 10 minutes.

## Install the binary: rename, never overwrite

The binary is in use by every session's hook, so a plain copy over it is refused by Windows while any
session has it open (proven in `src/install.rs`'s test with a real running exe). The sequence, as
implemented and tested in `agentlife::install::install_by_rename`:

```
copy  target\release\agentlife.exe  C:\Projects\.claude-hooks\agentlife.exe.new
ren   C:\Projects\.claude-hooks\agentlife.exe      agentlife.exe.old
ren   C:\Projects\.claude-hooks\agentlife.exe.new  agentlife.exe
```

Build the **release** binary (`cargo build --release`), not `target/debug`: the debug build is slower to
start, and a hook runs at every session start and end. A failed final rename is rolled back by the
function; by hand, if the third step fails, rename `.old` back.

## The one-lane trial (the PM's sequence)

1. Install the binary.
2. Add the entries to **one** lane's project settings (or one session), not user-level.
3. Start that lane; then run `agentlife list`. **The acceptance check is a real record read back**, not
   a diagnostic: `list` must show that lane, with its real name, mode and a `running` state.
4. Read `C:/Projects/.agentlife/hook.log`: one `SessionStart ok registered` line. A `skipped:` or
   `ERROR` line says exactly why not.
5. End the session with `/exit`; `agentlife list --all` and `hook.log` must show `SessionEnd ok ended`.
6. Only then widen to user-level.

## Rollback

Remove only the entries whose command is `agentlife.exe`, from each event's array, leaving every other
hook untouched. The registry and journal can stay in place; nothing reads them yet except `list`.

## What was measured (2026-10-07, this machine, 15 live lanes, ~640 processes)

* **The hook's own time**, from its own log line: `SessionStart` 229 to 253 ms, `SessionEnd` 110 to
  192 ms, of which the process snapshot is 83 to 157 ms.
* **Why a snapshot:** the first version used `lane-restart`'s `RealParentProcess` unchanged. Its
  `parent_of` refreshes every process with sysinfo's default (expensive) kind **on every hop**: about
  **860 ms per hop**, so a `SessionEnd` walk took **3.1 to 5.5 s** under load in the real-chain test.
  One `refresh(All, nothing)` costs about 80 ms. The hook now takes one snapshot and feeds it to
  `lane-restart`'s own `claude_parent_pid`, so the walk's rules are reused and only the data source
  changed. This is a finding for the OverMind lane (their hook has the same per-hop cost; their
  `PreToolUse` runs it on every tool call).
* **What is not ours:** wall-clock spawn-to-exit varies far more than the hook's own time. A bare
  `cmd /c exit 0` took 39 to 538 ms over 8 runs; `agentlife --version` 45 ms to 1.6 s; the first run of
  a freshly copied exe 0.7 to 1.7 s. So a `SessionEnd` can occasionally miss a 1.5 s budget through
  process creation alone, which is why the entries carry a larger `timeout`, and why the design never
  depends on `SessionEnd` firing (DESIGN-REVISION-1 §2.6).

## Known limits

* A lost `SessionEnd` looks like a kill, and is restored; the review flags it (DESIGN-REVISION-2 §3.3).
* `hook.log` grows without bound (two lines per session). Pruning belongs to a later milestone.
* Same-user processes can write the registry (DESIGN-REVISION-1 §4.3, item 5).
