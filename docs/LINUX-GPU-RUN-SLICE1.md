# Linux `gpu-run`, slice 1: `gpu-run.sh` (spec, test plan, script text)

Written 2026-10-30 at agentlife `origin/main` bb1ee51. PM ruling 2026-10-30: slice 1 of `docs/LINUX-GPU-RUN-DESIGN.md`
(#189) is this note, in this repo. **`cuda-build.sh`, the runner twin and the container check are later slices.**
The script and its tests are inline below because `scripts/gpu-run.sh` belongs in Fuel, whose lane has not agreed yet.

## Measured, and where

Both files below were run **once**, on this box's WSL2 Ubuntu 24.04 (not `lane-spike`), copied to `/tmp`:
`bash -n` clean, **22/22 checks pass** (the 13 numbered tests below; some assert twice). That is WSL2's Linux kernel with a
tmpdir, so it shows the `flock(1)` behaviour of the design, **not**: `/run/lock` on tmpfs, `fs.protected_regular`, a second
user, a container, a real reset, a GPU, or Rust interop. Those stay in "To verify on hardware" of #189. No shellcheck here.

Mutation: giving the child `9<&-` (the lock fd closed for the child) turned T6 red (2 checks); restored, 22/22.
The first draft also failed T6 for a real reason, now fixed: it checked the timeout only after a 60 s `flock -w`, so a
bound under a minute was not honoured. Each wait slice is now `min(60, time left)`.

## Differences from the #189 design (all deliberate)

1. **The lock file is empty; the metadata is `current.json` in the state dir.** #189 put both in one file. Bash cannot
   open a file read-write without `O_CREAT`, and `O_CREAT` on another user's file in `/run/lock` is exactly what
   `fs.protected_regular` denies. So the lock fd is a read-only open (enough for `flock`), and `current.json`
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

## Follow-on (not started)

Propose to the Fuel lane (`hnl6s8w0`), after this merges: land `scripts/gpu-run.sh` + `scripts/gpu-run-test.sh` in Fuel,
run the test on a real Linux install (tmpfs `/run/lock`, a second user, `fs.protected_regular=2`), and answer #189's open
questions (bash vs a small Rust binary; whether forge's slot code moves to a shared crate). That needs the Fuel lane's
agreement and a Linux box; this lane writes nothing in Fuel.

## `gpu-run.sh`

```bash
#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# gpu-run (Linux) - machine-wide serialization of GPU-touching runs, one at a time.
# Usage: gpu-run.sh -Project <name> [--] <cmd> <args...>
# Contract and reasoning: agentlife docs/LINUX-GPU-RUN-DESIGN.md and LINUX-GPU-RUN-SLICE1.md.
# The lock is flock(2) on LOCK; the kernel drops it when the last fd on the open file
# description closes (SIGKILL included). The child INHERITS the lock fd on purpose.
# There is no cross-OS lock and none is needed: only one OS owns the GPU at a time.
set -u

readonly GPU_RUN_LINUX_VERSION=1                    # Windows gpu-run.ps1 is v3; separate numbering
readonly LOCK="${GPU_RUN_LOCK:-/run/lock/gpu-run.lock}"      # PROTOCOL CONSTANT: never version the path
readonly STATE="${GPU_RUN_STATE_DIR:-/var/lib/gpu-run}"
# GPU_RUN_LOCK / GPU_RUN_STATE_DIR exist for the test suite. A different LOCK path means NO exclusion.
if [ -n "${GPU_RUN_TIMEOUT_SEC:-}" ]; then TIMEOUT_SEC=$GPU_RUN_TIMEOUT_SEC    # test hook
else TIMEOUT_SEC=$(( ${GPU_RUN_TIMEOUT_MIN:-45} * 60 )); fi
readonly LOCKFD=9 CURRENT="$STATE/current.json" LOG="$STATE/gpu-run.log"

log() { printf '%s [%s v%s pid=%s] %s\n' "$(date -u +%FT%TZ)" "${PROJECT:-?}" "$GPU_RUN_LINUX_VERSION" "$$" "$*" >>"$LOG" 2>/dev/null || true; }
say() { printf 'gpu-run: %s\n' "$*" >&2; log "$*"; }
json_str() { printf '%s' "$1" | tr '\n\r\t' '   ' | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'; }
json_get() { sed -n "s/.*\"$1\": *\"\{0,1\}\([^\",}]*\)\"\{0,1\}.*/\1/p" "$CURRENT" 2>/dev/null | head -n 1; }

PROJECT=""
if [ "${1:-}" = "-Project" ] && [ $# -ge 2 ]; then PROJECT=$2; shift 2; fi
[ "${1:-}" = "--" ] && shift
if [ -z "$PROJECT" ]; then echo 'gpu-run: -Project <name> must be the first argument. Usage: gpu-run.sh -Project <name> -- <cmd> <args...>' >&2; exit 64; fi
if [ $# -eq 0 ]; then echo 'gpu-run: no command given.' >&2; exit 64; fi
CMDSTR="$*"

# Open WITHOUT O_CREAT (fs.protected_regular denies O_CREAT on another user's file in /run/lock).
# A missing file is a setup error, never a silent private lock. Read-only is enough for flock.
if [ ! -e "$LOCK" ] || ! exec 9<"$LOCK"; then
  echo "gpu-run: cannot open $LOCK (missing or not readable). It is created at boot by systemd-tmpfiles; see LINUX-GPU-RUN-SLICE1.md." >&2
  exit 78
fi
if [ ! -d "$STATE" ] || [ ! -w "$STATE" ]; then
  echo "gpu-run: state dir $STATE missing or not writable (setup error)." >&2; exit 78
fi

# --- Nesting: pass through only if the lock is REALLY held by the claimed pid -----------------
if [ "${GPU_RUN_HELD:-}" = "1" ]; then
  if ! flock -n "$LOCKFD" 2>/dev/null && [ "$(json_get pid)" = "${GPU_RUN_HELD_PID:-x}" ]; then
    log "nested gpu-run, passing through (held by pid=$GPU_RUN_HELD_PID): $CMDSTR"
    exec "$@"
  fi
  say "GPU_RUN_HELD=1 but the lock is not held by pid=${GPU_RUN_HELD_PID:-?}; ignoring it and acquiring"
  unset GPU_RUN_HELD GPU_RUN_HELD_PID
fi

# --- Acquire, bounded -------------------------------------------------------------------------
START=$SECONDS
if ! flock -n "$LOCKFD"; then
  say "WAIT blocked by: $(cat "$CURRENT" 2>/dev/null || echo '(no current.json)')"
  while :; do
    slice=$(( TIMEOUT_SEC - (SECONDS - START) )); [ "$slice" -gt 60 ] && slice=60; [ "$slice" -lt 1 ] && slice=1
    flock -w "$slice" "$LOCKFD" && break
    waited=$(( SECONDS - START ))
    hp=$(json_get pid)
    if [ -n "$hp" ] && ! kill -0 "$hp" 2>/dev/null; then
      say "still waiting ${waited}s: recorded wrapper pid=$hp is DEAD but the lock is held, so a descendant holds it (fuser $LOCK)"
    else
      say "still waiting ${waited}s for $(json_get project) pid=$hp"
    fi
    if [ "$waited" -ge "$TIMEOUT_SEC" ]; then
      say "TIMEOUT after ${waited}s waiting for the GPU lock; a holder is hung. Aborting."
      exit 75
    fi
  done
  say "acquired after $(( SECONDS - START ))s wait"
fi

# --- Stale detection: we hold the lock, so a surviving current.json is a dead holder's ----------
if [ -s "$CURRENT" ]; then
  old_boot=$(json_get boot_id); cur_boot=$(cat /proc/sys/kernel/random/boot_id 2>/dev/null || echo unknown)
  if [ "$old_boot" != "$cur_boot" ]; then
    say "RECLAIM: the machine went down during a GPU run (boot_id changed). Previous: $(cat "$CURRENT")"
  else
    say "RECLAIM: the previous wrapper was killed without cleanup (same boot). Previous: $(cat "$CURRENT")"
  fi
fi
printf '{"v":%s,"pid":%s,"project":"%s","cmd":"%s","start":"%s","boot_id":"%s"}\n' \
  "$GPU_RUN_LINUX_VERSION" "$$" "$(json_str "$PROJECT")" "$(json_str "$CMDSTR")" "$(date -u +%FT%TZ)" \
  "$(cat /proc/sys/kernel/random/boot_id 2>/dev/null || echo unknown)" >"$CURRENT"
log "START: $CMDSTR"

# --- Run the child holding the lock; forward signals; clean up only on a normal end ----------
export GPU_RUN_HELD=1 GPU_RUN_HELD_PID=$$
child=0
fwd() { [ "$child" -ne 0 ] && kill -s "$1" "$child" 2>/dev/null; }
trap 'fwd TERM' TERM; trap 'fwd INT' INT; trap 'fwd HUP' HUP
"$@" & child=$!
wait "$child"; rc=$?
while kill -0 "$child" 2>/dev/null; do wait "$child"; rc=$?; done     # a trapped signal ends the first wait early
rm -f "$CURRENT"
log "END rc=$rc"
exit "$rc"
```

## `gpu-run-test.sh`

```bash
#!/usr/bin/env bash
# Tests for gpu-run.sh. No root needed: GPU_RUN_LOCK / GPU_RUN_STATE_DIR point into a temp dir.
# Usage: gpu-run-test.sh /path/to/gpu-run.sh
set -u
GR=$(readlink -f "$1")
D=$(mktemp -d); trap 'kill $(jobs -p) 2>/dev/null; rm -rf "$D"' EXIT
export GPU_RUN_LOCK="$D/lock" GPU_RUN_STATE_DIR="$D/state"
mkdir -p "$GPU_RUN_STATE_DIR"; : >"$GPU_RUN_LOCK"
pass=0; fail=0
ok() { if [ "$2" = "$3" ]; then pass=$((pass+1)); echo "ok   $1"; else fail=$((fail+1)); echo "FAIL $1: want [$3] got [$2]"; fi; }
has() { if grep -q -- "$3" "$2"; then pass=$((pass+1)); echo "ok   $1"; else fail=$((fail+1)); echo "FAIL $1: no [$3] in $2"; fi; }
waitfor() { for _ in $(seq 1 100); do eval "$1" && return 0; sleep 0.1; done; return 1; }
reset() { rm -f "$GPU_RUN_STATE_DIR"/*; }

# T1 exit code passes through
"$GR" -Project t -- sh -c 'exit 7'; ok "T1 exit code" $? 7
# T2 usage errors
"$GR" -- true 2>/dev/null; ok "T2a no -Project" $? 64
"$GR" -Project t 2>/dev/null; ok "T2b no command" $? 64
# T3 missing lock file is a setup error, not a private lock (and is not created)
GPU_RUN_LOCK="$D/nolock" "$GR" -Project t -- true 2>/dev/null; ok "T3 missing lock" $? 78
ok "T3 not created" "$([ -e "$D/nolock" ] && echo yes || echo no)" no
# T4 mutual exclusion: second wrapper waits while the first runs (control: it runs after)
reset
"$GR" -Project a -- sh -c 'sleep 3' & A=$!
waitfor '[ -s "$GPU_RUN_STATE_DIR/current.json" ]'
t0=$SECONDS; "$GR" -Project b -- true 2>"$D/b.err"; rc=$?; el=$((SECONDS-t0))
ok "T4 second ran" $rc 0
ok "T4 second waited (>=2s)" "$([ $el -ge 2 ] && echo y || echo n)" y
has "T4 logged WAIT" "$D/b.err" WAIT
wait $A
# T5 SIGKILL of wrapper AND child frees the lock; reclaim is loud and says same boot
reset
setsid "$GR" -Project k -- sleep 60 & sleep 0.5
waitfor '[ -s "$GPU_RUN_STATE_DIR/current.json" ]'
WP=$(sed -n 's/.*"pid":\([0-9]*\).*/\1/p' "$GPU_RUN_STATE_DIR/current.json")
pkill -KILL -P "$WP"; kill -KILL "$WP"; sleep 0.3
"$GR" -Project n -- true 2>"$D/n.err"; ok "T5 acquired after kill" $? 0
has "T5 RECLAIM same boot" "$D/n.err" "same boot"
# T6 orphan child keeps the lock after the wrapper is SIGKILLed; waiter times out with 75 and names the dead pid
reset
setsid "$GR" -Project o -- sleep 8 & sleep 0.5
waitfor '[ -s "$GPU_RUN_STATE_DIR/current.json" ]'
WP=$(sed -n 's/.*"pid":\([0-9]*\).*/\1/p' "$GPU_RUN_STATE_DIR/current.json"); kill -KILL "$WP"; sleep 0.3
GPU_RUN_TIMEOUT_SEC=1 timeout 90 "$GR" -Project w -- true 2>"$D/w.err"; ok "T6 timeout exit" $? 75
has "T6 says dead wrapper" "$D/w.err" "DEAD"
pkill -f 'sleep 8'; sleep 0.3
# T7 a boot_id change is reported as machine-went-down
reset
printf '{"v":1,"pid":1,"project":"old","cmd":"x","start":"s","boot_id":"other-boot"}\n' >"$GPU_RUN_STATE_DIR/current.json"
"$GR" -Project n -- true 2>"$D/n.err"; has "T7 RECLAIM boot" "$D/n.err" "machine went down"
# T8 clean run leaves no current.json (control for T5/T7: this is what makes them signal)
ok "T8 cleaned" "$([ -e "$GPU_RUN_STATE_DIR/current.json" ] && echo left || echo gone)" gone
# T9 nesting passes through when truly held (no deadlock); output proves the inner ran
reset
out=$("$GR" -Project p -- "$GR" -Project q -- echo inner 2>/dev/null); ok "T9 nested" "$out" inner
# T10 forged GPU_RUN_HELD with nothing held does not bypass: it acquires normally
reset
GPU_RUN_HELD=1 GPU_RUN_HELD_PID=1 "$GR" -Project f -- true 2>"$D/f.err"; ok "T10 forged rc" $? 0
has "T10 ignored" "$D/f.err" "ignoring it"
# T11 forged GPU_RUN_HELD while ANOTHER wrapper holds: must not pass through
reset
"$GR" -Project h -- sleep 2 & sleep 0.5
waitfor '[ -s "$GPU_RUN_STATE_DIR/current.json" ]'
t0=$SECONDS; GPU_RUN_HELD=1 GPU_RUN_HELD_PID=1 "$GR" -Project f -- true 2>/dev/null
ok "T11 forged did not skip the wait" "$([ $((SECONDS-t0)) -ge 1 ] && echo y || echo n)" y
wait
# T12 SIGTERM to the wrapper reaches the child and the wrapper cleans up
reset
"$GR" -Project s -- sleep 30 & W=$!; waitfor '[ -s "$GPU_RUN_STATE_DIR/current.json" ]'
kill -TERM $W; wait $W; ok "T12 rc 143" $? 143
ok "T12 cleaned" "$([ -e "$GPU_RUN_STATE_DIR/current.json" ] && echo left || echo gone)" gone
# T13 the child really inherits the lock fd (flock -n from outside fails during the run)
reset
"$GR" -Project i -- sleep 2 & sleep 0.5
flock -n "$GPU_RUN_LOCK" true; ok "T13 outside flock refused during run" $? 1
wait
flock -n "$GPU_RUN_LOCK" true; ok "T13 control: free after" $? 0

echo "pass=$pass fail=$fail"; [ "$fail" -eq 0 ]
```
