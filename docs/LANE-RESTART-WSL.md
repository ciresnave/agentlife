# `lane-restart` launch mode `wsl` (spec, nothing built)

Task from the PM (Synapse, 2026-10-23/24), owner decision 179 B (WSL2 pilot, `C:\Projects\WSL-LANE-PILOT.md` item 4): a lane
can be launched **inside a WSL2 distro, in the PM's bubblewrap sandbox**, by `lane-restart start` / `--self` / `--role`, with the
same HANDOFF flow and the same checks. Spec first; red-first tests are section 9; no code in this PR.

Everything below was read at `origin/main` eaff3e2 (0.29.13) unless it says otherwise. **UNVERIFIED** marks a statement about WSL
that this lane has not seen happen: the pilot distro `lane-spike` was **Stopped** when this was written (`wsl -l -v`, 2026-10-24), and
starting it is the PM's pilot, so none of those was measured. Each is a pilot step (section 10), not an assumption.

## 1. What does not change

- A spec with no `launch` field is a Windows lane, read and written exactly as today (`schema` 1). **No Windows lane's `start`,
  `--self`, `assert-idle`, HANDOFF, `stop` or `specs` behaviour changes**, and a Windows spec is byte-identical after this work.
- **Sonnet pinned.** `resolve_launch_model` and `prepare_launch` run on a WSL spec unchanged: a non-Opus model, the policy default,
  the explicit `--model` in the argv.
- **`lane-restart` stays free of any Synapse dependency.** The Synapse role, keys, relay and `--mcp-config` belong to the PM's wrapper
  (section 3); lane-restart only carries the allowlisted flags it carries today.
- The wrapper is **a PM script**. lane-restart invokes it and nothing else inside the distro (one exception: the fixed probe in 5).

## 2. The spec (`<role>.launch.json`)

Three optional fields, present **only** for a WSL lane:

| field | type | rule (refused at write AND at read, before anything is killed or spawned) |
|---|---|---|
| `launch` | `"wsl"` | absent = Windows. Any other string is `UnsupportedLaunch`, never defaulted to Windows. |
| `wsl.distro` | string | `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$` (no leading `-`: it would be read as a `wsl.exe` option). |
| `wsl.user` | string | `^[a-z_][a-z0-9_-]{0,31}$`; **`root` is refused** (the pilot lane has no sudo and must not gain it here). |
| `wsl.wrapper` | string | an absolute POSIX path **inside the distro** (starts with `/`, no `..` segment, no control character, no `;`, no space). Not a Windows path. |
| `wsl.state_dir` | string, optional | absolute POSIX path of the lane's state directory inside the distro. Default `/home/<user>/.local/state/lane-state` (the Unix value of `lane_state::platform::state_dir`, 0.29.x). |

`cwd` of a WSL spec is a POSIX path inside the distro (`/home/lane-commysdk/work/commy-sdk-rust`), validated as such (absolute, no `;`,
no control character); the Windows rules for `cwd` (drive letters, `paths_match` case-folding) do not apply to it.

**Schema.** A spec with `launch: wsl` is written with `schema: 2`; Windows specs stay `schema: 1`. This is deliberate: an older
build reads `schema != SCHEMA` as `UnsupportedSchema` (`spec.rs`) and answers *unknown, named, never defaulted* (RESTART-TOOL-DESIGN 14.2),
so an old `lane-restart` can never launch a POSIX `cwd` as a Windows process. This build reads 1 and 2.

The allowlist rule of 14.4 is unchanged: no environment values and no argv element outside the `LaunchFlag` table
(`--dangerously-load-development-channels`, `--add-dir`, `--mcp-config`, `--settings`). The `--mcp-config` value of a WSL lane is a
**literal** absolute POSIX path (`wsl.exe` does not expand `$HOME`); the wrapper is the place for anything environmental
(`SYNAPSE_HOME`, `SYNAPSE_ADDR`, `SYNAPSE_REMOTE_DAEMON`).

## 3. The command a WSL launch runs

`launch_argv` is unchanged, so the claude argv keeps its order: **prompt first**, then `--name`, `--model sonnet`,
`--permission-mode`, then the allowlisted flags in their recorded order, and `--dangerously-load-development-channels server:synapse`
**last** because it is variadic (`tools/wsl-relay/lane-usage.md` in the synapse repo, `wsl-pilot-design` @ d3cdbe1: a prompt after it
is swallowed as a channel entry). `claude` is `argv[0]` exactly as today.

The WSL launch wraps that argv instead of the Windows `claude`:

```
wsl.exe -d <distro> -u <user> --cd <cwd> --exec <wrapper> -- claude <prompt> --name <role> --model sonnet ... <variadic flags last>
```

- **`--exec`, no shell.** `wsl.exe <cmd>` without `--exec` hands the line to the user's shell, which re-parses it. With `--exec` the
  elements are an argv. The prompt contains spaces (`read <role> HANDOFF and continue`), so this is the point to measure first
  (section 10, P2). Every element still passes `first_unsafe_argument` (`;` and the existing `HostedArgv` checks), which is kept even
  though `--exec` should make `;` harmless: `wt.exe` still reads the host line before `wsl.exe` does.
- **Calling convention of the wrapper:** `<wrapper> -- claude <args...>`, the shape of the pilot's `lane-sandbox -- <cmd>`. The
  wrapper must not append a positional after claude's argv (variadic rule above). It owns the sandbox, the environment and the proxies.
- **`lane-restart host` stays on Windows and stays the host.** The tab `wt.exe` opens runs
  `lane-restart host --role R -- wsl.exe ...` (`host_wrapped_argv` with the WSL argv in place of `claude ...`), so the ConPTY, the
  startup-prompt handlers (`approvals`) and the dev-channels dialog (never auto-answered, `AwaitingConfirmation`) work as for any lane.
  **UNVERIFIED:** that `wsl.exe` under a ConPTY relays claude's startup dialog bytes the host matches on (pilot step P1). If it does
  not, the fallback is a Linux `lane-restart host` inside the distro (the P-slices of `docs/PORT-LINUX-PLAN.md`), which is a different
  design and is not specified here.
- `spawn_launch`'s `wt.exe` / `conhost.exe` branches are reused as they are; only the argv the host is given differs.

## 4. State: where a WSL lane's records live

The distro mounts no Windows drive (`automount=false`) and has no interop, so a hook inside the distro **cannot write
`C:/Projects/.lane-state`**. Therefore:

- The hooks inside the distro run the **Linux** build of `lane-restart state <event>` (CI already builds `ubuntu-latest`) and write the
  lane's `<role>.json` to `wsl.state_dir` inside the distro. `LANE_ROLE` is set by the wrapper.
- The Windows `lane-restart` **reads** that file through the distro's UNC path, `\\wsl.localhost\<distro>` + `wsl.state_dir`, read-only.
  It never writes there. **UNVERIFIED** (P3): that a UNC read works with `automount=false` / `interop=false` (those switches govern
  the Linux side's view of Windows, not Windows' view of the distro), and that **it starts a stopped distro** (expected, and it matters:
  see O3).
- The launch spec and the ledger stay on Windows in `C:/Projects/.lane-state` as today. A WSL lane's spec is written by the Windows side.

## 5. Identity, liveness and stop (the `SystemFacts` seam)

Windows `lane-restart` decides everything through `SystemFacts` (`facts.rs`: `is_alive_claude_process`, `cwd_of`,
`has_live_shell_descendant`, `transcript_is_recent`, `process_identity`, `kill_verified`, `find_claude_process_in`, `process_table`).
A WSL lane gets an implementation, `WslFacts`, selected by the spec's `launch`; the Windows `SysinfoFacts` is untouched. What each
fact means for a WSL lane:

| fact | Windows lane | WSL lane |
|---|---|---|
| the process that IS the lane | `claude.exe`, pid + start time + exe name | the **`wsl.exe` the host spawned**: its pid + start time are recorded in the spec (`stopped_pid_start_secs` as today); the claude inside is its descendant, not in the Windows table |
| alive | pid alive, image is `claude` | that `wsl.exe` is alive **and** the in-distro probe (below) finds the lane's claude. Either alone is not alive. |
| cwd | the process's cwd (prefix refused, `paths_match`) | the recorded POSIX `cwd` compared **exactly** (no case folding: ext4) against the state file's; the live cwd is not read |
| transcript | `~/.claude/projects/...jsonl` on Windows | the same file read at `\\wsl.localhost\...\home\<user>\.claude\projects\...` |
| live shell below it | descendants in the Windows table | the state file's own `no_background_shells` claim **plus** the probe's process list; if the probe fails the answer is `Err(ShellCheckError)`, which blocks a stop (closed direction), never "no shells" |
| kill | `kill_verified(pid, identity)` | the same verified kill of the `wsl.exe` tree, **then** the probe must confirm no claude of that lane remains; a survivor is `KillError`, reported, not retried blindly |

**The one command lane-restart runs inside a distro:** `wsl.exe -d <distro> -u <user> --exec /bin/ps -u <user> -o pid=,lstart=,args=`
(fixed argv, no wrapper, no user input in it), read as data. It lists the lane user's processes (the sandbox's pid namespace is
outside the lane, so `bwrap` and `claude` show). **Measured 2026-10-24 (P4, stand-in `lane-sandbox -- sleep 600`, no claude):** the
list holds the `systemd --user` and shell of the lane user, **two `bwrap` processes whose command line ENDS WITH the lane's own argv**
(`bwrap --die-with-parent ... /usr/bin/sleep 600`), and the inner command itself. So a match on "args carry `--name <role>`" would
also match both `bwrap` lines. **The rule is therefore: a process is the lane's claude when its `argv[0]` basename is `claude`, its
args carry `--name <role>`, and its start time is not before the host's `wsl.exe` start time** (the same "after the launch" rule
`find_claude_process_in` uses); a `bwrap` line is never the lane. `lstart` has one-second resolution (as the Windows start times do).
**Measured too:** `TerminateProcess` on the Windows-side `wsl.exe` ended the whole tree (0 survivors 4 s later, control: 3 processes
listed while it ran), because the PM's wrapper passes `bwrap --die-with-parent`. That is the wrapper's property, not lane-restart's:
if a wrapper does not set it, the stop above fails closed with the survivor named. Not measured: the same with a real `claude`.

**Identity of the caller for `--self`.** `Target::Myself` compares the caller's `claude` pid with the state file's. A WSL lane's claude is
a Linux process: the Windows `lane-restart` cannot be its caller (no interop), so **`--self` does not exist for a WSL lane** and is
refused with a named reason (`SelfNotAvailableForWsl`), never silently treated as `Other`.

## 6. How a WSL lane restarts (the HANDOFF flow, unchanged in shape)

1. The lane writes its HANDOFF in the distro, then runs the Linux `lane-restart assert-idle --role R` (state file in `wsl.state_dir`).
   Same checks as today (`busy: false`, `subagents_running: 0`, `no_background_shells: true`).
2. The lane sends the PM a Synapse `[NOTICE]` that it is ready to be restarted (the compact format; this is the lane's message, not a
   lane-restart feature, and lane-restart does not talk to Synapse).
3. **The PM** (or a person) runs `lane-restart --role R [--yes]` on Windows: the existing `Target::Other` path, which is the one that
   needs no caller identity (`decide`: identity, idle, shells; section 5's `WslFacts`). Kill, then launch from the spec (section 3), then
   `wait_for_liveness` on the new session in the **UNC-read** state file.
4. The next session reads HANDOFF and begins, as every lane does.

`lane-start` / `lane-stop` (RESTART-TOOL-DESIGN 14) work the same way for a WSL spec: `stop` writes the spec `stopping`, verified kill,
`suspended`; `start` launches from it. The PM-only rule for `start` is unchanged and **is not relaxed here**.

## 7. What is refused (all before anything is touched)

`launch: wsl` with a missing `wsl.distro` / `user` / `wrapper`; any field failing its rule in 2; a `wrapper` that is not absolute POSIX;
`--self`; a model that is not Sonnet or Haiku without the Opus override (as today); a distro not in `wsl -l -q` (read as data, the
`-q` output is UTF-16 on Windows and parsed as such, tested with a real UTF-16 fixture); a WSL spec on a non-Windows host (there is no
`wsl.exe`; refused, not skipped).

## 8. Threat notes (what this adds)

- A spec file is data written by lane-restart; the new fields are validated at read as well as write, so a hand-edited spec cannot make
  lane-restart run another binary in the distro: the program is `wsl.exe`, the distro/user come from the shape-checked fields, and the
  wrapper is one absolute path, run with `--exec`.
- The probe in 5 is a fixed argv; it takes no field of the spec except `distro` and `user`, both already shape-checked.
- lane-restart never starts the distro on purpose and never writes into it. A UNC read may start it (O3).
- Nothing here changes who may start a lane (PM or a person), the Hard/Soft stop rules, or consent.

## 9. Red-first test plan (the code PRs; each seen red before it is green, all without a real distro)

1. Spec round-trip: a Windows spec is byte-identical to today's golden file; a WSL spec writes `schema: 2` with the fields of 2 and reads
   back equal; **an old-build reader** (a test pinned to `SCHEMA = 1` logic) answers `UnsupportedSchema` for it.
2. Field validation, one case per rule of 2 and 7 (`-x` distro, `root`, relative wrapper, `..`, `;`, Windows path as `cwd`, unknown `launch`).
3. `wsl_command(spec, claude_argv)` is pure and returns the exact argv of 3 (golden test): prompt first, dev-channels flag last, `--exec`,
   `--cd`, `claude` after `--`; a recording `SpawnFn` asserts that `prepare_launch` still refuses Opus for a WSL spec.
4. `WslFacts` over a fake runner (the probe's output is injected): alive needs both `wsl.exe` and the probe; a probe error blocks the stop;
   a surviving claude after the kill is `KillError`; `--name` and start-time matching pick the lane's claude, not a sibling lane's.
5. `--self` for a WSL spec is `SelfNotAvailableForWsl`; `Target::Other` goes through `decide` with `WslFacts`.
6. Windows lanes: the existing suites unchanged and green (the regression test of section 1).
7. A UTF-16 `wsl -l -q` fixture (distro present / absent / the BOM and `\r\n` forms).
The real-process half (P1 to P5 below) is the pilot's measurement, not a CI test: CI has no distro.

## 10. Pilot steps (UNVERIFIED items to measure, the PM or the lane in the distro; none was run for this spec)

| id | measure | decides |
|---|---|---|
| P1 | `lane-restart host -- wsl.exe -d lane-spike -u lane-commysdk --exec /usr/local/bin/lane-sandbox -- claude ...` shows the dev-channels dialog and a handler matches it | host stays on Windows (3) |
| P2 | **MEASURED 2026-10-24, pass.** `--exec` keeps each element ONE argument: a spaced prompt, `a;b`, `q"uote`, `--name`, a literal `$HOME` (not expanded); `--cd /home/lane-commysdk/work/commy-sdk-rust` takes the POSIX path; the wrapper's `-- cmd` convention works. Driven by Python `subprocess` (the MSVCRT quoting Rust's `Command` uses), not by a Rust build | the argv of 3 |
| P3 | `Get-ChildItem \\wsl.localhost\lane-spike\home\lane-commysdk\.local\state\lane-state` works with `automount=false`; whether it starts a stopped distro | state reads (4), O3 |
| P4 | **MEASURED 2026-10-24 with a stand-in, pass with one fix (section 5: `bwrap` lines also carry the lane's argv).** `wsl.exe ... --exec /bin/ps -u lane-commysdk -o pid=,lstart=,args=` sees the whole tree from outside the sandbox; killing the Windows-side `wsl.exe` ended all of it. A real `claude` is still to be seen | identity and stop (5) |
| P5 | the Linux `lane-restart state` hook writes `wsl.state_dir/<role>.json` under the wrapper (`$HOME` is the lane user's own) | state (4) |

## 11. Open questions for the PM

- **O1.** `--self` is refused for a WSL lane (5, 6): the lane asks the PM by Synapse and the PM restarts it. Acceptable for the pilot, or
  should the Linux build be given a way to ask the Windows side itself? (No such channel exists without interop; recommend the PM path.)
- **O2.** `wsl.state_dir` default (`/home/<user>/.local/state/lane-state`) and the wrapper path: yours to confirm. Recommend a spec field,
  not a built-in, so a second distro costs no code.
- **O3.** If a UNC read starts the distro, a `lane-restart` status on a **suspended** WSL lane would wake it. Recommend: reads of a WSL
  lane's state happen only for a lane whose spec is `running`/`stopping`, never for `suspended`, and the probe is never run against a
  stopped distro (`wsl -l -v` says `Stopped` first; that read does not start it).
- **O4.** Schema 2 for WSL specs only (2) versus schema 2 for every spec: recommend the former (Windows specs and old readers untouched).
