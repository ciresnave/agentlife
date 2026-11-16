# Native gpu-run (Windows)

`crates/gpu-run` builds `gpu-run.exe`, the same protocol as baracuda/fuel `scripts/gpu-run.ps1` v3,
for callers that must not start PowerShell. Why: on 2026-11-15 `powershell -NoProfile -Command 1` hung
over 60 s on the dev host (Defender's engine wedged, AMSI waits on it), so `pwsh -File scripts/gpu-run.ps1`
never took the lock. The exe starts in 0.09 s in that same state (measured 2026-11-15, debug build).

    gpu-run -Project <name> -- <cmd> <args...>

Same as the ps1: mutex `Global\gpu-run` (open first, create only when absent, ACL Synchronize|Modify
for Authenticated Users), `%TEMP%\gpu-run.lock` (JSON `v,sv,pid,project,cmd,start`) and
`%TEMP%\gpu-run.log` (`<ts> [<project> v3 pid=<pid>] <msg>`), nesting by `GPU_RUN_HELD` /
`GPU_RUN_HELD_PID`, abandoned-mutex and dead-pid stale-lock reclaim, a 60 s wait slice with a warning,
`GPU_RUN_TIMEOUT_MIN` (default 45) then exit 75. Differences: timestamps are UTC (`...Z`) where the ps1
writes the local offset; a command that cannot start exits 127.

Tests: `crates/gpu-run` unit tests (arguments, lines, timestamps) and `scripts/gpu-run-native-test.sh`
(bash: exit code, log, lockfile, mutual exclusion, nesting, killed-holder reclaim). UNMEASURED: the
interleave with the ps1 (`scripts/gpu-run-native-test.sh <exe> <gpu-run.ps1>`), which needs a PowerShell
that can start; the harness prints SKIPPED, not a pass, when it cannot. The fuel/baracuda copies are untouched.

## Install path (PM 2026-11-15)

Runners are separate accounts, so the exe lives in one machine-wide directory, like `C:\sccache`:

    cargo install --path crates/gpu-run --root C:\gpu-run --locked
    # -> C:\gpu-run\bin\gpu-run.exe ; callers: C:\gpu-run\bin\gpu-run.exe -Project fuel -- <cmd>

Re-run it after each change to `crates/gpu-run` (cargo install replaces the exe). Not installed yet: the
install is a machine change, done after this merges and CireSnave/the PM say so.

## Log timestamps: UTC here, local offset in the ps1

`gpu-run.log` lines from this exe read `2026-11-15T13:09:00.1234567Z` (UTC); the ps1 writes
`2026-11-15T06:09:00.1234567-07:00` (local, the same instant). Mixed logs are still in time order
when read by instant, not by text.
