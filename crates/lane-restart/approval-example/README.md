# Inert example approval

⚠️ **Nothing in `lane-restart` reads this folder at run time.** It is here only as an example of the
approval format (RESTART-TOOL-DESIGN.md §12.2) and as a test fixture (`handlers.rs`'s tests read it
under `#[cfg(test)]`). OverMind ships no active approvals: `lane-restart host` fetches them from the
repo and path each user configures in `~/.overmind/lane-restart.json` (§12.3), from that repo's
default branch only.

`claude-peers-dev-channels.json` is byte-for-byte the file seeded into
`ciresnave/ciresnave/.overmind/lane-restart/approvals/`, so its tests check the real approval's
content. It answers the `--dangerously-load-development-channels` confirmation dialog; its
provenance quotes both of CireSnave's approvals verbatim.
