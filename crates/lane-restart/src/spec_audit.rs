// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart specs audit`: one row per recorded role saying whether a message sent over
//! Synapse can reach it.
//!
//! The 2026-11-04 audit of `.lane-state/*.launch.json` found 8 Synapse-only, 29 both and 13
//! claude-peers-only lanes (the standing order of 2026-10-31 was Synapse-only). A lane launched
//! without `server:synapse`, or without an identity to load it with, cannot receive Synapse mail:
//! a send is queued and nobody reads it, so lanes sat idle 6-8 hours. This reads what a launch
//! WOULD load (the recorded spec, `<role>.mcp.json`, the identity home that file names, the
//! certificate in it) and what the daemon says (presence, queued mail). It writes nothing.
//!
//! The certificate's expiry is read from its binary body (`synapse/agent-cert/v1`): after the
//! magic, a `u32` version and a `u8`, the length-prefixed fields id, hex id, role, global id, two
//! keys, then issued-at and expires-at as 8-byte big-endian microseconds since the epoch. A shape
//! this does not recognise is reported as unreadable, never as valid or as expired.

use crate::spec;
use crate::spec_channels::Channels;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A certificate with less than this left is flagged. The agentlife and pm certificates of
/// 2026-11-04 were valid 24 h (`synapse id provision --days` allows 1 to 30), so a day-scale
/// threshold would flag every lane.
pub const NEAR_EXPIRY_SECS: i64 = 6 * 3600;

/// Queued mail older than this, for a role that is not online, is flagged as stuck.
pub const STUCK_MAIL_SECS: i64 = 30 * 60;

/// What the daemon says about one role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Presence {
    pub online: bool,
    pub queued: u64,
    pub oldest_enqueued_at: Option<DateTime<Utc>>,
}

/// The daemon's view of every role, or why it could not be read.
pub type PresenceMap = Result<BTreeMap<String, Presence>, String>;

/// The state of a role's certificate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Cert {
    /// Valid until this time.
    Valid(DateTime<Utc>),
    /// Expired at this time.
    Expired(DateTime<Utc>),
    /// No `cert.pem` in the role's identity directory.
    Missing,
    /// There is a file but its shape is not the one described above.
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditRow {
    pub role: String,
    /// `None`: the spec could not be read (see `spec_error`).
    pub channels: Option<Channels>,
    pub spec_error: Option<String>,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub state: Option<String>,
    /// `Ok`, or why `<role>.mcp.json` cannot load a synapse server.
    pub mcp: Result<(), String>,
    /// The identity home the mcp file names; `None` when it names none.
    pub home: Option<String>,
    pub home_present: bool,
    pub cert: Cert,
    /// `None`: the daemon could not be read, or does not know the role.
    pub presence: Option<Presence>,
    /// The daemon was readable and has never heard of this role (it never claimed its identity).
    pub unknown_to_daemon: bool,
}

/// How bad a finding is: `Fail` makes the command exit nonzero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Fail,
    Warn,
}

/// Everything wrong with one row.
pub fn findings(row: &AuditRow, now: DateTime<Utc>) -> Vec<(Severity, String)> {
    use Severity::{Fail, Warn};
    let mut f = Vec::new();
    match (&row.channels, &row.spec_error) {
        (Some(c), _) => {
            if c.peers && !c.synapse {
                f.push((
                    Fail,
                    "peers-only: the launch loads no server:synapse".to_string(),
                ));
            } else if !c.synapse {
                f.push((Fail, "the launch loads no channel at all".to_string()));
            } else if !c.mcp_config {
                f.push((
                    Fail,
                    "server:synapse without --mcp-config is inert".to_string(),
                ));
            }
        }
        (None, Some(e)) => f.push((Fail, format!("no usable launch spec: {e}"))),
        (None, None) => {}
    }
    if let Err(why) = &row.mcp {
        f.push((Fail, format!("mcp.json: {why}")));
    }
    if row.mcp.is_ok() && !row.home_present {
        f.push((Fail, "identity home missing".to_string()));
    }
    match &row.cert {
        Cert::Missing if row.home_present => f.push((Fail, "no certificate".to_string())),
        Cert::Unreadable => f.push((Warn, "certificate shape not recognised".to_string())),
        Cert::Expired(t) => f.push((Fail, format!("certificate expired {}", t.to_rfc3339()))),
        Cert::Valid(t) if (*t - now).num_seconds() < NEAR_EXPIRY_SECS => {
            f.push((Warn, format!("certificate expires {}", t.to_rfc3339())))
        }
        _ => {}
    }
    if let Some(p) = &row.presence {
        if let (false, Some(t)) = (p.online, p.oldest_enqueued_at) {
            let age = (now - t).num_seconds();
            if p.queued > 0 && age >= STUCK_MAIL_SECS {
                f.push((
                    Warn,
                    format!(
                        "{} message(s) queued, oldest {}, role offline",
                        p.queued,
                        age_text(age)
                    ),
                ));
            }
        }
    }
    if row.unknown_to_daemon {
        f.push((
            Warn,
            "the daemon does not know this role (never claimed)".to_string(),
        ));
    }
    if row
        .model
        .as_deref()
        .is_some_and(|m| m.to_ascii_lowercase().contains("opus"))
    {
        f.push((Warn, "model is Opus (a start replaces it)".to_string()));
    }
    f
}

fn age_text(secs: i64) -> String {
    match secs {
        s if s < 3600 => format!("{} min", s / 60),
        s if s < 48 * 3600 => format!("{} h", s / 3600),
        s => format!("{} d", s / 86400),
    }
}

/// Every role with a recorded launch spec, sorted.
pub fn roles(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.strip_suffix(spec::FILE_SUFFIX).map(str::to_string)
        })
        .filter(|r| crate::relaunch::valid_identifier(r))
        .collect();
    v.sort();
    v
}

/// The `--home` (or `SYNAPSE_HOME`) a role's mcp file gives its synapse server.
fn mcp_home(v: &Value) -> Option<String> {
    let s = v.get("mcpServers")?.get("synapse")?;
    let from_args = s.get("args").and_then(Value::as_array).and_then(|a| {
        let i = a.iter().position(|x| x.as_str() == Some("--home"))?;
        a.get(i + 1)?.as_str().map(str::to_string)
    });
    from_args.or_else(|| {
        s.get("env")?
            .get("SYNAPSE_HOME")?
            .as_str()
            .map(str::to_string)
    })
}

/// `(mcp result, home)` for `<dir>/<role>.mcp.json`.
fn read_mcp(dir: &Path, role: &str) -> (Result<(), String>, Option<String>) {
    let path = dir.join(format!("{role}.mcp.json"));
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (Err("no mcp.json".to_string()), None)
        }
        Err(e) => return (Err(format!("unreadable: {e}")), None),
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return (Err("not valid JSON".to_string()), None);
    };
    if v.get("mcpServers").and_then(|m| m.get("synapse")).is_none() {
        return (Err("no mcpServers.synapse".to_string()), None);
    }
    (Ok(()), mcp_home(&v))
}

/// The expiry in a certificate's PEM text, or `None` when the shape is not the known one.
pub fn cert_expiry(pem: &str, role: &str) -> Option<DateTime<Utc>> {
    use std::io::Read;
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    let b = b64(&body)?;
    let mut r = std::io::Cursor::new(b);
    fn lp(r: &mut std::io::Cursor<Vec<u8>>) -> Option<Vec<u8>> {
        let mut n = [0u8; 4];
        r.read_exact(&mut n).ok()?;
        let n = u32::from_be_bytes(n) as usize;
        if n > 4096 {
            return None;
        }
        let mut v = vec![0u8; n];
        r.read_exact(&mut v).ok()?;
        Some(v)
    }
    if lp(&mut r)? != b"synapse/agent-cert/v1" {
        return None;
    }
    let mut skip = [0u8; 5]; // u32 version + u8
    r.read_exact(&mut skip).ok()?;
    lp(&mut r)?; // id
    lp(&mut r)?; // hex id
    if lp(&mut r)? != role.as_bytes() {
        return None;
    }
    lp(&mut r)?; // global id
    lp(&mut r)?; // key
    lp(&mut r)?; // key
    let _issued = lp(&mut r)?;
    let exp = lp(&mut r)?;
    let micros = i64::from_be_bytes(exp.try_into().ok()?);
    DateTime::from_timestamp_micros(micros)
}

/// Standard base64 (padding optional), no dependency for one use.
fn b64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=') {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn cert_state(home: &Path, role: &str, now: DateTime<Utc>) -> Cert {
    let p = home.join("roles").join(role).join("cert.pem");
    let Ok(text) = std::fs::read_to_string(&p) else {
        return Cert::Missing;
    };
    match cert_expiry(&text, role) {
        Some(t) if t > now => Cert::Valid(t),
        Some(t) => Cert::Expired(t),
        None => Cert::Unreadable,
    }
}

/// One row for `role`.
pub fn audit_role(dir: &Path, role: &str, presence: &PresenceMap, now: DateTime<Utc>) -> AuditRow {
    let (channels, spec_error, model, permission_mode, state) = match spec::read(dir, role) {
        Ok(s) => (
            Some(Channels::of(&s.launch_args)),
            None,
            Some(s.model.clone()),
            s.permission_mode.map(|m| format!("{m:?}")),
            Some(format!("{:?}", s.state)),
        ),
        Err(e) => (None, Some(e.to_string()), None, None, None),
    };
    let (mcp, home) = read_mcp(dir, role);
    let home_path = home.as_ref().map(PathBuf::from);
    let home_present = home_path
        .as_ref()
        .is_some_and(|h| h.join("roles").join(role).is_dir());
    let cert = match &home_path {
        Some(h) => cert_state(h, role, now),
        None => Cert::Missing,
    };
    AuditRow {
        role: role.to_string(),
        channels,
        spec_error,
        model,
        permission_mode,
        state,
        mcp,
        home,
        home_present,
        cert,
        presence: presence.as_ref().ok().and_then(|m| m.get(role)).cloned(),
        unknown_to_daemon: presence.as_ref().is_ok_and(|m| !m.contains_key(role)),
    }
}

/// The rows for `roles` (every recorded role when empty).
pub fn audit(
    dir: &Path,
    only: &[String],
    presence: &PresenceMap,
    now: DateTime<Utc>,
) -> Vec<AuditRow> {
    let all = if only.is_empty() {
        roles(dir)
    } else {
        only.to_vec()
    };
    all.iter()
        .map(|r| audit_role(dir, r, presence, now))
        .collect()
}

/// Parses `synapse list --role X` output (one JSON object per line).
pub fn parse_presence(text: &str) -> PresenceMap {
    let mut m = BTreeMap::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value =
            serde_json::from_str(line).map_err(|e| format!("unparseable list line: {e}"))?;
        let gid = v
            .get("global_id")
            .and_then(Value::as_str)
            .ok_or("a list line has no global_id")?;
        let role = gid.split('@').next().unwrap_or(gid).to_string();
        let pending = v.get("pending");
        m.insert(
            role,
            Presence {
                online: v.get("online").and_then(Value::as_bool).unwrap_or(false),
                queued: pending
                    .and_then(|p| p.get("queued"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                oldest_enqueued_at: pending
                    .and_then(|p| p.get("oldest_enqueued_at"))
                    .and_then(Value::as_str)
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|t| t.with_timezone(&Utc)),
            },
        );
    }
    Ok(m)
}

/// Asks the real daemon, via the `synapse` CLI acting as an existing role in `home` (a role with no
/// identity directory is never used: acting as one would create its keys). Killed after 10 s.
pub fn real_presence(home: &Path) -> PresenceMap {
    let roles = home.join("roles");
    let acting = if roles.join("pm").is_dir() {
        "pm".to_string()
    } else {
        std::fs::read_dir(&roles)
            .map_err(|e| format!("cannot read {}: {e}", roles.display()))?
            .flatten()
            .find(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .ok_or("the identity home has no role to ask as")?
    };
    let mut child = std::process::Command::new("synapse")
        .args(["list", "--role", &acting])
        .env("SYNAPSE_HOME", home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start synapse: {e}"))?;
    let mut pipe = child.stdout.take().ok_or("no stdout")?;
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = std::io::Read::read_to_string(&mut pipe, &mut buf);
        buf
    });
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() >= std::time::Duration::from_secs(10) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("synapse list timed out after 10 s".to_string());
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(25)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let text = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(format!("synapse list exited with {status}"));
    }
    parse_presence(&text)
}

/// The identity home most rows name (the daemon is asked through it); `None` when none names one.
pub fn common_home(rows: &[AuditRow]) -> Option<PathBuf> {
    let mut n: BTreeMap<&str, usize> = BTreeMap::new();
    for h in rows.iter().filter_map(|r| r.home.as_deref()) {
        *n.entry(h).or_default() += 1;
    }
    n.into_iter()
        .max_by_key(|(_, c)| *c)
        .map(|(h, _)| PathBuf::from(h))
}

/// The table, then a summary line. `Ok` is whether no row has a `Fail`.
pub fn render(rows: &[AuditRow], presence: &PresenceMap, now: DateTime<Utc>) -> (String, bool) {
    let mut out = String::from(
        "role\tchannels\tmodel\tmode\tstate\tmcp\thome\tcert\tonline\tqueued\tfindings\n",
    );
    let (mut fail_rows, mut warn_rows) = (0, 0);
    for r in rows {
        let f = findings(r, now);
        if f.iter().any(|(s, _)| *s == Severity::Fail) {
            fail_rows += 1;
        } else if !f.is_empty() {
            warn_rows += 1;
        }
        let ch = r.channels.map_or("?".to_string(), |c| c.text());
        let cert = match &r.cert {
            Cert::Valid(t) => format!("until {}", t.format("%Y-%m-%d")),
            Cert::Expired(t) => format!("EXPIRED {}", t.format("%Y-%m-%d")),
            Cert::Missing => "missing".to_string(),
            Cert::Unreadable => "unreadable".to_string(),
        };
        let (online, queued) = match &r.presence {
            Some(p) => (
                if p.online { "yes" } else { "no" }.to_string(),
                match (p.queued, p.oldest_enqueued_at) {
                    (0, _) | (_, None) => p.queued.to_string(),
                    (n, Some(t)) => format!("{n} ({})", age_text((now - t).num_seconds())),
                },
            ),
            None => ("?".to_string(), "?".to_string()),
        };
        let texts: Vec<&str> = f.iter().map(|(_, t)| t.as_str()).collect();
        out.push_str(&format!(
            "{}\t{ch}\t{}\t{}\t{}\t{}\t{}\t{cert}\t{online}\t{queued}\t{}\n",
            r.role,
            r.model.as_deref().unwrap_or("-"),
            r.permission_mode.as_deref().unwrap_or("-"),
            r.state.as_deref().unwrap_or("-"),
            if r.mcp.is_ok() { "ok" } else { "BAD" },
            if r.home_present { "present" } else { "MISSING" },
            if texts.is_empty() {
                "-".to_string()
            } else {
                texts.join("; ")
            },
        ));
    }
    if let Err(e) = presence {
        out.push_str(&format!("presence unavailable: {e}\n"));
    }
    out.push_str(&format!(
        "AUDIT {} role(s): {fail_rows} failing, {warn_rows} warning, {} clean\n",
        rows.len(),
        rows.len() - fail_rows - warn_rows
    ));
    (out, fail_rows == 0)
}

#[cfg(test)]
mod tests;
