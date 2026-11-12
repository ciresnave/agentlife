kind: docs: `RUST-ACL-STAGE2.md`, the plan to tighten `C:\rust`
---
A plan, nothing run (board 429): which Rust runner accounts write under `C:\rust\.cargo` today because the machine `CARGO_HOME` is inherited, the per-runner private `CARGO_HOME` step (manual `.env` edit and service restart for a registered runner, `-Rearm` for one that has taken its job), then the `icacls /save` backup, the tightened ACL, the read-backs (with a deny control) and the restore line. Read from the service registry, the ACLs and each repo's workflows on 2026-11-12; unmeasured items are marked in the text.
