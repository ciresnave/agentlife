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

# T14 the child reads the wrapper's stdin. A bare `"$@" &` without job control gives the child /dev/null.
# Control first: the same pipe straight into cat, so an empty result below is the wrapper's doing.
reset
ok "T14 control: pipe into cat" "$(printf 'hello
' | cat)" hello
ok "T14 stdin reaches the child" "$(printf 'hello
' | "$GR" -Project in -- cat 2>/dev/null)" hello
# T15 SIGINT to the wrapper kills the child (an async child ignores SIGINT unless the wrapper resets it).
# The wrapper itself is started the same way the fix starts the child: `&` alone would hand it an ignored
# SIGINT, which a script cannot trap. The control is T12's SIGTERM path, which already passes.
reset
( trap - INT QUIT; exec "$GR" -Project si -- sleep 20 ) <&0 & W=$!
waitfor '[ -s "$GPU_RUN_STATE_DIR/current.json" ]'
t0=$SECONDS; kill -INT $W; wait $W; rc=$?
ok "T15 rc 130 (child died of SIGINT)" $rc 130
ok "T15 prompt (child did not run out its 20 s)" "$([ $((SECONDS-t0)) -lt 10 ] && echo y || echo n)" y
ok "T15 cleaned" "$([ -e "$GPU_RUN_STATE_DIR/current.json" ] && echo left || echo gone)" gone

# T16 the wrapper adds no ignored signals to its child: the child's SigIgn mask equals the mask of the same
# command run directly from this shell. Covers SIGQUIT (and INT) even where the environment already ignores
# QUIT on entry (WSL does), which a kill-based test cannot separate. The old `"$@" &` child had INT and QUIT added.
reset
mask() { sh -c 'grep SigIgn /proc/$$/status'; }
direct=$(mask)
wrapped=$("$GR" -Project sm -- sh -c 'grep SigIgn /proc/$$/status' 2>/dev/null)
ok "T16 control: the mask line was read" "$([ -n "$direct" ] && echo y || echo n)" y
ok "T16 child's SigIgn equals a direct child's (INT and QUIT not added)" "$wrapped" "$direct"

echo "pass=$pass fail=$fail"; [ "$fail" -eq 0 ]
