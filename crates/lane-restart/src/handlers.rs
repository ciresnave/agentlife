// SPDX-License-Identifier: MIT OR Apache-2.0

//! Declarative startup-prompt handlers ("approvals") — RESTART-TOOL-DESIGN.md §12.
//!
//! ⚠️ THIS CRATE SHIPS ZERO ACTIVE HANDLERS (§12.2, CIRESNAVE-EXPECTATIONS.md
//! §5.1c, 2026-09-27: "approvals move OUT of OverMind"). Every handler the
//! host ever acts on is fetched at host startup from the repo + path the
//! USER configured (`approvals.rs`), from that repo's default branch only.
//! Nothing configured means nothing active. This module only parses,
//! validates and matches; it never decides where handlers come from.
//!
//! The one handler this repo still carries,
//! `approval-example/claude-peers-dev-channels.json`, is an INERT example:
//! nothing outside `#[cfg(test)]` reads it.

use serde::Deserialize;
use std::collections::HashMap;

/// A handler's fields exactly as its JSON declares them, NOT yet checked by
/// [`refusal_reason`]. It can only become a [`HandlerSpec`] through
/// `HandlerSpec::try_from`, so nothing unchecked can reach the host. Every
/// field except `expires_at` is required, and an unknown field is an error
/// (`deny_unknown_fields`) - a typo such as `"expires"` must refuse the
/// handler, never silently mean "no expiry" (§12.3).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawHandlerSpec {
    pub id: String,
    #[serde(rename = "match")]
    pub match_spec: MatchSpec,
    /// The keystrokes sent on an exact match, written in a CLOSED token
    /// grammar ([`parse_action`]): at most three tokens, each one ASCII
    /// letter or digit (at most one such token), `\r`, `<Enter>` or
    /// `<Down>`. Never a raw escape sequence, never free text - short and
    /// closed enough that no secret can ride in it (§12.3a). The host sends
    /// [`action_bytes`], not this text.
    pub action: String,
    pub scope: ScopeSpec,
    pub provenance: Provenance,
    #[serde(default)]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// A handler that passed [`refusal_reason`]. The only ways to get one are
/// `TryFrom<RawHandlerSpec>` and `Deserialize` (which goes through it), and
/// the inner value is private and read-only (`Deref`), so a handler that was
/// never checked, or was edited after the check, cannot be constructed
/// (RESTART-TOOL-DESIGN.md §12.3).
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "RawHandlerSpec")]
pub struct HandlerSpec(RawHandlerSpec);

impl TryFrom<RawHandlerSpec> for HandlerSpec {
    type Error = String;

    fn try_from(raw: RawHandlerSpec) -> Result<Self, String> {
        match refusal_reason(&raw) {
            Some(reason) => Err(reason),
            None => Ok(Self(raw)),
        }
    }
}

impl std::ops::Deref for HandlerSpec {
    type Target = RawHandlerSpec;

    fn deref(&self) -> &RawHandlerSpec {
        &self.0
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatchSpec {
    /// Every string here must appear verbatim in the captured screen text
    /// — not a fuzzy or partial match (§12.4). At least one is required.
    pub text_anchors: Vec<String>,
    /// Named fields pinned to an exact allowed value, checked as
    /// `"{name}: {value}"` ending at a line boundary (§12.4).
    #[serde(default)]
    pub fields: HashMap<String, String>,
    /// Whole-line pins: each must equal one screen line EXACTLY, after
    /// trimming surrounding whitespace - never a substring or a prefix of
    /// it. This is how a handler pins a value such as the folder path on a
    /// trust dialog: the anchor `C:\Projects\agentlife` is a substring of
    /// `C:\Projects\agentlife-evil`, the line pin is not. A line the
    /// terminal wrapped can never match (it fails closed).
    #[serde(default)]
    pub lines: Vec<String>,
    /// Like [`lines`](Self::lines), and the row right after it must be blank
    /// (or the last row). A path the terminal HARD-wrapped at the screen
    /// edge continues on the next row, which is not blank: so a trusted path
    /// exactly one column short of the width, followed by more text that
    /// wrapped, cannot pass as the trusted path. Use it for a value that can
    /// be long (a folder path); a line followed by other text (a menu
    /// entry) belongs in `lines`.
    #[serde(default)]
    pub isolated_lines: Vec<String>,
    /// Makes this a FOLDER-TRUST PREFIX approval (`crate::folder_trust`, board 244): answers the
    /// folder-trust dialog for any folder strictly under this plain absolute path (for example
    /// `C:\Projects`), after the path has been resolved on disk. Such a record must carry the
    /// folder-trust anchors and lines, no `fields` or `isolated_lines`, and the action `<Down><Enter>`
    /// (see [`refusal_reason`]); a host older than this field refuses the whole record as an unknown field.
    #[serde(default)]
    pub folder_prefix: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeSpec {
    /// Which lane roles this handler applies to; `"*"` matches every role.
    pub roles: Vec<String>,
}

/// A human-readable restatement of who approved this and in what words.
/// ⚠️ Not what makes the handler real - that is the user's own merge into
/// their approvals repo's default branch (§12.7). Checked for completeness
/// only.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub approved_by: String,
    /// Every approval this handler rests on, oldest first, each quoted
    /// verbatim. At least one is required.
    pub approvals: Vec<Approval>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    /// The day it was said. A date, not a time: a guessed time of day would
    /// be a made-up fact.
    pub said_on: chrono::NaiveDate,
    /// The approver's own words, verbatim - never a paraphrase.
    pub quote: String,
    /// What those words were answering.
    pub approving: String,
}

/// Words that mark a dialog as asking for a secret. Matched as whole words
/// (or whole word sequences) after lowercasing and replacing every
/// non-alphanumeric character with a space, so `"PIN:"` hits but
/// `"pinned"` does not. ⚠️ Secrets are never approvals (§12.3a): a handler
/// mentioning one of these is refused at load, and a screen mentioning one
/// is never answered, whatever handler matched it.
const SECRET_WORDS: &[&str] = &[
    "password",
    "passwords",
    "passphrase",
    "passcode",
    "pin",
    "secret",
    "secrets",
    "token",
    "tokens",
    "credential",
    "credentials",
    "apikey",
    "api key",
    "private key",
    "otp",
    "2fa",
    "mfa",
    "one time code",
    "one time password",
    "verification code",
    "security code",
    "recovery code",
    "seed phrase",
    "recovery phrase",
];

/// The first [`SECRET_WORDS`] entry `text` contains as a whole word, if any.
pub fn secret_word_in(text: &str) -> Option<&'static str> {
    let normalized: String = text
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect();
    let padded = format!(
        " {} ",
        normalized.split_whitespace().collect::<Vec<_>>().join(" ")
    );
    SECRET_WORDS
        .iter()
        .copied()
        .find(|w| padded.contains(&format!(" {w} ")))
}

/// Most tokens an action may hold.
pub const MAX_ACTION_TOKENS: usize = 3;

/// One key of the closed action grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyToken {
    /// One ASCII letter or digit.
    Char(char),
    /// Enter: `\r` or `<Enter>`.
    Enter,
    /// The Down arrow: `<Down>`.
    Down,
}

/// Parses `action` in the closed grammar, or `None`. At most
/// [`MAX_ACTION_TOKENS`] tokens, at most ONE of them a letter or digit (so
/// no code such as "123" can be typed), and nothing else: no ESC or other
/// control byte, no other `<...>` name, no space, no free text.
pub fn parse_action(action: &str) -> Option<Vec<KeyToken>> {
    let mut tokens = Vec::new();
    let mut rest = action;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("<Down>") {
            tokens.push(KeyToken::Down);
            rest = r;
        } else if let Some(r) = rest.strip_prefix("<Enter>") {
            tokens.push(KeyToken::Enter);
            rest = r;
        } else {
            let c = rest.chars().next()?;
            if c == '\r' {
                tokens.push(KeyToken::Enter);
            } else if c.is_ascii_alphanumeric() {
                tokens.push(KeyToken::Char(c));
            } else {
                return None;
            }
            rest = &rest[c.len_utf8()..];
        }
        if tokens.len() > MAX_ACTION_TOKENS {
            return None;
        }
    }
    let chars = tokens
        .iter()
        .filter(|t| matches!(t, KeyToken::Char(_)))
        .count();
    (!tokens.is_empty() && chars <= 1).then_some(tokens)
}

/// Whether `action` is in the closed grammar ([`parse_action`]).
pub fn action_is_allowed(action: &str) -> bool {
    parse_action(action).is_some()
}

/// The bytes the host writes for a valid `action`: a letter or digit as
/// itself, Enter as `\r`, Down as the ANSI cursor-down sequence `ESC [ B`
/// (measured by OverMind through ConPTY to answer the folder-trust dialog).
/// `None` for an action outside the grammar. The ESC lives only here, never
/// in a handler file.
pub fn action_bytes(action: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for t in parse_action(action)? {
        match t {
            KeyToken::Char(c) => out.push(c as u8),
            KeyToken::Enter => out.push(b'\r'),
            KeyToken::Down => out.extend_from_slice(b"\x1b[B"),
        }
    }
    Some(out)
}

/// Whether `id` is usable as a file stem and a log key: 1-64 characters of
/// lowercase ASCII letters, digits and `-`.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Shortest text anchor accepted. An anchor like `"1"` would match almost
/// any screen; eight characters keeps every anchor a real phrase.
pub const MIN_ANCHOR_LEN: usize = 8;

/// Why `handler` must be refused, or `None` if it may load (§12.3, §12.3a).
/// Parsing already enforced the required fields; this enforces everything
/// a well-formed but unsafe handler could still get wrong.
fn refusal_reason(handler: &RawHandlerSpec) -> Option<String> {
    if !valid_id(&handler.id) {
        return Some(format!(
            "id {:?} must be 1-64 chars of a-z, 0-9 and '-'",
            handler.id
        ));
    }
    if handler.match_spec.text_anchors.is_empty() {
        return Some("match.text_anchors is empty - it would match any screen".into());
    }
    if let Some(a) = handler
        .match_spec
        .text_anchors
        .iter()
        .find(|a| a.trim().chars().count() < MIN_ANCHOR_LEN)
    {
        return Some(format!(
            "text anchor {a:?} is shorter than {MIN_ANCHOR_LEN} characters"
        ));
    }
    if let Some(l) = handler
        .match_spec
        .lines
        .iter()
        .chain(handler.match_spec.isolated_lines.iter())
        .find(|l| l.trim().chars().count() < MIN_ANCHOR_LEN || l.contains(['\r', '\n']))
    {
        return Some(format!(
            "pinned line {l:?} is shorter than {MIN_ANCHOR_LEN} characters or spans lines"
        ));
    }
    if let Some(prefix) = &handler.match_spec.folder_prefix {
        if let Some(reason) = folder_prefix_refusal(prefix, handler) {
            return Some(reason);
        }
    }
    if !action_is_allowed(&handler.action) {
        return Some(format!(
            "action {:?} is not a keystroke sequence of at most 3 tokens (at most one letter or \
             digit, \\r, <Enter>, <Down>) - secrets are never approvals",
            handler.action
        ));
    }
    let mut texts: Vec<&str> = vec![handler.id.as_str()];
    texts.extend(handler.match_spec.text_anchors.iter().map(String::as_str));
    texts.extend(handler.match_spec.lines.iter().map(String::as_str));
    texts.extend(handler.match_spec.isolated_lines.iter().map(String::as_str));
    texts.extend(handler.match_spec.folder_prefix.as_deref());
    for (name, value) in &handler.match_spec.fields {
        texts.push(name);
        texts.push(value);
    }
    if let Some(w) = texts.iter().find_map(|t| secret_word_in(t)) {
        return Some(format!(
            "it matches a dialog that mentions {w:?} - secrets are never approvals"
        ));
    }
    if handler.scope.roles.is_empty() {
        return Some("scope.roles is empty".into());
    }
    if handler.provenance.approved_by.trim().is_empty() {
        return Some("provenance.approved_by is empty".into());
    }
    if handler.provenance.approvals.is_empty() {
        return Some("provenance.approvals is empty".into());
    }
    if handler
        .provenance
        .approvals
        .iter()
        .any(|a| a.quote.trim().is_empty() || a.approving.trim().is_empty())
    {
        return Some("a provenance approval has an empty quote or `approving`".into());
    }
    None
}

/// The only dialog a folder-prefix record may answer, and the only way it may answer it.
const TRUST_ANCHOR: &str = "Quick safety check";
const TRUST_LINES: [&str; 2] = ["Accessing workspace:", "Yes, I trust this folder"];
const TRUST_ACTION: &str = "<Down><Enter>";

/// Why a `folder_prefix` record must be refused, or `None`. Such a record can answer exactly the
/// folder-trust dialog (choosing "Yes, I trust this folder": the default is "No, exit", so Down then
/// Enter) and nothing else, whatever else its JSON says.
fn folder_prefix_refusal(prefix: &str, handler: &RawHandlerSpec) -> Option<String> {
    let Some((_, comps)) = crate::folder_trust::split_path(prefix) else {
        return Some(format!(
            "folder_prefix {prefix:?} is not a plain absolute drive path (no UNC, `..`, `/`, 8.3 name or trailing `\\`)"
        ));
    };
    if comps.is_empty() {
        return Some("folder_prefix is a drive root".into());
    }
    let m = &handler.match_spec;
    if !m.text_anchors.iter().any(|a| a.contains(TRUST_ANCHOR))
        || !TRUST_LINES
            .iter()
            .all(|l| m.lines.iter().any(|x| x.trim() == *l))
    {
        return Some(format!(
            "a folder_prefix record must pin the folder-trust dialog: anchor {TRUST_ANCHOR:?} and lines {TRUST_LINES:?}"
        ));
    }
    if !m.fields.is_empty() || !m.isolated_lines.is_empty() {
        return Some("a folder_prefix record may not also pin fields or isolated_lines".into());
    }
    if handler.action != TRUST_ACTION {
        return Some(format!(
            "a folder_prefix record's action must be exactly {TRUST_ACTION:?}, not {:?}",
            handler.action
        ));
    }
    None
}

/// Parses one handler file and refuses it (with the reason) if it fails
/// to parse or fails [`refusal_reason`]. Never partially parsed.
pub fn parse_and_validate(json: &str) -> Result<HandlerSpec, String> {
    let raw: RawHandlerSpec =
        serde_json::from_str(json).map_err(|e| format!("does not parse: {e}"))?;
    HandlerSpec::try_from(raw)
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

/// Whether some line of `screen_text` equals `pinned` exactly once both are
/// trimmed of surrounding whitespace.
fn line_matches(screen_text: &str, pinned: &str) -> bool {
    let pinned = pinned.trim();
    !pinned.is_empty() && screen_text.lines().any(|l| l.trim() == pinned)
}

/// As [`line_matches`], and the next row must be blank or absent (see
/// [`MatchSpec::isolated_lines`]).
fn isolated_line_matches(screen_text: &str, pinned: &str) -> bool {
    let pinned = pinned.trim();
    if pinned.is_empty() {
        return false;
    }
    let rows: Vec<&str> = screen_text.lines().collect();
    rows.iter().enumerate().any(|(i, row)| {
        row.trim() == pinned && rows.get(i + 1).is_none_or(|next| next.trim().is_empty())
    })
}

/// Exact-match only (§12.4): every `text_anchor` must appear verbatim,
/// every `fields` entry must appear as `"{name}: {value}"` with nothing
/// else appended to the value on the same line, and the screen must not
/// mention a secret (§12.3a) - a dialog that asks for one is never
/// answered, even by a handler whose anchors it happens to contain.
pub fn matches(handler: &HandlerSpec, screen_text: &str) -> bool {
    matches_with(handler, screen_text, &crate::folder_trust::FsResolver)
}

/// [`matches`] with the filesystem a `folder_prefix` record resolves paths on made explicit.
pub fn matches_with(
    handler: &HandlerSpec,
    screen_text: &str,
    resolver: &dyn crate::folder_trust::FolderResolver,
) -> bool {
    base_matches(handler, screen_text) && trusted_folder_of(handler, screen_text, resolver).is_ok()
}

/// For a `folder_prefix` record: `Ok(Some(path))` when the dialog's folder is trusted (the resolved
/// canonical path), `Err(())` when it is not; `Ok(None)` for every other record.
#[allow(clippy::result_unit_err)]
pub fn trusted_folder_of(
    handler: &HandlerSpec,
    screen_text: &str,
    resolver: &dyn crate::folder_trust::FolderResolver,
) -> Result<Option<String>, ()> {
    let Some(prefix) = &handler.match_spec.folder_prefix else {
        return Ok(None);
    };
    let shown = crate::folder_trust::dialog_folder(screen_text).ok_or(())?;
    crate::folder_trust::trusted_folder(prefix, &shown, resolver)
        .map(Some)
        .ok_or(())
}

fn base_matches(handler: &HandlerSpec, screen_text: &str) -> bool {
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
    let lines_ok = handler
        .match_spec
        .lines
        .iter()
        .all(|l| line_matches(screen_text, l))
        && handler
            .match_spec
            .isolated_lines
            .iter()
            .all(|l| isolated_line_matches(screen_text, l));
    anchors_ok && fields_ok && lines_ok && secret_word_in(screen_text).is_none()
}

/// The first active, exactly-matching handler for `screen_text`, if any.
pub fn find_matching_handler<'a>(
    handlers: &'a [HandlerSpec],
    role: &str,
    now: chrono::DateTime<chrono::Utc>,
    screen_text: &str,
) -> Option<&'a HandlerSpec> {
    find_matching_handler_resolved(
        handlers,
        role,
        now,
        screen_text,
        &crate::folder_trust::FsResolver,
    )
    .map(|(h, _)| h)
}

/// As [`find_matching_handler`], with the resolver explicit and the resolved folder returned for a
/// `folder_prefix` record (so the host logs exactly the path that was checked, not a second lookup).
pub fn find_matching_handler_resolved<'a>(
    handlers: &'a [HandlerSpec],
    role: &str,
    now: chrono::DateTime<chrono::Utc>,
    screen_text: &str,
    resolver: &dyn crate::folder_trust::FolderResolver,
) -> Option<(&'a HandlerSpec, Option<String>)> {
    handlers
        .iter()
        .filter(|h| is_active(h, role, now))
        .find_map(|h| {
            if !base_matches(h, screen_text) {
                return None;
            }
            trusted_folder_of(h, screen_text, resolver)
                .ok()
                .map(|folder| (h, folder))
        })
}

/// Per-anchor, per-field detail behind a `matches()` verdict - PM finding,
/// 2026-09-19: a real restart left NOTHING to explain why a handler that
/// should have matched didn't, so this exists purely for `host.rs`'s own
/// diagnostic log (`.lane-state/host-<role>-<pid>.log`), never for deciding
/// whether to inject (that stays `matches`/`find_matching_handler` alone).
pub struct MatchReport {
    pub handler_id: String,
    pub active: bool,
    /// (anchor text, whether it was found verbatim)
    pub anchors: Vec<(String, bool)>,
    /// (field name, pinned value, whether it matched at a real boundary)
    pub fields: Vec<(String, String, bool)>,
    /// (pinned line, whether some screen line equals it exactly)
    pub lines: Vec<(String, bool)>,
    /// The secret word on screen that vetoed a match, if any.
    pub secret_word: Option<&'static str>,
    /// For a `folder_prefix` record: the folder the dialog showed (or `(none)`) and whether it is
    /// trusted under the prefix on disk.
    pub folder: Option<(String, bool)>,
    pub matched: bool,
}

pub fn match_report(
    handler: &HandlerSpec,
    role: &str,
    now: chrono::DateTime<chrono::Utc>,
    screen_text: &str,
) -> MatchReport {
    let active = is_active(handler, role, now);
    let anchors: Vec<(String, bool)> = handler
        .match_spec
        .text_anchors
        .iter()
        .map(|a| (a.clone(), screen_text.contains(a.as_str())))
        .collect();
    let fields: Vec<(String, String, bool)> = handler
        .match_spec
        .fields
        .iter()
        .map(|(n, v)| (n.clone(), v.clone(), field_matches(screen_text, n, v)))
        .collect();
    let mut lines: Vec<(String, bool)> = handler
        .match_spec
        .lines
        .iter()
        .map(|l| (l.clone(), line_matches(screen_text, l)))
        .collect();
    lines.extend(handler.match_spec.isolated_lines.iter().map(|l| {
        (
            format!("(then a blank row) {l}"),
            isolated_line_matches(screen_text, l),
        )
    }));
    let secret_word = secret_word_in(screen_text);
    let folder = handler.match_spec.folder_prefix.as_ref().map(|_| {
        let shown =
            crate::folder_trust::dialog_folder(screen_text).unwrap_or_else(|| "(none)".into());
        let ok = trusted_folder_of(handler, screen_text, &crate::folder_trust::FsResolver).is_ok();
        (shown, ok)
    });
    let matched = active
        && folder.as_ref().is_none_or(|(_, ok)| *ok)
        && anchors.iter().all(|(_, ok)| *ok)
        && fields.iter().all(|(_, _, ok)| *ok)
        && lines.iter().all(|(_, ok)| *ok)
        && secret_word.is_none();
    MatchReport {
        handler_id: handler.id.clone(),
        active,
        anchors,
        fields,
        lines,
        secret_word,
        folder,
        matched,
    }
}

/// A short, stable hash of a handler's exact file content, for
/// `lane-restart approvals` and the host log (§12.9) - so anyone can see
/// precisely what's active, and a diff between two listings shows exactly
/// what changed.
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

    const PROVENANCE: &str = r#"{
        "approved_by": "CireSnave",
        "approvals": [{"said_on": "2026-09-18", "quote": "Proceed!", "approving": "a test"}]
    }"#;

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
                "action": "1\r",
                "scope": {{ "roles": ["overmind"] }},
                "provenance": {PROVENANCE},
                "expires_at": null
            }}"#
        )
    }

    fn with(json_fields: &str) -> String {
        format!(
            r#"{{
                "id": "t",
                "match": {{"text_anchors": ["SECURITY CONFIRMATION"], "fields": {{}}}},
                "scope": {{"roles": ["overmind"]}},
                "provenance": {PROVENANCE},
                {json_fields}
            }}"#
        )
    }

    #[test]
    fn an_unchecked_handler_cannot_be_deserialized_into_a_handler_spec() {
        // Audit group 5 finding 1: this JSON (bad id, no anchors, empty provenance) used to
        // deserialize into a HandlerSpec and make `check_and_inject` type "1\r" on any screen.
        let json = r#"{"id":"BAD ID","match":{"text_anchors":[]},"action":"1\r","scope":{"roles":["*"]},"provenance":{"approved_by":"","approvals":[]}}"#;
        assert!(parse_and_validate(json).is_err());
        let err = serde_json::from_str::<HandlerSpec>(json)
            .unwrap_err()
            .to_string();
        assert!(err.contains("id"), "{err}");
        // Positive control: the same shape, valid, deserializes directly.
        assert!(serde_json::from_str::<HandlerSpec>(&sample_json("ok-id", None)).is_ok());
    }

    #[test]
    fn parses_and_validates_a_well_formed_handler() {
        let h = parse_and_validate(&sample_json("test-handler", None)).unwrap();
        assert_eq!(h.id, "test-handler");
        assert_eq!(h.provenance.approved_by, "CireSnave");
        assert_eq!(h.provenance.approvals[0].quote, "Proceed!");
    }

    #[test]
    fn refuses_a_handler_with_no_provenance() {
        let json = r#"{
            "id": "test-handler",
            "match": {"text_anchors": ["SECURITY CONFIRMATION"], "fields": {}},
            "action": "1\r",
            "scope": {"roles": ["overmind"]}
        }"#;
        assert!(parse_and_validate(json).is_err());
    }

    #[test]
    fn refuses_an_unknown_field_rather_than_ignoring_it() {
        // A typo'd expiry must refuse the handler, not silently mean "never
        // expires". Positive control: the same JSON with the real field name
        // parses.
        let typo = with(r#""action": "1\r", "expires": "2020-01-01T00:00:00Z""#);
        assert!(parse_and_validate(&typo).unwrap_err().contains("expires"));
        let real = with(r#""action": "1\r", "expires_at": "2020-01-01T00:00:00Z""#);
        assert!(parse_and_validate(&real).is_ok());
    }

    #[test]
    fn refuses_the_old_single_quote_provenance_shape() {
        let json = r#"{
            "id": "t",
            "match": {"text_anchors": ["SECURITY CONFIRMATION"], "fields": {}},
            "action": "1\r",
            "scope": {"roles": ["overmind"]},
            "provenance": {"approved_by": "CireSnave", "approved_at": "2026-09-18T22:00:00Z", "quote": "q"}
        }"#;
        assert!(parse_and_validate(json).is_err());
    }

    #[test]
    fn refuses_malformed_json() {
        assert!(parse_and_validate("{not json").is_err());
    }

    #[test]
    fn refuses_an_action_long_enough_to_carry_a_secret() {
        for bad in [
            "hunter2\r",
            "1234\r",
            "",
            "yes\r",
            "1\n",
            "\u{1b}",
            "\u{1b}[B",
            "<Down>\u{1b}",
            "<Esc>",
            "<down>",
            "<Up>",
            "<Down><Down><Down><Enter>",
            "<Down>\r\r\r",
            "12",
            "1<Enter>2",
            "<Down> ",
            "<Down><Enter",
            "\u{e9}",
        ] {
            let json = with(&format!(
                r#""action": {}"#,
                serde_json::to_string(bad).unwrap()
            ));
            let err = parse_and_validate(&json).unwrap_err();
            assert!(err.contains("keystroke"), "{bad:?}: {err}");
        }
        for good in [
            "1\r",
            "y",
            "\r",
            "Y\r",
            "2",
            "<Enter>",
            "<Down>",
            "<Down><Enter>",
            "<Down>\r",
            "y<Enter>",
            "<Down><Down><Enter>",
            "\r\r",
        ] {
            let json = with(&format!(
                r#""action": {}"#,
                serde_json::to_string(good).unwrap()
            ));
            assert!(
                parse_and_validate(&json).is_ok(),
                "{good:?} must be allowed"
            );
        }
    }

    fn with_lines(lines: &str) -> HandlerSpec {
        parse_and_validate(&format!(
            r#"{{
                "id": "trust",
                "match": {{"text_anchors": ["Do you trust the files"], "lines": {lines}}},
                "action": "<Down><Enter>",
                "scope": {{"roles": ["*"]}},
                "provenance": {PROVENANCE}
            }}"#
        ))
        .unwrap()
    }

    #[test]
    fn a_pinned_line_matches_that_folder_and_not_a_longer_one() {
        let h = with_lines(r#"["C:\\Projects\\agentlife"]"#);
        let screen =
            |path: &str| format!("Do you trust the files in this folder?\n  {path}  \n1. Yes");
        // Positive control: the exact folder, with surrounding blanks, matches.
        assert!(matches(&h, &screen(r"C:\Projects\agentlife")));
        // The prefix hazard an anchor would have.
        assert!(!matches(&h, &screen(r"C:\Projects\agentlife-evil")));
        assert!(!matches(&h, &screen(r"C:\Projects\agentlife\sub")));
        assert!(!matches(&h, &screen(r"X:C:\Projects\agentlife")));
        // A wrapped path fails closed.
        assert!(!matches(
            &h,
            "Do you trust the files in this folder?\nC:\\Projects\\agent\nlife"
        ));
        // The same pin as a plain anchor WOULD match the evil folder: the reason this field exists.
        let anchor_only = parse_and_validate(&format!(
            r#"{{"id":"a","match":{{"text_anchors":["C:\\Projects\\agentlife"]}},"action":"<Down><Enter>","scope":{{"roles":["*"]}},"provenance":{PROVENANCE}}}"#
        ))
        .unwrap();
        assert!(matches(
            &anchor_only,
            &screen(r"C:\Projects\agentlife-evil")
        ));
    }

    #[test]
    fn match_report_names_a_pinned_line_that_missed() {
        let h = with_lines(r#"["C:\\Projects\\agentlife"]"#);
        let r = match_report(
            &h,
            "any",
            chrono::Utc::now(),
            "Do you trust the files here?\nC:\\Projects\\agentlife-evil",
        );
        assert_eq!(r.lines, vec![(r"C:\Projects\agentlife".to_string(), false)]);
        assert!(!r.matched);
    }

    #[test]
    fn refuses_a_pinned_line_that_is_short_or_spans_lines_or_names_a_secret() {
        for bad in [
            r#"["short"]"#,
            r#"["line one\nline two here"]"#,
            r#"["Enter your password here"]"#,
        ] {
            let json = format!(
                r#"{{"id":"t","match":{{"text_anchors":["Do you trust the files"],"lines":{bad}}},"action":"<Down><Enter>","scope":{{"roles":["*"]}},"provenance":{PROVENANCE}}}"#
            );
            assert!(parse_and_validate(&json).is_err(), "{bad}");
        }
    }

    // -- the folder-trust dialog: OverMind's capture on claude 2.1.301 (pyte, 2026-10-16) ---------- //

    const TRUST_EXAMPLE: &str = include_str!("../approval-example/trust-folder-agentlife.json");

    /// The dialog as captured: one leading space on every text line; the path on its own row, HARD-wrapped at the
    /// screen edge when it does not fit `cols` (the continuation row also has the one leading space).
    fn trust_dialog(cols: usize, path: &str) -> String {
        let mut rows = vec![
            "\u{2500}".repeat(cols),
            " Accessing workspace:".to_string(),
            String::new(),
        ];
        let mut rest = format!(" {path}");
        while rest.chars().count() > cols {
            let head: String = rest.chars().take(cols).collect();
            rows.push(head);
            rest = format!(" {}", rest.chars().skip(cols).collect::<String>());
        }
        rows.push(rest);
        rows.extend([
            String::new(),
            " Quick safety check: Is this a project you created or one you trust? (Like your own code, a well-known open source".into(),
            " project, or work from your team). If not, take a moment to review what's in this folder first.".into(),
            String::new(),
            " Claude Code'll be able to read, edit, and execute files here.".into(),
            String::new(),
            " Security guide".into(),
            String::new(),
            " \u{276f} No, exit".into(),
            "   Yes, I trust this folder".into(),
            String::new(),
            " Enter to confirm \u{b7} Esc to cancel".into(),
        ]);
        rows.join("\n")
    }

    #[test]
    fn the_trust_record_answers_its_own_folder_at_a_wide_screen_and_nothing_else() {
        let h = parse_and_validate(TRUST_EXAMPLE).expect("the trust record must pass validation");
        let ok = trust_dialog(120, r"C:\Projects\agentlife");
        assert!(
            matches(&h, &ok),
            "positive control: the exact folder at 120 columns"
        );
        assert_eq!(action_bytes(&h.action).unwrap(), b"\x1b[B\r");
        // A longer path, a path under it, and a different folder never match.
        for other in [
            r"C:\Projects\agentlife-evil",
            r"C:\Projects\agentlife\sub",
            r"C:\Projects\agentlife2",
            r"D:\Projects\agentlife",
            r"C:\Projects\other",
        ] {
            assert!(!matches(&h, &trust_dialog(120, other)), "{other}");
        }
        // The dialog's other lines are required too.
        assert!(!matches(
            &h,
            &ok.replace("Yes, I trust this folder", "Yes, I trust that folder")
        ));
        assert!(!matches(
            &h,
            &ok.replace(" Accessing workspace:", " Accessing files:")
        ));
    }

    #[test]
    fn a_path_that_hard_wraps_at_a_narrow_screen_fails_closed() {
        let h = parse_and_validate(TRUST_EXAMPLE).unwrap();
        // 80 columns is the host's fallback size: a 79-character path no longer fits with its leading space.
        let long = format!(r"C:\Projects\agentlife{}", "x".repeat(70));
        assert!(!matches(&h, &trust_dialog(80, &long)));
        // The record's own folder is short enough to match at 80 columns too.
        assert!(matches(&h, &trust_dialog(80, r"C:\Projects\agentlife")));
    }

    #[test]
    fn the_wrap_that_lands_exactly_on_the_trusted_path_needs_the_blank_row_pin() {
        // A path exactly (columns - 1) long, plus a suffix: row 1 IS the trusted path, row 2 is its continuation.
        let trusted = r"C:\Projects\agentlife";
        let cols = trusted.len() + 1; // " " + path fills the row
        let attacker = format!("{trusted}-evil");
        let screen = trust_dialog(cols, &attacker);
        assert!(
            screen.lines().any(|l| l.trim() == trusted),
            "the setup: the first wrapped row equals the trusted path"
        );
        // A plain line pin is fooled by that.
        let mut raw: RawHandlerSpec = serde_json::from_str(TRUST_EXAMPLE).unwrap();
        let path_pin = raw.match_spec.isolated_lines.remove(0);
        raw.match_spec.lines.push(path_pin);
        let with_plain = HandlerSpec::try_from(raw).unwrap();
        assert!(
            matches(&with_plain, &screen),
            "residual: a plain `lines` pin is fooled at the exact width"
        );
        // The isolated pin is not: the next row is the non-blank continuation.
        let isolated = parse_and_validate(TRUST_EXAMPLE).unwrap();
        assert!(!matches(&isolated, &screen));
        // And the same record still matches the honest dialog at that width.
        assert!(matches(&isolated, &trust_dialog(cols + 1, trusted)));
    }

    #[test]
    fn an_isolated_line_may_be_the_last_row() {
        let h = parse_and_validate(TRUST_EXAMPLE).unwrap();
        let mut screen = trust_dialog(120, r"C:\Projects\agentlife");
        // Cut the dialog after the path row (nothing follows).
        let cut = screen.find("\n\n Quick safety").unwrap();
        screen.truncate(cut);
        // The other pins are gone, so it must not match; drop them from the record to test only the isolation rule.
        assert!(!matches(&h, &screen));
        let mut raw: RawHandlerSpec = serde_json::from_str(TRUST_EXAMPLE).unwrap();
        raw.match_spec.lines.clear();
        raw.match_spec.text_anchors = vec!["Accessing workspace:".into()];
        let only = HandlerSpec::try_from(raw).unwrap();
        assert!(
            matches(&only, &screen),
            "the pinned row is last: nothing follows to continue it"
        );
    }

    #[test]
    fn match_report_names_an_isolated_pin_that_missed() {
        let h = parse_and_validate(TRUST_EXAMPLE).unwrap();
        let r = match_report(
            &h,
            "any",
            chrono::Utc::now(),
            &trust_dialog(120, r"C:\Projects\agentlife-evil"),
        );
        assert!(!r.matched);
        assert!(
            r.lines
                .iter()
                .any(|(l, ok)| l.starts_with("(then a blank row)") && !ok),
            "{:?}",
            r.lines
        );
    }

    #[test]
    fn refuses_an_isolated_line_that_is_short_or_spans_lines_or_names_a_secret() {
        for bad in [
            r#"["short"]"#,
            r#"["line one\nline two here"]"#,
            r#"["Enter your password here"]"#,
        ] {
            let json = format!(
                r#"{{"id":"t","match":{{"text_anchors":["Do you trust the files"],"isolated_lines":{bad}}},"action":"<Down><Enter>","scope":{{"roles":["*"]}},"provenance":{PROVENANCE}}}"#
            );
            assert!(parse_and_validate(&json).is_err(), "{bad}");
        }
    }

    #[test]
    fn action_bytes_maps_each_token_and_only_down_carries_an_escape() {
        assert_eq!(action_bytes("<Down><Enter>").unwrap(), b"\x1b[B\r");
        assert_eq!(action_bytes("<Down>\r").unwrap(), b"\x1b[B\r");
        assert_eq!(action_bytes("1\r").unwrap(), b"1\r");
        assert_eq!(action_bytes("y").unwrap(), b"y");
        assert_eq!(
            action_bytes("<Down><Down><Enter>").unwrap(),
            b"\x1b[B\x1b[B\r"
        );
        // Every accepted action's bytes hold an ESC only as part of `ESC [ B`.
        for a in ["<Down>", "<Down><Enter>", "<Down><Down><Down>", "1<Down>"] {
            let b = action_bytes(a).unwrap();
            let escs = b.iter().filter(|&&x| x == 0x1b).count();
            let downs = b.windows(3).filter(|w| *w == b"\x1b[B").count();
            assert_eq!(escs, downs, "{a}");
        }
        assert_eq!(action_bytes("\u{1b}[B"), None);
    }

    #[test]
    fn a_trust_dialog_handler_loads_with_down_enter() {
        let json = with(r#""action": "<Down><Enter>""#);
        let h = parse_and_validate(&json).unwrap();
        assert_eq!(
            parse_action(&h.action).unwrap(),
            vec![KeyToken::Down, KeyToken::Enter]
        );
    }

    #[test]
    fn refuses_a_handler_for_a_dialog_that_mentions_a_secret() {
        for anchor in [
            "Enter your password",
            "Paste your API key here",
            "Your PIN: please",
            "One-time code sent",
            "Authentication token required",
        ] {
            let json = format!(
                r#"{{
                    "id": "t",
                    "match": {{"text_anchors": ["{anchor}"], "fields": {{}}}},
                    "action": "1\r",
                    "scope": {{"roles": ["overmind"]}},
                    "provenance": {PROVENANCE}
                }}"#
            );
            let err = parse_and_validate(&json).unwrap_err();
            assert!(
                err.contains("secrets are never approvals"),
                "{anchor}: {err}"
            );
        }
    }

    #[test]
    fn secret_words_match_whole_words_only() {
        assert_eq!(secret_word_in("Enter PIN:"), Some("pin"));
        assert_eq!(secret_word_in("API-key"), Some("api key"));
        assert_eq!(secret_word_in("the pinned tab"), None);
        assert_eq!(secret_word_in("tokenizer settings"), None);
        assert_eq!(
            secret_word_in("I am using this for local development"),
            None
        );
    }

    #[test]
    fn refuses_an_empty_or_too_short_anchor_list() {
        let none = r#"{"id":"t","match":{"text_anchors":[],"fields":{}},"action":"1\r","scope":{"roles":["*"]},"provenance":PROV}"#
            .replace("PROV", PROVENANCE);
        assert!(parse_and_validate(&none).unwrap_err().contains("empty"));
        let short = none.replace(r#""text_anchors":[]"#, r#""text_anchors":["1. Yes"]"#);
        assert!(parse_and_validate(&short).unwrap_err().contains("shorter"));
    }

    #[test]
    fn refuses_an_id_that_is_not_a_safe_file_stem() {
        let json = sample_json("../escape", None);
        assert!(parse_and_validate(&json).unwrap_err().contains("id"));
    }

    #[test]
    fn refuses_empty_provenance_approvals() {
        let json = sample_json("t", None).replace(
            r#""approvals": [{"said_on": "2026-09-18", "quote": "Proceed!", "approving": "a test"}]"#,
            r#""approvals": []"#,
        );
        assert!(parse_and_validate(&json).unwrap_err().contains("approvals"));
    }

    #[test]
    fn a_screen_mentioning_a_secret_is_never_matched() {
        let h = parse_and_validate(&sample_json("t", None)).unwrap();
        let screen = "SECURITY CONFIRMATION\nI am using this for local development\n";
        assert!(matches(&h, screen), "positive control");
        let with_secret = format!("{screen}Password: ");
        assert!(!matches(&h, &with_secret));
        let report = match_report(&h, "overmind", chrono::Utc::now(), &with_secret);
        assert_eq!(report.secret_word, Some("password"));
        assert!(!report.matched);
    }

    #[test]
    fn match_report_pinpoints_which_anchor_and_field_failed() {
        // PM finding, 2026-09-19: a real restart left no way to tell WHICH
        // anchor or field caused a non-match - this is the diagnostic
        // host.rs logs, so it must actually pinpoint the failure, not just
        // restate the overall bool `matches()` already gives.
        let h = parse_and_validate(&sample_json("t", Some("server:claude-peers"))).unwrap();
        let screen = "  I am using this for local development\n  Channels: server:claude-peers\n";
        let report = match_report(&h, "overmind", chrono::Utc::now(), screen);
        assert!(!report.matched);
        assert!(report.active);
        assert_eq!(
            report.anchors,
            vec![
                ("SECURITY CONFIRMATION".to_string(), false),
                ("local development".to_string(), true),
            ]
        );
        assert_eq!(
            report.fields,
            vec![(
                "Channels".to_string(),
                "server:claude-peers".to_string(),
                true
            )]
        );
    }

    #[test]
    fn matches_when_every_anchor_and_field_is_present_exactly() {
        let h = parse_and_validate(&sample_json("t", Some("server:claude-peers"))).unwrap();
        let screen = "  SECURITY CONFIRMATION\n  I am using this for local development\n  Channels: server:claude-peers\n";
        assert!(matches(&h, screen));
    }

    #[test]
    fn does_not_match_on_a_missing_anchor() {
        let h = parse_and_validate(&sample_json("t", Some("server:claude-peers"))).unwrap();
        let screen = "  I am using this for local development\n  Channels: server:claude-peers\n";
        assert!(!matches(&h, screen));
    }

    #[test]
    fn does_not_match_on_an_extra_or_different_field_value() {
        // ⚠️ THE PREFIX-MATCH TRAP §12.4 warns about: "server:claude-peers"
        // plus anything else must NOT match a handler pinned to exactly
        // "server:claude-peers".
        let h = parse_and_validate(&sample_json("t", Some("server:claude-peers"))).unwrap();
        let screen = "  SECURITY CONFIRMATION\n  I am using this for local development\n  Channels: server:claude-peers,server:extra\n";
        assert!(!matches(&h, screen));
    }

    #[test]
    fn is_active_respects_role_scope() {
        let h = parse_and_validate(&sample_json("t", None)).unwrap();
        let now = chrono::Utc::now();
        assert!(is_active(&h, "overmind", now));
        assert!(!is_active(&h, "synapse", now));
    }

    #[test]
    fn is_active_treats_an_expired_handler_as_not_present() {
        let json = with(r#""action": "1\r", "expires_at": "2020-01-01T00:00:00Z""#);
        let h = parse_and_validate(&json).unwrap();
        assert!(!is_active(&h, "overmind", chrono::Utc::now()));
    }

    // -- the inert example: the same bytes the seed PR puts in the user's repo //

    /// ⚠️ Read ONLY here, under `cfg(test)`. The binary never contains it.
    const EXAMPLE_JSON: &str = include_str!("../approval-example/claude-peers-dev-channels.json");

    fn example_handler() -> HandlerSpec {
        parse_and_validate(EXAMPLE_JSON).expect("the example must pass the same validation")
    }

    /// The real dialog, verbatim from `.lane-state/unhandled-prompts/
    /// 2026-09-27T14-58-37.644790300+00-00.txt` - the Unpopped restart that
    /// reached CireSnave's screen unasked (trailing blank rows trimmed).
    pub(super) const REAL_DIALOG_SCREEN: &str = "\
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────\n\
  WARNING: Loading development channels\n\
\n\
  --dangerously-load-development-channels is for local channel development only. Do not use this option to run\n\
  channels you have downloaded off the internet.\n\
\n\
  Please use --channels to run a list of approved channels.\n\
\n\
  Channels: server:claude-peers\n\
\n\
  ❯ 1. I am using this for local development\n\
    2. Exit\n\
\n\
  Enter to confirm · Esc to cancel\n";

    #[test]
    fn example_matches_the_real_dialog_and_selects_option_1() {
        let h = example_handler();
        assert!(matches(&h, REAL_DIALOG_SCREEN));
        assert_eq!(
            h.action, "1\r",
            "no trailing \\n - CireSnave's own correction"
        );
    }

    #[test]
    fn example_does_not_match_an_extra_channel() {
        let h = example_handler();
        let screen = REAL_DIALOG_SCREEN.replace(
            "Channels: server:claude-peers\n",
            "Channels: server:claude-peers,server:extra\n",
        );
        assert!(!matches(&h, &screen));
    }

    #[test]
    fn example_applies_to_every_role_until_its_approved_expiry() {
        // "Yes on all fronts. Proceed." (2026-09-27) approved seeding it for
        // ALL lanes - scope "*". Expiry is the 2026-09-19 approval's, unchanged.
        let h = example_handler();
        assert_eq!(h.scope.roles, vec!["*".to_string()]);
        let before = "2026-09-27T00:00:00Z".parse().unwrap();
        assert!(is_active(&h, "overmind", before));
        assert!(is_active(&h, "some-new-lane", before));
        let after = "2027-03-19T00:00:01Z".parse().unwrap();
        assert!(!is_active(&h, "overmind", after));
    }

    #[test]
    fn example_quotes_both_approvals_verbatim() {
        let quotes: Vec<String> = example_handler()
            .provenance
            .approvals
            .iter()
            .map(|a| a.quote.clone())
            .collect();
        assert_eq!(
            quotes,
            vec![
                "I like that.  Proceed.".to_string(),
                "Yes on all fronts.  Proceed.".to_string()
            ]
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

    // -- the dev-channels dialog for every channel set a lane can launch with ---------------------
    // 2026-10-22: every lane is about to move from server:claude-peers to server:synapse; an approval
    // record whose Channels value does not equal the dialog's leaves that lane waiting for CireSnave.

    /// Real capture, verbatim from `.lane-state/unhandled-prompts/2026-10-09T17-48-16...txt`
    /// (a lane launched with both servers; trailing blank rows trimmed).
    pub(super) const TWO_SERVER_SCREEN: &str = "\
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────\n\
  WARNING: Loading development channels\n\
\n\
  --dangerously-load-development-channels is for local channel development only. Do not use this option to run\n\
  channels you have downloaded off the internet.\n\
\n\
  Please use --channels to run a list of approved channels.\n\
\n\
  Channels: server:claude-peers, server:synapse\n\
\n\
  ❯ 1. I am using this for local development\n\
    2. Exit\n\
\n\
  Enter to confirm · Esc to cancel\n";

    /// The example record with a different `Channels` value (the shape CireSnave's repo uses for
    /// `synapse-dev-channel-pilot.json` and `claude-peers-synapse-dev-channels-pilot.json`).
    fn record_for_channels(channels: &str) -> HandlerSpec {
        let old = "\"Channels\": \"server:claude-peers\"";
        assert_eq!(EXAMPLE_JSON.matches(old).count(), 1);
        let json = EXAMPLE_JSON.replace(old, &format!("\"Channels\": \"{channels}\""));
        parse_and_validate(&json).expect("the record shape must pass validation")
    }

    #[test]
    fn a_record_matches_exactly_the_channel_set_it_names() {
        // (record's Channels, screen's Channels, must match)
        let cases = [
            ("server:claude-peers", "server:claude-peers", true),
            ("server:synapse", "server:synapse", true),
            (
                "server:claude-peers, server:synapse",
                "server:claude-peers, server:synapse",
                true,
            ),
            ("server:synapse", "server:claude-peers", false),
            (
                "server:claude-peers",
                "server:claude-peers, server:synapse",
                false,
            ),
            (
                "server:synapse",
                "server:claude-peers, server:synapse",
                false,
            ),
            (
                "server:claude-peers, server:synapse",
                "server:synapse",
                false,
            ),
            ("server:synapse", "server:synapse,server:extra", false),
        ];
        for (record, screen, expect) in cases {
            // The screens are built from the real captures by replacing only the Channels value.
            let text = TWO_SERVER_SCREEN.replace("server:claude-peers, server:synapse", screen);
            assert_eq!(
                matches(&record_for_channels(record), &text),
                expect,
                "record {record:?} vs screen {screen:?}"
            );
        }
        // And the two real captures themselves.
        assert!(matches(
            &record_for_channels("server:claude-peers"),
            REAL_DIALOG_SCREEN
        ));
        assert!(matches(
            &record_for_channels("server:claude-peers, server:synapse"),
            TWO_SERVER_SCREEN
        ));
    }

    #[test]
    fn an_unrecognised_or_secret_asking_dialog_is_never_answered() {
        let h = record_for_channels("server:synapse");
        let screen =
            TWO_SERVER_SCREEN.replace("server:claude-peers, server:synapse", "server:synapse");
        assert!(
            matches(&h, &screen),
            "control: the same record does answer the real dialog"
        );
        // Changed wording: an anchor is gone.
        assert!(!matches(
            &h,
            &screen.replace(
                "WARNING: Loading development channels",
                "NOTICE: Loading channels"
            )
        ));
        assert!(!matches(
            &h,
            &screen.replace("I am using this for local development", "Continue")
        ));
        // A dialog that also asks for a secret is refused even though every anchor is present.
        assert!(!matches(&h, &format!("{screen}\n  Enter your password:\n")));
        // Something that is not this dialog at all.
        assert!(!matches(
            &h,
            "Do you want to run this command? 1. Yes 2. No"
        ));
    }

    // -- folder-trust PREFIX approval (board 244) ------------------------------------------------------

    use crate::folder_trust::staged::StagedFs;

    /// The record CireSnave's repo gets (draft in `approval-example/`).
    const PREFIX_JSON: &str = include_str!("../approval-example/trust-folder-prefix.json");

    fn prefix_handler() -> HandlerSpec {
        parse_and_validate(PREFIX_JSON).expect("the folder-prefix record must pass validation")
    }

    /// The real dialog, shaped exactly like `.lane-state/unhandled-prompts/2026-10-22T00-29-28...txt`
    /// (configurant) and `2026-10-21T23-09-02...txt` (antspawn), with `path` substituted.
    fn trust_screen(path: &str) -> String {
        format!(
            "\
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────\n\
\n\n\n Accessing workspace:\n\
\n {path}\n\
\n Quick safety check: Is this a project you created or one you trust? (Like your own code, a well-known open source\n \
project, or work from your team). If not, take a moment to review what's in this folder first.\n\
\n Claude Code'll be able to read, edit, and execute files here.\n\
\n Security guide\n\
\n ❯ No, exit\n   Yes, I trust this folder\n\
\n Enter to confirm · Esc to cancel\n"
        )
    }

    fn fs() -> StagedFs {
        StagedFs::with(&[
            r"C:\Projects\configurant",
            r"C:\Projects\antspawn",
            r"C:\Projects\a\deep\er",
            r"C:\Projects-evil\x",
            r"C:\ProjectsX\y",
        ])
        .alias(r"C:\Projects\evil", r"D:\secrets")
        .alias(r"C:\Projects\evil2", r"C:\Windows\System32")
        .alias(r"C:\Projects\LONGNA~1", r"C:\Projects\longname-real")
    }

    fn answers(screen: &str) -> Option<String> {
        let hs = [prefix_handler()];
        find_matching_handler_resolved(&hs, "any-lane", chrono::Utc::now(), screen, &fs())
            .map(|(h, folder)| format!("{}:{}", h.id, folder.unwrap_or_default()))
    }

    #[test]
    fn the_prefix_record_answers_the_real_new_lane_dialogs_and_reports_the_resolved_folder() {
        assert_eq!(
            answers(&trust_screen(r"C:\Projects\configurant")).as_deref(),
            Some(r"trust-folder-prefix:C:\Projects\configurant")
        );
        assert_eq!(
            answers(&trust_screen(r"C:\Projects\antspawn")).as_deref(),
            Some(r"trust-folder-prefix:C:\Projects\antspawn")
        );
        assert!(answers(&trust_screen(r"C:\Projects\a\deep\er")).is_some());
        assert_eq!(prefix_handler().action, "<Down><Enter>");
    }

    #[test]
    fn every_way_out_of_the_prefix_is_refused() {
        // Control first: the same harness answers a good folder, so the refusals below are real.
        assert!(answers(&trust_screen(r"C:\Projects\configurant")).is_some());
        for bad in [
            r"C:\Projects\..\Windows", // `..` escape
            r"C:\Projects\configurant\..\..\Windows",
            r"C:\Projects\evil",                    // junction to another drive
            r"C:\Projects\evil2",                   // junction into C:\Windows
            r"C:\Projects\LONGNA~1",                // 8.3 short name
            r"\\server\share\Projects\configurant", // UNC
            r"\\?\C:\Projects\configurant",         // verbatim path
            r"C:\Projects-evil\x",                  // prefix confusion
            r"C:\ProjectsX\y",
            r"C:\Projects",             // the prefix itself
            r"C:\Projects\ghost",       // not on disk
            r"D:\Projects\configurant", // another drive
            r"C:/Projects/configurant", // wrong separators
        ] {
            assert_eq!(answers(&trust_screen(bad)), None, "{bad}");
        }
        // A path the terminal wrapped onto a second row is never trusted.
        let wrapped = trust_screen(r"C:\Projects\configurant")
            .replace(r"C:\Projects\configurant", "C:\\Projects\\configu\n rant");
        assert_eq!(answers(&wrapped), None);
    }

    #[test]
    fn it_answers_the_trust_dialog_and_nothing_else() {
        let good = trust_screen(r"C:\Projects\configurant");
        assert!(answers(&good).is_some());
        // The development-channels dialogs (real captures) are never answered by it.
        for other in [REAL_DIALOG_SCREEN, TWO_SERVER_SCREEN] {
            assert_eq!(answers(other), None);
        }
        // A dialog that also asks for a secret is never answered, anchors or not.
        assert_eq!(answers(&format!("{good}\n Enter your password:\n")), None);
        assert_eq!(
            answers(&good.replace("Quick safety check", "Quick check")),
            None
        );
        assert_eq!(
            answers(&good.replace("Yes, I trust this folder", "Yes, trust it")),
            None
        );
        assert_eq!(
            answers(&good.replace("Accessing workspace:", "Opening workspace:")),
            None
        );
        // Something else that merely names a folder.
        assert_eq!(
            answers("Delete C:\\Projects\\configurant? 1. Yes 2. No"),
            None
        );
    }

    #[test]
    fn an_expired_prefix_record_answers_nothing() {
        let h = prefix_handler();
        let expiry = h.expires_at.expect("the record carries an expiry");
        let hs = [h];
        let screen = trust_screen(r"C:\Projects\configurant");
        let before = expiry - chrono::Duration::seconds(1);
        let after = expiry + chrono::Duration::seconds(1);
        assert!(find_matching_handler_resolved(&hs, "x", before, &screen, &fs()).is_some());
        assert!(find_matching_handler_resolved(&hs, "x", after, &screen, &fs()).is_none());
    }

    #[test]
    fn a_folder_prefix_record_that_could_answer_anything_else_is_refused_at_load() {
        let prefix_line = r#""folder_prefix": "C:\\Projects""#;
        assert_eq!(PREFIX_JSON.matches(prefix_line).count(), 1);
        let with_prefix =
            |p: &str| PREFIX_JSON.replace(prefix_line, &format!(r#""folder_prefix": "{p}""#));
        for bad in [
            r"\\\\server\\share",
            r"C:\\Projects\\..\\Windows",
            r"C:\\Projects\\",
            r"C:\\",
            r"C:/Projects",
            r"Projects",
            r"C:\\PROGRA~1",
            r"\\\\?\\C:\\Projects",
        ] {
            assert!(parse_and_validate(&with_prefix(bad)).is_err(), "{bad}");
        }
        assert!(
            parse_and_validate(&with_prefix(r"C:\\Projects")).is_ok(),
            "control"
        );
        assert!(parse_and_validate(&with_prefix(r"D:\\Work\\Repos")).is_ok());
        // Any other action (it could answer a different dialog), or a missing pin on the trust dialog.
        let mutate = |from: &str, to: &str| {
            assert_eq!(PREFIX_JSON.matches(from).count(), 1, "{from}");
            parse_and_validate(&PREFIX_JSON.replace(from, to))
        };
        assert!(mutate(r#""action": "<Down><Enter>""#, r#""action": "1\r""#).is_err());
        assert!(mutate(r#""action": "<Down><Enter>""#, r#""action": "<Enter>""#).is_err());
        assert!(mutate(r#""Quick safety check""#, r#""Some other dialog""#).is_err());
        assert!(mutate(r#""Yes, I trust this folder""#, r#""Yes, I do""#).is_err());
        assert!(mutate(r#""Accessing workspace:""#, r#""Opening workspace:""#).is_err());
        assert!(mutate(
            r#""lines": ["#,
            r#""fields": {"Channels": "server:synapse"}, "lines": ["#
        )
        .is_err());
    }

    #[test]
    fn a_record_without_folder_prefix_still_pins_one_folder_exactly() {
        // The per-lane records keep working unchanged: a different folder is not answered.
        let h = parse_and_validate(TRUST_EXAMPLE).unwrap();
        assert!(h.match_spec.folder_prefix.is_none());
        assert!(matches(&h, &trust_screen(r"C:\Projects\agentlife")));
        assert!(!matches(&h, &trust_screen(r"C:\Projects\agentlife-evil")));
        assert!(!matches(&h, &trust_screen(r"C:\Projects\configurant")));
    }
}

#[cfg(test)]
mod real_records;
