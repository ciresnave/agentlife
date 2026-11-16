kind: Changed: portfolio checks run CodeRipper 0.47.5
---
CI pins the shared portfolio workflow at coderipper c72a6e0 (coderipper#356), which installs CodeRipper 0.47.5 (was 33cc3b7, 0.43.49). `tools/migrate-rule-ids.py --dry-run` from that SHA reports nothing to migrate: this repo names no renamed rule. The Portfolio job of this PR is the evidence for new findings.
