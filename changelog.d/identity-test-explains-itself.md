kind: the identity many-rounds test reports what held a pid when a killed child is not NotRunning
---
Test only, no behaviour change. `identity::tests::a_killed_child_is_never_a_different_exe_over_many_real_rounds` failed once on a
Windows CI leg of #283 (`1 of 320 killed children were not NotRunning: [Same]`, issue #284) and passed on rerun. It could not be
reproduced locally in 7520 real rounds (4 and 8 threads; pids were reused in about 7.8 % of rounds but never within the same start
second, so the same-second pid recycle hypothesis has no support). Each non-`NotRunning` verdict now carries the recorded identity,
what held the pid at that moment, and what held it 50 ms and 500 ms later, so the next occurrence says whether a live holder stayed
(a recycled pid) or a dead process was still listed. Verified: the test passes locally; the cause is NOT found.

— lane: agentlife
