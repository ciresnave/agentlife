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

# Files in the shared state dir are created group-writable (umask 002 in a subshell, so the CHILD keeps its umask):
# a second user in the group must be able to append to the log and replace a stale current.json. Found by the CI
# system test on a real second user: the log silently lost every line a user wrote after another user created it 0644.
log() { ( umask 002; printf '%s [%s v%s pid=%s] %s\n' "$(date -u +%FT%TZ)" "${PROJECT:-?}" "$GPU_RUN_LINUX_VERSION" "$$" "$*" >>"$LOG" ) 2>/dev/null || true; }
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
# rm first: a stale current.json another user left 0644 cannot be opened for writing, but the (group-writable,
# non-sticky) directory lets us replace it. We hold the lock, so nobody else is writing it.
rm -f "$CURRENT"
( umask 002; printf '{"v":%s,"pid":%s,"project":"%s","cmd":"%s","start":"%s","boot_id":"%s"}\n' \
  "$GPU_RUN_LINUX_VERSION" "$$" "$(json_str "$PROJECT")" "$(json_str "$CMDSTR")" "$(date -u +%FT%TZ)" \
  "$(cat /proc/sys/kernel/random/boot_id 2>/dev/null || echo unknown)" >"$CURRENT" )
log "START: $CMDSTR"

# --- Run the child holding the lock; forward signals; clean up only on a normal end ----------
export GPU_RUN_HELD=1 GPU_RUN_HELD_PID=$$
child=0
fwd() { [ "$child" -ne 0 ] && kill -s "$1" "$child" 2>/dev/null; }
trap 'fwd TERM' TERM; trap 'fwd INT' INT; trap 'fwd HUP' HUP
# Without job control (a script), a bare `"$@" &` hands the child /dev/null as stdin and SIGINT/SIGQUIT set to
# ignored (bash manual, "Job Control Basics"/"Signals"; measured on WSL2 Ubuntu 24.04, see SLICE1 doc). The
# subshell restores both: `<&0` keeps our stdin, `trap - INT QUIT` runs in the forked subshell, before `exec`,
# and exec keeps the disposition. A terminal's ^C/^\ go to the whole foreground group, so the child gets them
# directly; `fwd INT` covers a `kill -INT <wrapper>`. If the wrapper's OWN SIGINT was ignored on entry
# (started with `&` from a script), bash cannot trap it: start it like this, or from a terminal.
( trap - INT QUIT; exec "$@" ) <&0 & child=$!
wait "$child"; rc=$?
while kill -0 "$child" 2>/dev/null; do wait "$child"; rc=$?; done     # a trapped signal ends the first wait early
rm -f "$CURRENT"
log "END rc=$rc"
exit "$rc"
