#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Tests the native gpu-run.exe (crates/gpu-run) in a bash harness, no PowerShell involved:
#   exit code, log + lockfile lines, mutual exclusion, nesting, stale-lock reclaim.
# Then, only if PowerShell answers within 20 s, interleaves it with a gpu-run.ps1 given as $2
# (baracuda/fuel scripts/gpu-run.ps1); otherwise says SKIPPED, never passes silently.
# Usage: scripts/gpu-run-native-test.sh <path-to-gpu-run.exe> [<path-to-gpu-run.ps1>]
# Uses a private %TEMP% so it never touches the real gpu-run.log / gpu-run.lock, but the mutex
# Global\gpu-run is machine-wide: do not run it while a real GPU job holds the lock.
set -u
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'
EXE=${1:?path to gpu-run.exe}
PS1=${2:-}
T=$(mktemp -d)
WT=$(cygpath -w "$T")
export TEMP="$WT"
LOG="$T/gpu-run.log"
LOCK="$T/gpu-run.lock"
fail=0
ok()  { echo "ok   - $1"; }
bad() { echo "FAIL - $1"; fail=1; }
check() { if eval "$2"; then ok "$1"; else bad "$1  [$2]"; fi; }
now() { date +%s; }

# 1. exit code, log lines, lockfile cleaned
"$EXE" -Project t1 -- cmd /c exit 7; rc=$?
check "exit code propagates (7)" "[ $rc -eq 7 ]"
check "log has RUN, DONE exit=7, released" "grep -q 'RUN: cmd /c exit 7' '$LOG' && grep -q 'DONE exit=7' '$LOG' && grep -q 'released' '$LOG'"
check "log line shape '[t1 v3 pid=N]'" "grep -Eq '^[0-9T:.Z-]+ \[t1 v3 pid=[0-9]+\] RUN:' '$LOG'"
check "lockfile removed after a clean exit" "[ ! -e '$LOCK' ]"

# 2. mutual exclusion: B must wait for A
: > "$LOG"
a0=$(now)
"$EXE" -Project A -- cmd /c "ping -n 5 127.0.0.1 >nul" & pa=$!
# wait until A really holds the lock (its start took 0.5-1 s on a loaded box; a fixed sleep raced)
for _ in $(seq 1 100); do [ -s "$LOCK" ] && break; sleep 0.1; done
lockjson=$(cat "$LOCK" 2>/dev/null)
check "lockfile while held has the ps1 keys" "echo '$lockjson' | grep -Eq '^\{\"v\":1,\"sv\":3,\"pid\":[0-9]+,\"project\":\"A\",\"cmd\":\".*\",\"start\":\".*\"\}\$'"
b0=$(now)
"$EXE" -Project B -- cmd /c "echo b" >/dev/null; b1=$(now)
wait $pa
check "B waited for A (>= 2 s)" "[ $((b1 - b0)) -ge 2 ]"
check "log shows WAIT then 'acquired after'" "grep -q 'WAIT — GPU lock held by' '$LOG' && grep -q 'acquired after' '$LOG'"
ra=$(grep -n '\[A v3.*released' "$LOG" | head -1 | cut -d: -f1); rb=$(grep -n '\[B v3.*RUN:' "$LOG" | head -1 | cut -d: -f1)
check "A released before B ran" "[ -n '$ra' ] && [ -n '$rb' ] && [ $ra -lt $rb ]"

# 3. nesting: an inner gpu-run under an outer one passes through, no deadlock
: > "$LOG"
timeout 30 "$EXE" -Project outer -- "$EXE" -Project inner -- cmd /c "echo nested"; rc=$?
check "nested run exits 0 without deadlock" "[ $rc -eq 0 ]"
check "log has 'nested gpu-run' pass-through" "grep -q 'nested gpu-run' '$LOG'"

# 4. stale lock: kill the holder hard, the next run reclaims loudly
: > "$LOG"
"$EXE" -Project victim -- cmd /c "ping -n 30 127.0.0.1 >nul" & pv=$!
for _ in $(seq 1 100); do [ -s "$LOCK" ] && break; sleep 0.1; done
holder=$(sed -n 's/.*"pid":\([0-9]*\).*/\1/p' "$LOCK")
taskkill /F /T /PID "$holder" >/dev/null 2>&1
kill $pv 2>/dev/null
sleep 1
check "stale lockfile left behind by the killed holder" "[ -e '$LOCK' ]"
"$EXE" -Project after -- cmd /c "echo ok" >/dev/null 2>&1
check "next run logs RECLAIMED ... DEAD pid" "grep -Eq 'RECLAIMED (stale GPU lock from DEAD pid=$holder|abandoned)' '$LOG'"

# 5. interleave with the PowerShell copy, if it can start at all
if [ -n "$PS1" ]; then
  if timeout 20 powershell.exe -NoProfile -Command 1 >/dev/null 2>&1; then
    : > "$LOG"
    "$EXE" -Project native -- cmd /c "ping -n 5 127.0.0.1 >nul" & pn=$!
    sleep 1
    p0=$(now)
    powershell.exe -NoProfile -File "$PS1" -Project ps -- cmd /c "echo ps" >/dev/null 2>&1; p1=$(now)
    wait $pn
    check "ps1 waits for the native holder (>= 2 s)" "[ $((p1 - p0)) -ge 2 ]"
    "$EXE" -Project native2 -- cmd /c "echo n" >/dev/null 2>&1
    # The ps1's version moves on its own (baracuda's is v4 since 2026-11-07, the native's is v3): the line
    # shape is what must match, the number is only logged ("version skew is surfaced, not enforced").
    check "native and ps1 write the same log" "grep -Eq '\[ps v[0-9]+ pid=[0-9]+\]' '$LOG' && grep -q '\[native2 v3' '$LOG'"
  else
    echo "SKIPPED - ps1 interleave: powershell.exe did not start within 20 s (the AMSI/Defender hang this tool exists to avoid)"
  fi
else
  echo "SKIPPED - ps1 interleave: no gpu-run.ps1 path given"
fi

rm -rf "$T"
[ $fail -eq 0 ] && echo "ALL PASSED" || { echo "FAILED"; exit 1; }
