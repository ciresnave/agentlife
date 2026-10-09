# lane-state

What a Claude Code **lane** wrote about itself, and the pure rules for finding that lane's `claude`
process and launch command line. It is the small piece that `lane-restart`, `with-secret` and
[agentlife](https://github.com/ciresnave/agentlife) all need, so none of them has to copy another's code.

A *lane* here is one long-running Claude Code session with a role name (for example `overmind`), started
in a working directory, whose hooks write a small JSON state file about it.

## What is in it

| module | what it does |
|---|---|
| `state` | the `<role>.json` state file (`LaneState`: role, session id, pid and start time, cwd, model, permission mode, launch arguments, busy flag, ...) and `load`. It only reads: the lane's own hooks write it. |
| `facts` | the OS facts a decision needs, behind one trait (`SystemFacts`) so the logic is testable against a fake: is this pid a live `claude`, what is its cwd and start time, kill it only if it is still the process that was checked (`kill_verified`), find a new `claude` started after some moment. `SysinfoFacts` is the real implementation, on top of `sysinfo`. |
| `paths` | path comparison that survives trailing separators, `/` against `\` and Windows case, the directory name Claude Code files a session's transcript under (`project_dir_name`), and the user's home directory (`home_dir`). |
| `claude_proc` | finding the `claude` process from a hook's ancestry (skipping only known shells, refusing a stranger), parsing the flags of its launch command line, reading a hook's JSON payload, and `SESSION_IDENTITY_ENV_VARS`: the environment variables that name a running session, which a relaunch must not hand on. |

## Scope, honestly

- It is **not a general-purpose** process, session or Claude Code library. It exists to serve one set of
  tools (the OverMind portfolio's lane tools and agentlife) and its API follows their needs.
- It is **pre-1.0**. A breaking change bumps the second number (`0.11.x` to `0.12.0`), and the API is not
  yet stable.
- The state-file layout is what Claude Code hooks of the lane tools write today; it is read as written, and
  fields a lane never wrote stay `None` rather than being guessed.
- The real process code (`SysinfoFacts`) is developed and tested on **Windows** (the tools it serves run
  there); it builds and its pure parts are tested on Linux in CI.
- Nothing here decides to start, restart or stop a lane, and nothing launches one. It can kill a process only
  through `SystemFacts::kill_verified`, which a caller such as `lane-restart` invokes after its own checks, and
  which refuses a pid whose identity changed since it was checked.
- The minimum Rust version is the highest one declared by a dependency (currently `sysinfo`'s); it is not
  separately tested against older toolchains.

## Licence

`MIT OR Apache-2.0`, at your option: see `LICENSE-MIT` and `LICENSE-APACHE`.

Source and history live in the [OverMind repository](https://github.com/ciresnave/OverMind) (`crates/lane-state`).
