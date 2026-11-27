// SPDX-License-Identifier: MIT OR Apache-2.0

//! O19(a): `lane-restart start` logs the roster epoch beside the lane home's cached epoch and warns
//! when they differ (PM finding lane-synapse-channel-closed, 2026-11-22). A `synapse list --role <lane>`
//! from another home claims that role, so the roster epoch runs ahead of the epoch the lane's own home
//! cached in `sessions/<role>.json`, and the lane's channel then exits superseded with its mail queued.
//!
//! Read-only on both sides: the cached epoch is read from the file (its token is never read into a
//! message), and the roster is read with `synapse list` under the LANE'S OWN home and no `--role`, so
//! this check cannot itself claim anything.

use serde_json::Value;
use std::path::Path;

/// The `epoch` in `<home>/sessions/<role>.json`. `Ok(None)` when the file is absent (never joined from
/// this home); `Err` when it exists but cannot be read as an epoch. The token is not touched.
pub fn cached_epoch(home: &Path, role: &str) -> Result<Option<u64>, String> {
    let path = home.join("sessions").join(format!("{role}.json"));
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let v: Value =
        serde_json::from_str(&text).map_err(|_| format!("{} is not valid JSON", path.display()))?;
    v.get("epoch")
        .and_then(Value::as_u64)
        .map(Some)
        .ok_or_else(|| format!("{} has no numeric epoch", path.display()))
}

/// The line `start` prints and logs, and whether it is a warning: the two epochs side by side, and a
/// warning when the roster is ahead of (or merely different from) the cache.
pub fn note(role: &str, cached: Option<u64>, roster: Option<u64>) -> (String, bool) {
    let show = |e: Option<u64>| e.map_or_else(|| "none".to_string(), |n| n.to_string());
    let line = format!(
        "synapse epoch for {role}: roster {} cached {}",
        show(roster),
        show(cached)
    );
    match (cached, roster) {
        (Some(c), Some(r)) if c == r => (line, false),
        (Some(c), Some(r)) => (
            format!(
                "{line}; WARNING the roster is {} the cache, so another holder may have claimed {role} \
                 and this lane's channel can exit superseded with mail queued",
                if r > c { "ahead of" } else { "behind" }
            ),
            true,
        ),
        _ => (line, false),
    }
}

/// O22(a): the online flag of `role` in `synapse list` output. `Ok(None)`: the role is not listed. `Err`: nothing parsed.
pub fn online_from_list(out: &str, role: &str) -> Result<Option<bool>, String> {
    let mut parsed = 0usize;
    let mut found = None;
    for line in out.lines().map(str::trim).filter(|l| l.starts_with('{')) {
        let v: Value = serde_json::from_str(line)
            .map_err(|e| format!("a roster row does not parse ({e}): {line:?}"))?;
        let id = v
            .get("global_id")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("a roster row has no global_id: {line:?}"))?;
        parsed += 1;
        if id.split_once('@').map_or(id, |(s, _)| s) == role {
            found = v.get("online").and_then(Value::as_bool);
        }
    }
    if parsed == 0 {
        Err("the roster answer held no role lines".to_string())
    } else {
        Ok(found)
    }
}

/// O22(a) (PM ruling 2026-11-24): a launcher start may claim the role from the lane home only when the cached epoch
/// is BEHIND the roster, the role is OFFLINE (a held role is never claimed over) and the answer was read. Anything
/// unread (`None`) is no claim.
pub fn should_reclaim(
    cached: Option<u64>,
    roster: Option<u64>,
    online: Option<bool>,
    launcher: bool,
) -> bool {
    launcher && online == Some(false) && matches!((cached, roster), (Some(c), Some(r)) if c < r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn online_is_read_for_the_role_and_an_unlisted_role_is_none() {
        let out = "{\"epoch\":7,\"global_id\":\"a@x\",\"online\":false}
{\"epoch\":2,\"global_id\":\"b@x\",\"online\":true}
";
        assert_eq!(online_from_list(out, "a"), Ok(Some(false)));
        assert_eq!(online_from_list(out, "b"), Ok(Some(true)));
        assert_eq!(online_from_list(out, "c"), Ok(None));
        assert!(online_from_list("nothing", "a").is_err());
    }

    #[test]
    fn only_a_launcher_start_with_the_cache_behind_and_the_role_offline_reclaims() {
        assert!(should_reclaim(Some(6), Some(7), Some(false), true));
        assert!(
            !should_reclaim(Some(6), Some(7), Some(true), true),
            "an online role is never claimed over"
        );
        assert!(
            !should_reclaim(Some(6), Some(7), Some(false), false),
            "not a launcher start"
        );
        assert!(
            !should_reclaim(Some(7), Some(7), Some(false), true),
            "equal"
        );
        assert!(
            !should_reclaim(Some(8), Some(7), Some(false), true),
            "cache ahead"
        );
        assert!(
            !should_reclaim(None, Some(7), Some(false), true),
            "no cache is not behind"
        );
        assert!(
            !should_reclaim(Some(6), Some(7), None, true),
            "online unread"
        );
        assert!(
            !should_reclaim(Some(6), None, Some(false), true),
            "roster unread"
        );
    }

    #[test]
    fn a_roster_epoch_ahead_of_the_cache_logs_both_and_warns() {
        let (line, warn) = note("analysant", Some(6), Some(7));
        assert!(warn);
        assert!(line.contains("roster 7 cached 6"), "{line}");
        assert!(line.contains("WARNING"), "{line}");
        assert!(line.contains("ahead of"), "{line}");
    }

    #[test]
    fn equal_epochs_log_without_a_warning_and_a_missing_side_does_not_warn() {
        let (line, warn) = note("x", Some(3), Some(3));
        assert!(!warn && line.contains("roster 3 cached 3"), "{line}");
        let (line, warn) = note("x", None, Some(3));
        assert!(!warn && line.contains("cached none"), "{line}");
        let (_, warn) = note("x", Some(3), None);
        assert!(!warn);
    }

    #[test]
    fn the_cached_epoch_is_read_from_the_session_file_and_never_its_token() {
        let d = std::env::temp_dir().join(format!("epochchk-{}", std::process::id()));
        let s = d.join("sessions");
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(
            s.join("r.json"),
            r#"{"instance_id":"i","token":"SECRET","global_id":"r@a","epoch":8}"#,
        )
        .unwrap();
        assert_eq!(cached_epoch(&d, "r"), Ok(Some(8)));
        assert_eq!(cached_epoch(&d, "absent"), Ok(None));
        std::fs::write(s.join("bad.json"), r#"{"token":"SECRET"}"#).unwrap();
        let e = cached_epoch(&d, "bad").unwrap_err();
        assert!(!e.contains("SECRET"), "{e}");
        std::fs::remove_dir_all(&d).ok();
    }
}
