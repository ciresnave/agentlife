kind: Fixed: gpu-run bash harness no longer races the holder start
---
`scripts/gpu-run-native-test.sh` waited a fixed 1 s for the first holder to take the lock; on a loaded host the exe took longer to start, so the "B waited / WAIT logged" check failed 1 run in 3 against the installed release exe (measured 2026-11-15). It now polls for the lockfile (up to 10 s). 5 runs in a row pass after the change; the exe itself is unchanged.
