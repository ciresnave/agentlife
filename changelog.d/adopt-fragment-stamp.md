kind: CI: one changelog fragment per PR
---
- **Changelog fragments and a stamp script (PM order of CireSnave 2026-11-12).** `tools/stamp-version.py`, `tools/check-fragment.py`, the `changelog-fragment` workflow and `changelog.d/README.md` are adopted from the portfolio kit: a PR carries one `changelog.d/<topic>.md` fragment and no version edit; the PM stamps the version at merge. This PR carries a fragment itself so it does not collide with open version bumps. Verified with a throwaway fragment and `stamp-version.py 99.99.99 --dry-run`.
