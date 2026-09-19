// SPDX-License-Identifier: MIT OR Apache-2.0

//! Declarative startup-prompt handlers — RESTART-TOOL-DESIGN.md §12.
//!
//! ⚠️ ACTIVE HANDLERS COME ONLY FROM WHAT'S EMBEDDED AT BUILD TIME
//! ([`EMBEDDED_HANDLER_JSON`]), never a runtime file. Every handler embedded here
//! reached the binary through a merged PR against this repo — the PR is the
//! provenance record; the `provenance` field inside each handler's own JSON
//! is a human-readable restatement of what the PR already shows, not what
//! makes the handler real (§12.7). A runtime directory (`.lane-state/handler-proposals/`,
//! for the LLM-proposal loop) is never read by anything in this module.

use serde::Deserialize;
use std::collections::HashMap;

/// One handler, exactly as its own JSON file (`crates/lane-restart/handlers/*.json`)
/// declares it. Every field is required by `serde` — a handler file missing
/// any of them (most importantly `provenance`) fails to parse, which is
/// caught at build/PR-review time (§12.3), not silently defaulted.
#[derive(Debug, Clone, Deserialize)]
pub struct HandlerSpec {
    pub id: String,
    #[serde(rename = "match")]
    pub match_spec: MatchSpec,
    /// The literal keystrokes sent on an exact match, verbatim — never a
    /// structured "always confirm" toggle (§12.2).
    pub action: String,
    pub scope: ScopeSpec,
    pub provenance: Provenance,
    #[serde(default)]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MatchSpec {
    /// Every string here must appear verbatim in the captured screen text
    /// — not a fuzzy or partial match (§12.4).
    pub text_anchors: Vec<String>,
    /// Named fields pinned to an exact allowed value. Checked as the
    /// literal substring `"{name}: {value}"` in the captured screen text —
    /// an implementation choice flagged for revision once a real captured
    /// dialog confirms (or corrects) the actual formatting (§12.4).
    #[serde(default)]
    pub fields: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScopeSpec {
    /// Which lane roles this handler applies to; `"*"` matches every role.
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Provenance {
    pub approved_by: String,
    pub approved_at: chrono::DateTime<chrono::Utc>,
    /// CireSnave's own verbatim words approving THIS handler's exact spec
    /// — never the general framework approval alone (§12.1, §12.2).
    pub quote: String,
}

/// The ONLY handlers this binary can ever activate — embedded at build
/// time, one `include_str!` per file under `crates/lane-restart/handlers/`.
/// ⚠️ EMPTY BY DESIGN (PM instruction, 2026-09-18): "ship with ZERO handlers
/// embedded." Adding an entry here, and the JSON file it reads, both go
/// through this repo's own PR review (§12.7) — never a runtime edit.
///
/// ```ignore
/// const EMBEDDED_HANDLER_JSON: &[&str] = &[
///     include_str!("../handlers/claude-peers-dev-channels.json"),
/// ];
/// ```
pub const EMBEDDED_HANDLER_JSON: &[&str] = &[];

/// Parses every embedded handler, refusing (and reporting, never silently
/// dropping) any that fail to parse — malformed JSON, or missing a
/// required field (most importantly `provenance`), per §12.3.
pub fn load_embedded_handlers() -> Vec<HandlerSpec> {
    let mut handlers = Vec::new();
    for (i, json) in EMBEDDED_HANDLER_JSON.iter().enumerate() {
        match serde_json::from_str::<HandlerSpec>(json) {
            Ok(handler) => handlers.push(handler),
            Err(e) => {
                eprintln!("lane-restart: refusing embedded handler #{i} - failed to parse: {e}");
            }
        }
    }
    handlers
}

/// Whether `handler` is even eligible to be checked at all: its `scope`
/// includes `role` (or `"*"`), and it hasn't expired.
pub fn is_active(handler: &HandlerSpec, role: &str, now: chrono::DateTime<chrono::Utc>) -> bool {
    let role_ok = handler.scope.roles.iter().any(|r| r == role || r == "*");
    let not_expired = handler.expires_at.is_none_or(|exp| exp > now);
    role_ok && not_expired
}

/// Whether `"{name}: {value}"` appears in `screen_text` with the value's
/// end at a real line boundary (end-of-string, `\n`, or `\r`) — never
/// merely as a PREFIX of a longer value on the same line. ⚠️ THE PREFIX-
/// MATCH TRAP §12.4 warns about: plain `str::contains` would let a handler
/// pinned to `"Channels: server:claude-peers"` match a screen showing
/// `"Channels: server:claude-peers,server:extra"`, since the shorter
/// string IS a literal substring of the longer one — caught by this
/// crate's own `does_not_match_on_an_extra_or_different_field_value` test.
fn field_matches(screen_text: &str, name: &str, value: &str) -> bool {
    let needle = format!("{name}: {value}");
    let mut search_from = 0;
    while let Some(offset) = screen_text[search_from..].find(needle.as_str()) {
        let start = search_from + offset;
        let end = start + needle.len();
        let boundary_after = screen_text[end..]
            .chars()
            .next()
            .is_none_or(|c| c == '\n' || c == '\r');
        if boundary_after {
            return true;
        }
        search_from = start + 1;
    }
    false
}

/// Exact-match only (§12.4): every `text_anchor` must appear verbatim, and
/// every `fields` entry must appear as `"{name}: {value}"` with nothing
/// else appended to the value on the same line. Any deviation at all — an
/// extra channel, changed wording, a missing anchor — means no match; this
/// function is never a prefix or fuzzy check.
pub fn matches(handler: &HandlerSpec, screen_text: &str) -> bool {
    let anchors_ok = handler
        .match_spec
        .text_anchors
        .iter()
        .all(|a| screen_text.contains(a.as_str()));
    let fields_ok = handler
        .match_spec
        .fields
        .iter()
        .all(|(name, value)| field_matches(screen_text, name, value));
    anchors_ok && fields_ok
}

/// The first active, exactly-matching handler for `screen_text`, if any.
pub fn find_matching_handler<'a>(
    handlers: &'a [HandlerSpec],
    role: &str,
    now: chrono::DateTime<chrono::Utc>,
    screen_text: &str,
) -> Option<&'a HandlerSpec> {
    handlers
        .iter()
        .find(|h| is_active(h, role, now) && matches(h, screen_text))
}

/// A short, stable hash of a handler's exact JSON content, for
/// `lane-restart --version` (§12.9) — so anyone can see precisely what's
/// active without reading source, and a diff between two `--version`
/// outputs shows exactly what changed.
pub fn handler_content_hash(json: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(json.as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_json(id: &str, extra_field_value: Option<&str>) -> String {
        let fields = match extra_field_value {
            Some(v) => format!(r#"{{"Channels": "{v}"}}"#),
            None => "{}".to_string(),
        };
        format!(
            r#"{{
                "id": "{id}",
                "match": {{
                    "text_anchors": ["SECURITY CONFIRMATION", "local development"],
                    "fields": {fields}
                }},
                "action": "y\n",
                "scope": {{ "roles": ["overmind"] }},
                "provenance": {{
                    "approved_by": "CireSnave",
                    "approved_at": "2026-09-18T22:00:00Z",
                    "quote": "Proceed!"
                }},
                "expires_at": null
            }}"#
        )
    }

    #[test]
    fn parses_a_well_formed_handler() {
        let h: HandlerSpec = serde_json::from_str(&sample_json("test-handler", None)).unwrap();
        assert_eq!(h.id, "test-handler");
        assert_eq!(h.provenance.approved_by, "CireSnave");
        assert_eq!(h.provenance.quote, "Proceed!");
    }

    #[test]
    fn refuses_a_handler_with_no_provenance() {
        let json = r#"{
            "id": "test-handler",
            "match": {"text_anchors": [], "fields": {}},
            "action": "y\n",
            "scope": {"roles": ["overmind"]}
        }"#;
        assert!(serde_json::from_str::<HandlerSpec>(json).is_err());
    }

    #[test]
    fn refuses_malformed_json() {
        assert!(serde_json::from_str::<HandlerSpec>("{not json").is_err());
    }

    #[test]
    fn matches_when_every_anchor_and_field_is_present_exactly() {
        let h: HandlerSpec =
            serde_json::from_str(&sample_json("t", Some("server:claude-peers"))).unwrap();
        let screen = "  SECURITY CONFIRMATION\n  I am using this for local development\n  Channels: server:claude-peers\n";
        assert!(matches(&h, screen));
    }

    #[test]
    fn does_not_match_on_a_missing_anchor() {
        let h: HandlerSpec =
            serde_json::from_str(&sample_json("t", Some("server:claude-peers"))).unwrap();
        let screen = "  I am using this for local development\n  Channels: server:claude-peers\n";
        assert!(!matches(&h, screen));
    }

    #[test]
    fn does_not_match_on_an_extra_or_different_field_value() {
        // ⚠️ THE PREFIX-MATCH TRAP §12.4 warns about: "server:claude-peers"
        // plus anything else must NOT match a handler pinned to exactly
        // "server:claude-peers".
        let h: HandlerSpec =
            serde_json::from_str(&sample_json("t", Some("server:claude-peers"))).unwrap();
        let screen = "  SECURITY CONFIRMATION\n  I am using this for local development\n  Channels: server:claude-peers,server:extra\n";
        assert!(!matches(&h, screen));
    }

    #[test]
    fn is_active_respects_role_scope() {
        let h: HandlerSpec = serde_json::from_str(&sample_json("t", None)).unwrap();
        let now = chrono::Utc::now();
        assert!(is_active(&h, "overmind", now));
        assert!(!is_active(&h, "synapse", now));
    }

    #[test]
    fn is_active_treats_wildcard_scope_as_every_role() {
        let json = r#"{
            "id": "t",
            "match": {"text_anchors": [], "fields": {}},
            "action": "y\n",
            "scope": {"roles": ["*"]},
            "provenance": {"approved_by": "CireSnave", "approved_at": "2026-09-18T22:00:00Z", "quote": "q"}
        }"#;
        let h: HandlerSpec = serde_json::from_str(json).unwrap();
        assert!(is_active(&h, "anything-at-all", chrono::Utc::now()));
    }

    #[test]
    fn is_active_treats_an_expired_handler_as_not_present() {
        let json = r#"{
            "id": "t",
            "match": {"text_anchors": [], "fields": {}},
            "action": "y\n",
            "scope": {"roles": ["overmind"]},
            "provenance": {"approved_by": "CireSnave", "approved_at": "2026-09-18T22:00:00Z", "quote": "q"},
            "expires_at": "2020-01-01T00:00:00Z"
        }"#;
        let h: HandlerSpec = serde_json::from_str(json).unwrap();
        assert!(!is_active(&h, "overmind", chrono::Utc::now()));
    }

    #[test]
    fn find_matching_handler_returns_none_when_zero_handlers_embedded() {
        // ⚠️ THE SHIP-WITH-ZERO-HANDLERS PROPERTY: `load_embedded_handlers()`
        // must produce an empty list today, and this must therefore never
        // match anything - the mechanism is a no-op until a real handler
        // is merged.
        let handlers = load_embedded_handlers();
        assert!(handlers.is_empty());
        assert!(
            find_matching_handler(&handlers, "overmind", chrono::Utc::now(), "anything").is_none()
        );
    }

    #[test]
    fn handler_content_hash_is_deterministic_and_content_sensitive() {
        let a = handler_content_hash("{\"id\":\"x\"}");
        let b = handler_content_hash("{\"id\":\"x\"}");
        let c = handler_content_hash("{\"id\":\"y\"}");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 64, "sha256 hex digest is 64 chars");
    }
}
