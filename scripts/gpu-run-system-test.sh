#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# System test for gpu-run.sh on the REAL default paths. Needs root and changes the machine (a sysctl, two users,
# one group, /run/lock/gpu-run.lock, /var/lib/gpu-run): run it in CI or a throwaway box, never on a lane's host.
# Usage: sudo gpu-run-system-test.sh /path/to/gpu-run.sh
# What it adds to gpu-run-test.sh (which uses a temp dir and one user): the default LOCK on /run/lock, a second
# user in the shared group, and fs.protected_regular=2 (the setting under which a non-O_CREAT open must be enough).
set -u
[ "$(id -u)" -eq 0 ] || { echo "needs root" >&2; exit 2; }
GR_SRC=$(readlink -f "$1")
pass=0; fail=0
ok() { if [ "$2" = "$3" ]; then pass=$((pass+1)); echo "ok   $1"; else fail=$((fail+1)); echo "FAIL $1: want [$3] got [$2]"; fi; }

echo "kernel: $(uname -r); /run/lock fstype: $(findmnt -no FSTYPE /run/lock 2>/dev/null || echo unknown)"
ok "S0 /run/lock is tmpfs" "$(findmnt -no FSTYPE /run/lock 2>/dev/null || echo unknown)" tmpfs
sysctl -w fs.protected_regular=2 >/dev/null
ok "S0 protected_regular set" "$(sysctl -n fs.protected_regular)" 2

# The setup LINUX-GPU-RUN-SLICE1.md says systemd-tmpfiles does at boot (mode and owner as there).
GR=/usr/local/bin/gpu-run.sh; install -m 755 "$GR_SRC" "$GR"
getent group gpurt >/dev/null || groupadd gpurt
for u in gpurt1 gpurt2; do id "$u" >/dev/null 2>&1 || useradd -m -g gpurt -s /bin/bash "$u"; done
install -m 0666 -o root -g root /dev/null /run/lock/gpu-run.lock
mkdir -p /var/lib/gpu-run && chown root:gpurt /var/lib/gpu-run && chmod 2775 /var/lib/gpu-run
rm -f /var/lib/gpu-run/current.json
as() { local u=$1; shift; runuser -u "$u" -- "$@"; }

# S1 control: the protection is really on. gpurt2 creating-by-open (O_CREAT) a root-owned file in the sticky
# world-writable /run/lock is refused, so S2 below passing means the wrapper's read-only open is what saves it.
as gpurt2 bash -c ': >> /run/lock/gpu-run.lock' 2>/dev/null; ok "S1 control: O_CREAT open by another user is refused" $? 1
# S2 the wrapper, as a user that does not own the lock file, runs and exits with the child's code.
as gpurt2 "$GR" -Project s2 -- sh -c 'exit 7' 2>/dev/null; ok "S2 second user runs under protected_regular=2" $? 7
ok "S2 lock file still root-owned and not recreated" "$(stat -c %U /run/lock/gpu-run.lock)" root
# S3 two users exclude each other on the default lock: the second waits (control: it then runs).
as gpurt1 "$GR" -Project u1 -- sleep 3 & A=$!
for _ in $(seq 1 100); do [ -s /var/lib/gpu-run/current.json ] && break; sleep 0.1; done
t0=$SECONDS; as gpurt2 "$GR" -Project u2 -- true 2>/tmp/gpu-run-s3.err; rc=$?; el=$((SECONDS-t0))
ok "S3 second user ran after the first" $rc 0
ok "S3 second user waited (>=2s)" "$([ "$el" -ge 2 ] && echo y || echo n)" y
ok "S3 second user was told who blocks it" "$(grep -c 'WAIT blocked by' /tmp/gpu-run-s3.err)" 1
wait "$A"
# S4 current.json is written by the holder (group-writable dir) and removed after a clean run.
ok "S4 current.json gone after a clean run" "$([ -e /var/lib/gpu-run/current.json ] && echo left || echo gone)" gone

echo "pass=$pass fail=$fail"; [ "$fail" -eq 0 ]
