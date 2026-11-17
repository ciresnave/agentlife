kind: ci: deny.toml ignores RUSTSEC-2026-0543 (chrono) until 2026-12-17
---
PM ruling 2026-11-17 (pm-platform): chrono 0.4.45 is unmaintained (RUSTSEC-2026-0543, "No safe upgrade is available!"), so `cargo deny check advisories` failed. `deny.toml` ignores that one id with a reason and an expiry of 2026-12-17; ROADMAP row O10 tracks the move to jiff or time. After the change `cargo deny check` is clean (output in the PR body).
