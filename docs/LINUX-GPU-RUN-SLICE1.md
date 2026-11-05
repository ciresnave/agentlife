# Linux `gpu-run`, slice 1: `gpu-run.sh` (spec, test plan, script text)

Written 2026-10-30 at agentlife `origin/main` bb1ee51. PM ruling 2026-10-30: slice 1 of `docs/LINUX-GPU-RUN-DESIGN.md`
(#189) is this note, in this repo. **`cuda-build.sh`, the runner twin and the container check are later slices.**
The script and its tests are inline below because `scripts/gpu-run.sh` belongs in Fuel, whose lane has not agreed yet.

## Measured, and where

Both files below were run **once**, on this box's WSL2 Ubuntu 24.04 (not `lane-spike`), copied to `/tmp`:
`bash -n` clean, **22/22 checks pass** (T1-T13; some assert twice; T14 and T15 were added 2026-11-05, 27/27, see below). That is WSL2's Linux kernel with a
tmpdir, so it shows the `flock(1)` behaviour of the design, **not**: `/run/lock` on tmpfs, `fs.protected_regular`, a second
user, a container, a real reset, a GPU, or Rust interop. Those stay in "To verify on hardware" of #189. No shellcheck here.

Mutation: giving the child `9<&-` (the lock fd closed for the child) turned T6 red (2 checks); restored, 22/22.
The first draft also failed T6 for a real reason, now fixed: it checked the timeout only after a 60 s `flock -w`, so a
bound under a minute was not honoured. Each wait slice is now `min(60, time left)`.

## Differences from the #189 design (all deliberate)

1. **The lock file is empty; the metadata is `current.json` in the state dir.** #189 put both in one file. Bash cannot
   open a file read-write without `O_CREAT`, and `O_CREAT` on another user's file in `/run/lock` is what
   `fs.protected_regular` denies. **Corrected 2026-11-05 from the native CI run: the kernel exempts a file owned by
   the directory's owner, so the root-owned lock the tmpfiles line creates is NOT protected (measured, S1a); the
   protection bites when a *user* created the file (S1b), the case the read-only open is for (S6).** So the lock fd is a read-only open (enough for `flock`), and `current.json`
   (group-writable `/var/lib/gpu-run`, not sticky) carries the record. Stale = `current.json` still present once we hold
   the lock; the holder is the only writer while it holds, so no race.
2. **Fixed fd 9.** `exec {fd}<` was tried first; the child must inherit the lock, and a fixed number is the form whose
   inheritance is certain. A child sees fd 9 open; do not use it.
3. **Nesting is verified, not trusted.** `GPU_RUN_HELD=1` passes through only if the lock is held *and* `current.json`'s
   pid equals `GPU_RUN_HELD_PID`; otherwise it is logged, ignored, and the wrapper acquires (T10, T11).
4. **Test hooks:** `GPU_RUN_LOCK`, `GPU_RUN_STATE_DIR`, `GPU_RUN_TIMEOUT_SEC`. A different `GPU_RUN_LOCK` means *no*
   exclusion with other participants; the harness must never set it. `GPU_RUN_TIMEOUT_MIN` (45) is the public knob.
5. Exit codes: 75 timeout (as Windows), 64 usage, 78 setup error (lock or state dir missing/unwritable; the lock file is
   never created by the wrapper).

## Setup it requires (not done by the script)

```
# /etc/tmpfiles.d/gpu-run.conf   (systemd-tmpfiles --create)
f /run/lock/gpu-run.lock 0666 root root -
d /var/lib/gpu-run       2775 root gpu  -
```
Group name `gpu` is a placeholder for the group the runner and the interactive user share. Requires util-linux `flock`.

## Test plan (what `gpu-run-test.sh` asserts, with the control for each absence)

| T | Asserts | Control |
|---|---|---|
| 1 | child's exit code is the wrapper's | exit 7 is neither 0 nor 1 |
| 2 | usage errors exit 64 | |
| 3 | missing lock file exits 78 and is **not** created | existence checked after |
| 4 | second wrapper waits while the first runs, logs `WAIT` | it then runs and exits 0 |
| 5 | SIGKILL of wrapper and child frees the lock; next acquire logs `RECLAIM ... same boot` | T8: a clean run leaves no `current.json` |
| 6 | wrapper SIGKILLed, **orphan child keeps the lock**; waiter exits 75 and says the recorded pid is DEAD | mutation `9<&-` turns it red |
| 7 | different `boot_id` in a surviving `current.json` logs "machine went down" | T5 is the same-boot arm |
| 9 | nested wrapper passes through (no deadlock) | |
| 10, 11 | forged `GPU_RUN_HELD` neither bypasses a real holder (waits) nor blocks when nothing is held | |
| 12 | SIGTERM reaches the child; rc 143; `current.json` removed | |
| 13 | an outside `flock -n` fails during a run and succeeds after | |
| 14 | the child reads the wrapper's stdin | the same pipe into `cat` |
| 15 | SIGINT to the wrapper kills the child, rc 130, `current.json` removed, well inside the child's 20 s | T12's SIGTERM path |

## Follow-on (not started)

Propose to the Fuel lane (`hnl6s8w0`), after this merges: land `scripts/gpu-run.sh` + `scripts/gpu-run-test.sh` in Fuel,
run the test on a real Linux install (tmpfs `/run/lock`, a second user, `fs.protected_regular=2`), and answer #189's open
questions (bash vs a small Rust binary; whether forge's slot code moves to a shared crate). That needs the Fuel lane's
agreement and a Linux box; this lane writes nothing in Fuel.

## The scripts

**Moved out of this note on 2026-11-05 into `scripts/gpu-run.sh` and `scripts/gpu-run-test.sh`** (LF, `.gitattributes`
`*.sh text eol=lf`), so CI runs the real files and the text cannot drift from what is tested. They still belong in Fuel
once its lane agrees; this repo carries them until then. CI job `gpu-run` (ubuntu-latest) runs
`bash scripts/gpu-run-test.sh scripts/gpu-run.sh` (temp dir, one user: the 27 checks) and then, as root on the throwaway
runner, `scripts/gpu-run-system-test.sh` on the real default paths: `/run/lock` must be tmpfs, `fs.protected_regular=2`
is set and shown active by a control (another user's O_CREAT open is refused), a second user in the shared group runs
the wrapper and excludes the first. Those three things (tmpfs `/run/lock`, a second user, `fs.protected_regular=2`) are
**not** claimed proven until that job's first green run; the system test was **not run locally** (it needs root and
changes users and a sysctl; only `bash -n` and its root guard were run).

## 2026-11-05 corrections (Fuel's review, each checked here first)

| Claim | Checked | Result |
|---|---|---|
| Child of `"$@" &` gets `/dev/null` as stdin | WSL2 Ubuntu 24.04: child's `/proc/self/fd/0`; control: same child in the foreground | **real** (`/dev/null` vs a pipe) |
| Child of `"$@" &` ignores SIGINT/SIGQUIT | `SigIgn` mask `0x6` vs `0x4`; `kill -INT` on an async `sleep` left it alive, a plain subshell child died | **real for SIGINT**; SIGQUIT is already ignored on entry in this environment, so it is the manual's word, not measured |
| Design item 7: forge takes slots | `git grep slot` in forge, build, cutlass-sys at baracuda 35060e3a (control: `fs2` found) | **wrong**, design corrected |

Fix in `gpu-run.sh`: `( trap - INT QUIT; exec "$@" ) <&0 & child=$!`. `set -m` also works but gives the child its own
process group (a tty read would stop it); `<&0` alone fixes only stdin. Tests T14 (stdin) and T15 (SIGINT to the wrapper)
were **red against the old script** (T14 got empty stdin; T15 ran out its 20 s, rc 0 not 130), with controls (T14: the same
pipe into `cat`; T15: T12's SIGTERM path), and are green after: **27/27** on WSL2 Ubuntu 24.04. A wrapper that was itself
started with `&` from a script has SIGINT ignored on entry, which bash cannot trap, so T15 starts it the same way the fix
starts the child.
