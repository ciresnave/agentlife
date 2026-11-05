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

# /run/lock is a directory inside the /run tmpfs on most distros, not a mount of its own: ask which filesystem holds it.
FST=$(stat -f -c %T /run/lock 2>/dev/null || echo unknown)
echo "kernel: $(uname -r); /run/lock is on: $FST"
ok "S0 /run/lock is on tmpfs" "$FST" tmpfs
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

# S1 the protection is really on, and WHEN it applies. The kernel exempts a file owned by the directory's owner, so
# the root-owned lock the tmpfiles line creates is NOT protected (control, measured), while a file another user
# created in the sticky /run/lock IS: gpurt2's O_CREAT open is refused. S6 below runs the wrapper against that file.
as gpurt2 bash -c ': >> /run/lock/gpu-run.lock' 2>/dev/null; ok "S1a root-owned lock: O_CREAT open by another user is NOT refused (owner exemption)" $? 0
install -m 0666 -o gpurt1 -g gpurt /dev/null /run/lock/gpu-run-ctl
as gpurt2 bash -c ': >> /run/lock/gpu-run-ctl' 2>/dev/null; ok "S1b control: a user-owned file in /run/lock IS protected (O_CREAT open refused)" $? 1
rm -f /run/lock/gpu-run-ctl
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
# S5 both users' lines are in the shared log (the first user creates it; the second must still be able to append).
ok "S5 log has the first user's lines" "$([ "$(grep -c '\[u1 ' /var/lib/gpu-run/gpu-run.log)" -ge 1 ] && echo y || echo n)" y
ok "S5 log has the second user's lines" "$([ "$(grep -c '\[u2 ' /var/lib/gpu-run/gpu-run.log)" -ge 1 ] && echo y || echo n)" y
ok "S5 log is group-writable" "$(stat -c %A /var/lib/gpu-run/gpu-run.log | cut -c5-6)" rw
# S6 the protected case: the lock file was created by gpurt1 (so protected_regular applies to everyone else).
# The wrapper only opens it read-only, so gpurt2 still runs; this is the case the read-only open exists for.
rm -f /run/lock/gpu-run.lock; install -m 0666 -o gpurt1 -g gpurt /dev/null /run/lock/gpu-run.lock
as gpurt2 "$GR" -Project s6 -- true 2>/dev/null; ok "S6 second user runs against a user-owned lock file" $? 0
# S7 a stale current.json another user left 0644 (a crashed holder) is replaced, not silently kept.
as gpurt1 bash -c 'umask 022; printf "{\"v\":1,\"pid\":1,\"project\":\"stale\",\"cmd\":\"x\",\"start\":\"s\",\"boot_id\":\"%s\"}\n" "$(cat /proc/sys/kernel/random/boot_id)" > /var/lib/gpu-run/current.json'
as gpurt2 "$GR" -Project s7 -- sleep 2 2>/dev/null & B=$!
sleep 1
ok "S7 current.json names the new holder, not the stale one" "$(grep -c '"project":"s7"' /var/lib/gpu-run/current.json)" 1
wait "$B"

echo "pass=$pass fail=$fail"; [ "$fail" -eq 0 ]
