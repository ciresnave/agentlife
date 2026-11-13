kind: docs: `SUPERVISION-DESIGN.md` records what Synapse already offers for the event contract
---
Section 4 and question 2 of `docs/SUPERVISION-DESIGN.md` now state what was read at synapse `origin/main` on 2026-11-13 instead of deferring to the Synapse lane: rooms exist but deliver only to online members, the daemon path has no per-sender send rule, and a new `[LIFECYCLE]` tag must be added to `MESSAGE-FORMATS.md` and the daemon's lint key table. The recommendation for question 2 changes from "until Synapse offers fan-out" to "one message per subscriber; rooms later". Docs only.
