# Linux `gpu-run` and `cuda-build`: design

Written 2026-10-30 at Fuel `origin/main` (scripts read with `git show origin/main:scripts/gpu-run.ps1`, v3, and
`scripts/cuda-build.ps1`, v2). PM task 2026-10-30 (Fuel ROADMAP hardware-free first step): the desktop will
dual-boot Windows and Linux; this is the Linux counterpart of the Windows GPU token/lock and CUDA build slot.
**Docs only. Nothing here was run on Linux**: every Linux behaviour below is from the man pages and general
knowledge and is listed in "To verify on hardware" before it is relied on.

## What must carry over (the contract, not the mechanism)

From the Windows scripts, these are the properties to keep:

1. **Machine-wide exclusion, atomic.** One GPU-touching run at a time (`gpu-run`); at most N CUDA builds
   (`cuda-build`). No stat-then-create.
2. **Released when the holder dies, including SIGKILL and a crash.** No pid-liveness guessing to *release*.
3. **Loud reclaim.** A waiter that inherits a dead holder's lock says so (stderr + log). The 2026-07-31 incident
   is why: "previous run crashed" and "clean start" must be distinguishable afterwards.
4. **Bounded wait.** A *hung* holder never releases; warn each minute, abort at the bound with exit 75.
   `GPU_RUN_TIMEOUT_MIN` (45) and `CUDA_BUILD_TIMEOUT_MIN` (60) keep their names and defaults.
5. **The harness takes the lock, not the agent.** Same CLI: `gpu-run -Project <name> -- <cmd> <args...>`.
6. **Nesting passes through** via `GPU_RUN_HELD` / `CUDA_BUILD_HELD`; child exit code becomes the wrapper's.
7. **Protocol constants are shared by every participant** (slot name pattern, slot count N, threads K), and
   `baracuda-forge` (Rust) participates in the slot namespace, so the protocol must be speakable from Rust and
   shell, not only from PowerShell.

## Mechanism: `flock(2)` on files in a tmpfs, not a semaphore, not a pwsh port

| Windows | Linux |
|---|---|
| `Global\gpu-run` named mutex | `flock(LOCK_EX)` on `/run/lock/gpu-run.lock` |
| `Global\cuda-build-slot-<i>`, `WaitAny` | N files `/run/lock/cuda-build-slot-<i>`, try each with `LOCK_NB`, poll |
| `AbandonedMutexException` | not needed: the kernel drops a `flock` when the last fd on the open file description closes |
| lockfile = metadata only | the **same file** carries the metadata, and the lock lives on it |

Why `flock`:

- **Atomic, released on any death** (SIGKILL, OOM kill, panic of the holder). Same guarantee as the mutex.
- **Speakable from bash (`flock(1)`, util-linux) and Rust (`libc::flock` / `fd-lock`)**, so `baracuda-forge`
  joins without a .NET runtime. Do **not** reuse `gpu-run.ps1` under `pwsh` on Linux: .NET's Unix named mutexes
  live in `/tmp/.dotnet/shm`, are not visible to non-.NET participants, and have their own per-user rules. They
  would give a second, disjoint lock, which is the "different name means no exclusion" trap.
- **Not a POSIX/SysV semaphore for the counted slots.** The `cuda-build.ps1` header explains why a semaphore
  leaks a count when a holder is killed (POSIX `sem_open` has the same flaw; SysV `SEM_UNDO` is fragile).
  N `flock` files keep per-slot recovery.
- **`/run/lock` is tmpfs**, so locks cannot survive a reboot (the property the Windows header claims for
  `%TEMP%`; here it is true by construction).

### The `Global\` trap, Linux edition

A bare name had to be `Global\` on Windows because `Local\` is per logon session. The Linux equivalents:

- **Mount namespaces.** A container (Docker with GPU, a Flatpak, a `systemd-nspawn`) has its own `/run/lock`
  and would silently get its own lock. The path must be **bind-mounted** into any GPU-using container
  (`-v /run/lock/gpu-run.lock:/run/lock/gpu-run.lock`), and the wrapper must refuse to run in a container
  that does not show the real file (check: the file's `st_dev`/`st_ino` match the host-published value in
  `/run/lock/gpu-run.id`, or just document the mount and test it).
- **Users.** The Windows script assumes one interactive user. Keep that assumption, but note a second user
  (a runner service account) is the realistic Linux case. Then permissions matter, see next point.
- **`fs.protected_regular`.** With `fs.protected_regular=1` or `2` (common default), `open(O_CREAT)` of an
  *existing* file owned by another user in a sticky world-writable directory such as `/run/lock` fails with
  `EACCES` even at mode 0666. So the lock files must be **created at boot by `systemd-tmpfiles`**
  (`f /run/lock/gpu-run.lock 0666 root root`, one line per slot) and the wrapper must open them **without
  `O_CREAT`**. A missing file is a loud setup error, not a silent private lock.

## `gpu-run` (Linux)

`scripts/gpu-run.sh`, bash, requires util-linux `flock`. Mutex path and `GpuRunVersion` are protocol constants
exactly as on Windows (never version the path).

1. Parse `-Project NAME [--] cmd...` by hand (same CLI as the `.ps1`).
2. If `GPU_RUN_HELD=1`: pass through only if the lock is **really held** by `GPU_RUN_HELD_PID`
   (`flock -n` on the file fails *and* the metadata pid matches). The Windows script trusts the variable, so a
   long-lived process started inside a held run (a tmux server) later passes through while holding nothing.
   Cheap to fix here, so fix it.
3. `exec {fd}<>/run/lock/gpu-run.lock`. Try `flock -n`. On failure, log `WAIT` with the metadata, then loop
   `flock -w 60`, warn on stderr each minute, `exit 75` at the bound.
4. **Stale detection, simpler and stronger than Windows.** The lock is on the metadata file itself. If we
   acquire it and the file still has content, the previous holder never cleaned up. No pid check and no pid-reuse
   false negative (`gpu-run.ps1` v3 has one). Distinguish by the recorded `boot_id` against
   `/proc/sys/kernel/random/boot_id` (see "Reboots").
5. Truncate and write the metadata (`v, sv, pid, project, cmd, start, boot_id`), set `GPU_RUN_HELD`,
   `GPU_RUN_HELD_PID`, run the child **without** closing the lock fd, truncate the file on clean exit, exit with
   the child's code.

**Deliberate difference: the child inherits the lock fd** (do not use `flock -o`, do not set `O_CLOEXEC`).
Windows ties the mutex to the wrapper thread, so a GPU child that outlives a killed wrapper runs unprotected.
On Linux an orphan still using the GPU keeps the lock until it exits, which is correct. The cost: after
`kill -9 gpu-run`, waiters stay blocked while an orphan lives. The wait-timeout message must therefore also
say when the recorded wrapper pid is dead but the lock is still held (descendants; `fuser`/`lsof` on the file
names them), because that is the Linux version of the "hung holder" state.

## `cuda-build` (Linux)

`scripts/cuda-build.sh`, same shape: N files `cuda-build-slot-0..N-1`, a `LOCK_NB` pass over all slots, then
sleep 1 s plus jitter and retry (`flock` has no wait-any; that is the only real loss against `WaitAny`, and it
costs a second of latency on a 10-30 minute build). Same warn-at-first-block, then each minute, abort at 60 min
with 75. Per-slot metadata JSON in the slot file, same stale rule as above. Sets `BARACUDA_FORGE_THREADS=K`
(logging an override) and `CUDA_BUILD_HELD` / `_PID` / `_SLOT`. Run order for a build that then executes on the
device is unchanged: outer `cuda-build`, inner `gpu-run`, different files, no deadlock.

### N and K are NOT copied across

N=2, K=4 (so N x C x K = 16) was measured on Windows against **host address space** consumed by concurrent
`ptxas` (an allocation failed near 22). Linux has no such ceiling in the same form; the constraint there is
RAM plus overcommit and cgroup limits, and the footprint of a flash-attention `ptxas` (100-180 MB per
intermediate) still applies. So N and K are **per-OS protocol constants that must be re-measured on the Linux
install**; start with N=2, K=4 as the conservative default and write down the measurement. They never run
concurrently with the Windows ones (one OS is booted), so they do not need to match, but **all Linux
participants (Fuel, baracuda, vulkane, forge) must agree with each other**, the same "most permissive
participant sets the budget" rule. An optional second belt, not a replacement:
`systemd-run --scope -p MemoryMax=...` around the build, so an overshoot kills the build, not the desktop.

## Reboots and the dual boot

- **No cross-OS lock, and none is needed.** Only one OS owns the GPU at a time; a lock file on a shared NTFS
  partition would be worse than nothing (no `flock` semantics across drivers). State this in both scripts'
  headers so nobody adds one.
- **Locks do not survive a reboot** (tmpfs / kernel objects), which is right: a held lock must not outlive the
  process that held it.
- **The evidence should.** The Windows lockfile and log live in `%TEMP%`, which is not reliably cleared and
  not designed as a crash record. On Linux keep a small persistent record under `/var/lib/gpu-run/` (group
  writable): a `current.json` written at start and removed at clean exit, plus the append-only
  `gpu-run.log`. At the next acquire, a surviving `current.json` with a **different `boot_id`** means "the
  machine went down during a GPU run" (host crash, power loss, hard reset: the 2026-07-31 class), and is logged
  loudly with the project and command. The same `boot_id` means the wrapper was killed. Windows cannot make
  this distinction today.
- **Hibernation / Windows Fast Startup** can leave GPU or driver state that Linux does not own. Document that
  the Linux side should only be entered by a full shutdown or restart of Windows; not something a script can
  enforce.
- **WSL2 on the Windows side is a separate hole**: a WSL2 process using the GPU (GPU-PV) does not take
  `Global\gpu-run` unless it goes through the Windows wrapper. Out of scope here, noted so it is not assumed
  covered.

## Runner

The self-hosted GPU job gets a Linux twin: labels `[self-hosted, linux, gpu]` (+ `gpu-cuda` / `gpu-vulkan`),
`gpu-run.sh` as the in-job mutex, the same `FUEL_REQUIRE_<FAMILY>` environment contract. When the box is booted
into Windows, the Linux runner is offline and its jobs queue; that is expected and the workflows must not
treat "no Linux runner" as a failure of the Windows one. The runner service runs as a user in the group that
owns the `/var/lib/gpu-run` directory and can open the lock files (tmpfiles mode above).

## To verify on hardware (nothing above is measured)

1. `flock` is dropped on `kill -9` of the holder with nobody waiting, and with a waiter (control: the same run
   with the holder left alive keeps the waiter blocked).
2. Orphaned child keeps the lock after the wrapper is killed, and `fuser` names it.
3. `fs.protected_regular=2` really denies an `O_CREAT` open of a tmpfiles-made file by a second user, and the
   non-`O_CREAT` open succeeds.
4. A container without the bind mount really gets a private lock; with it, none.
5. `boot_id` differs across a forced reset with `current.json` left behind.
6. Rust `fd-lock`/`libc::flock` and `flock(1)` contend on the same file (forge must interoperate).
7. Re-measure the `ptxas` ceiling and pick N, K.

## Open questions for the Fuel lane

- Is `bash` + util-linux acceptable as a dependency of the Linux wrappers, or should they be a small Rust
  binary (also usable from `baracuda-forge` and from Windows)?
- Should forge's slot acquisition move into the shared Rust crate so there is one implementation per OS?
- Does Fuel want the persistent crash record on Windows too (a `ProgramData` directory), so the
  attribution gap closes on both sides?
