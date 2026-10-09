// SPDX-License-Identifier: MIT OR Apache-2.0

//! The ask flow — RESTART-TOOL-DESIGN.md §12.6.
//!
//! ⚠️ Found 2026-09-27: the only watcher of the old `awaiting_confirmation`
//! stdout line was the process a `--self` restart kills, and nothing read
//! `.lane-state/unhandled-prompts/`, so an unapproved dialog reached
//! CireSnave on screen instead of as a question. The host (which survives
//! the restart - it IS the relaunched tab) now tells the notify role's lane
//! itself, through the claude-peers broker on localhost, the moment its
//! startup window ends with a dialog nobody answered.
//!
//! The broker API used (`POST /list-peers`, `POST /send-message`) is the
//! one in `claude-peers-mcp`'s `broker.ts`; the sender id is not a peer, so
//! a reply goes nowhere, and the message says so.

use crate::paths::paths_match;
use crate::state;
use serde::Deserialize;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

pub const SENDER_ID: &str = "lane-restart-host";

/// `CLAUDE_PEERS_PORT`, else the broker's own default, 7899.
pub fn broker_addr() -> SocketAddr {
    let port = std::env::var("CLAUDE_PEERS_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(7899);
    SocketAddr::from(([127, 0, 0, 1], port))
}

#[derive(Debug, Clone, Deserialize)]
pub struct Peer {
    pub id: String,
    pub cwd: String,
}

/// A minimal HTTP/1.1 JSON POST to the local broker: `Connection: close`,
/// 5s timeouts, `Content-Length` or chunked bodies. Only ever talks to
/// 127.0.0.1.
pub fn post_json(addr: SocketAddr, path: &str, body: &serde_json::Value) -> Result<String, String> {
    let timeout = Duration::from_secs(5);
    let mut stream =
        TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("broker {addr}: {e}"))?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let payload = body.to_string();
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("broker write: {e}"))?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("broker read: {e}"))?;
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or("broker sent no HTTP header terminator")?;
    let status_ok = head
        .lines()
        .next()
        .is_some_and(|l| l.split_whitespace().nth(1) == Some("200"));
    let chunked = head.lines().any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    let body = if chunked {
        dechunk(body)?
    } else {
        body.to_string()
    };
    if !status_ok {
        return Err(format!(
            "broker {path}: {}",
            head.lines().next().unwrap_or("?")
        ));
    }
    Ok(body)
}

fn dechunk(mut s: &str) -> Result<String, String> {
    let mut out = String::new();
    loop {
        let (size_line, rest) = s.split_once("\r\n").ok_or("bad chunked body")?;
        let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| "bad chunk size")?;
        if size == 0 {
            return Ok(out);
        }
        out.push_str(rest.get(..size).ok_or("short chunk")?);
        s = rest.get(size + 2..).ok_or("short chunk")?;
    }
}

/// Every live peer whose cwd is `cwd`.
pub fn peers_in(addr: SocketAddr, cwd: &str) -> Result<Vec<Peer>, String> {
    let body = post_json(
        addr,
        "/list-peers",
        &serde_json::json!({"scope": "machine", "cwd": cwd, "git_root": null}),
    )?;
    let peers: Vec<Peer> =
        serde_json::from_str(&body).map_err(|e| format!("unexpected /list-peers reply: {e}"))?;
    Ok(peers
        .into_iter()
        .filter(|p| paths_match(&p.cwd, cwd))
        .collect())
}

pub fn send(addr: SocketAddr, to_id: &str, text: &str) -> Result<(), String> {
    #[derive(Deserialize)]
    struct Reply {
        ok: bool,
        error: Option<String>,
    }
    let body = post_json(
        addr,
        "/send-message",
        &serde_json::json!({"from_id": SENDER_ID, "to_id": to_id, "text": text}),
    )?;
    let reply: Reply =
        serde_json::from_str(&body).map_err(|e| format!("unexpected /send-message reply: {e}"))?;
    if reply.ok {
        Ok(())
    } else {
        Err(reply.error.unwrap_or_else(|| "refused".into()))
    }
}

/// Sends `text` to every live peer in the notify role's own cwd (read from
/// its `.lane-state/<role>.json`). Returns the peer ids it reached; an
/// error if it reached none.
pub fn notify_role(
    addr: SocketAddr,
    state_dir: &Path,
    notify_role: &str,
    text: &str,
) -> Result<Vec<String>, String> {
    let target = state::load(state_dir, notify_role)
        .map_err(|e| format!("cannot find the {notify_role} lane: {e}"))?;
    let peers = peers_in(addr, &target.cwd)?;
    if peers.is_empty() {
        return Err(format!(
            "no live claude-peers session in {} (the {notify_role} lane's cwd)",
            target.cwd
        ));
    }
    let mut reached = Vec::new();
    let mut errors = Vec::new();
    for p in peers {
        match send(addr, &p.id, text) {
            Ok(()) => reached.push(p.id),
            Err(e) => errors.push(format!("{}: {e}", p.id)),
        }
    }
    if reached.is_empty() {
        Err(errors.join("; "))
    } else {
        Ok(reached)
    }
}

/// Whether the lane `role` has shown real progress since `since`: its
/// state file was written after `since` by an event other than
/// `SessionStart` - the same "processed a prompt" signal the outer
/// restart's liveness check uses (§5). A session blocked on a startup
/// dialog never gets that far.
pub fn lane_progressed_since(
    state_dir: &Path,
    role: &str,
    since: chrono::DateTime<chrono::Utc>,
) -> bool {
    state::load(state_dir, role)
        .is_ok_and(|s| s.updated_at > since && s.updated_by_event != "SessionStart")
}

/// The `[ASK]` message (`C:/Projects/CLAUDE.md` §10 compact format).
pub struct Ask<'a> {
    pub role: &'a str,
    pub notify_role: &'a str,
    pub host_pid: u32,
    pub capture_path: Option<&'a Path>,
    pub approvals_summary: &'a str,
    pub approvals_source: Option<(&'a str, &'a str)>,
    pub screen_text: &'a str,
}

impl Ask<'_> {
    pub fn text(&self) -> String {
        let capture = self
            .capture_path
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "NOT-WRITTEN".into());
        let (options, where_to) = match self.approvals_source {
            Some((repo, path)) => (
                format!("approval-PR-to-{repo},decline"),
                format!(
                    "if the user approves this exact dialog, open a PR adding {path}/<id>.json to \
                     {repo} with their words quoted verbatim - their merge is the approval"
                ),
            ),
            None => (
                "configure-~/.overmind/lane-restart.json,decline".to_string(),
                "no approvals repo is configured, so nothing can be approved yet".to_string(),
            ),
        };
        let screen: Vec<&str> = self
            .screen_text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.chars().all(|c| c == '─'))
            .take(15)
            .collect();
        format!(
            "[ASK] lane-restart-unmatched-startup-dialog to={notify} options={options} \
             recommend=- blocks={role}-relaunch\n\
             note: `lane-restart host` (pid {pid}) relaunched `{role}`; a startup dialog no \
             approval matched is waiting on its screen.\n\
             note: screen capture: {capture}\n\
             note: approvals: {summary}\n\
             note: ask the user; {where_to}. Never approve a dialog that asks for a secret.\n\
             note: screen: {screen}\n\
             note: sent by a tool, not a session - replies go nowhere.",
            notify = self.notify_role,
            role = self.role,
            pid = self.host_pid,
            summary = self.approvals_summary,
            screen = screen.join(" / "),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A one-request fake broker: returns what it received and replies
    /// with `reply` (a full HTTP response).
    fn fake_broker(replies: Vec<String>) -> (SocketAddr, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for reply in replies {
                let (mut s, _) = listener.accept().unwrap();
                s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = s.read(&mut chunk).unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                    let t = String::from_utf8_lossy(&buf).to_string();
                    if let Some((head, body)) = t.split_once("\r\n\r\n") {
                        let len: usize = head
                            .lines()
                            .find_map(|l| l.strip_prefix("Content-Length: "))
                            .unwrap()
                            .parse()
                            .unwrap();
                        if body.len() >= len {
                            seen.push(t);
                            break;
                        }
                    }
                }
                s.write_all(reply.as_bytes()).unwrap();
            }
            seen
        });
        (addr, handle)
    }

    fn ok_json(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    fn write_state(dir: &Path, role: &str, cwd: &str, event: &str, at: &str) {
        let s = serde_json::json!({
            "role": role, "session_id": "s", "pid": 1, "cwd": cwd, "name": null,
            "model": null, "permission_mode": null, "remote_control": false, "busy": false,
            "subagents_running": 0, "no_background_shells": null, "launch_args": null,
            "updated_at": at, "updated_by_event": event
        });
        std::fs::write(dir.join(format!("{role}.json")), s.to_string()).unwrap();
    }

    #[test]
    fn notify_role_sends_to_the_peer_in_the_roles_cwd_only() {
        let dir = tempfile::tempdir().unwrap();
        write_state(
            dir.path(),
            "pm",
            "C:\\Projects",
            "Stop",
            "2026-09-27T00:00:00Z",
        );
        let peers =
            r#"[{"id":"pm1","cwd":"C:\\Projects"},{"id":"other","cwd":"C:\\Projects\\OverMind"}]"#;
        let (addr, h) = fake_broker(vec![ok_json(peers), ok_json(r#"{"ok":true}"#)]);
        let reached = notify_role(addr, dir.path(), "pm", "hello").unwrap();
        assert_eq!(reached, vec!["pm1".to_string()]);
        let seen = h.join().unwrap();
        assert!(seen[0].starts_with("POST /list-peers "));
        assert!(seen[1].starts_with("POST /send-message "));
        assert!(seen[1].contains(r#""to_id":"pm1""#));
        assert!(seen[1].contains(r#""from_id":"lane-restart-host""#));
    }

    /// The real broker, the real state dir: sends one `[ASK]`-shaped
    /// message to the lane named by `LANE_RESTART_LIVE_NOTIFY_ROLE` (use
    /// your OWN role). Opt-in: `cargo test -- --ignored live_`.
    #[test]
    #[ignore]
    fn live_notify_reaches_a_real_peer() {
        let role = std::env::var("LANE_RESTART_LIVE_NOTIFY_ROLE")
            .expect("set LANE_RESTART_LIVE_NOTIFY_ROLE to your own role");
        let text = Ask {
            role: "live-test",
            notify_role: &role,
            host_pid: std::process::id(),
            capture_path: None,
            approvals_summary: "live test - ignore",
            approvals_source: None,
            screen_text: "LIVE TEST of lane-restart notify.rs - no action needed",
        }
        .text();
        let reached = notify_role(
            broker_addr(),
            Path::new("C:/Projects/.lane-state"),
            &role,
            &text,
        )
        .unwrap();
        assert!(!reached.is_empty());
    }

    #[test]
    fn notify_role_errors_when_no_peer_is_in_that_cwd() {
        let dir = tempfile::tempdir().unwrap();
        write_state(
            dir.path(),
            "pm",
            "C:\\Projects",
            "Stop",
            "2026-09-27T00:00:00Z",
        );
        let (addr, _h) = fake_broker(vec![ok_json(r#"[{"id":"x","cwd":"C:\\Elsewhere"}]"#)]);
        let err = notify_role(addr, dir.path(), "pm", "hello").unwrap_err();
        assert!(err.contains("no live claude-peers session"), "{err}");
    }

    #[test]
    fn notify_role_errors_without_a_state_file_for_the_role() {
        let dir = tempfile::tempdir().unwrap();
        let addr = SocketAddr::from(([127, 0, 0, 1], 1));
        assert!(notify_role(addr, dir.path(), "pm", "x")
            .unwrap_err()
            .contains("pm lane"));
    }

    #[test]
    fn send_surfaces_a_broker_refusal() {
        let (addr, _h) = fake_broker(vec![ok_json(r#"{"ok":false,"error":"Peer z not found"}"#)]);
        assert_eq!(send(addr, "z", "t").unwrap_err(), "Peer z not found");
    }

    #[test]
    fn post_json_decodes_a_chunked_reply_and_rejects_a_non_200() {
        let (a, b) = (r#"[{"id"#, r#"":"a","cwd":"C:\\x"}]"#);
        let chunked = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{a}\r\n{:x}\r\n{b}\r\n0\r\n\r\n",
            a.len(),
            b.len()
        );
        let (addr, _h) = fake_broker(vec![chunked]);
        let body = post_json(addr, "/list-peers", &serde_json::json!({})).unwrap();
        assert_eq!(body, r#"[{"id":"a","cwd":"C:\\x"}]"#);

        let (addr, _h) = fake_broker(vec![
            "HTTP/1.1 500 Internal\r\nContent-Length: 2\r\n\r\n{}".to_string()
        ]);
        assert!(post_json(addr, "/x", &serde_json::json!({}))
            .unwrap_err()
            .contains("500"));
    }

    #[test]
    fn a_closed_port_is_an_error_not_a_hang() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        assert!(post_json(addr, "/x", &serde_json::json!({})).is_err());
    }

    #[test]
    fn lane_progressed_since_needs_a_later_non_sessionstart_write() {
        let dir = tempfile::tempdir().unwrap();
        let since: chrono::DateTime<chrono::Utc> = "2026-09-27T12:00:00Z".parse().unwrap();
        write_state(
            dir.path(),
            "x",
            "C:\\x",
            "UserPromptSubmit",
            "2026-09-27T12:00:05Z",
        );
        assert!(lane_progressed_since(dir.path(), "x", since));
        write_state(
            dir.path(),
            "x",
            "C:\\x",
            "SessionStart",
            "2026-09-27T12:00:05Z",
        );
        assert!(!lane_progressed_since(dir.path(), "x", since));
        write_state(
            dir.path(),
            "x",
            "C:\\x",
            "UserPromptSubmit",
            "2026-09-27T11:59:00Z",
        );
        assert!(!lane_progressed_since(dir.path(), "x", since));
        assert!(!lane_progressed_since(dir.path(), "missing", since));
    }

    #[test]
    fn ask_text_is_one_compact_header_plus_notes() {
        let ask = Ask {
            role: "unpopped",
            notify_role: "pm",
            host_pid: 42,
            capture_path: Some(Path::new("C:/x/y.txt")),
            approvals_summary: "0 active, 0 refused, from a/b:c@123",
            approvals_source: Some(("a/b", "c")),
            screen_text: "────\n  WARNING: something\n\n  ❯ 1. Yes\n",
        };
        let t = ask.text();
        let first = t.lines().next().unwrap();
        assert!(first.starts_with("[ASK] lane-restart-unmatched-startup-dialog to=pm "));
        assert!(first.contains("blocks=unpopped-relaunch"));
        assert!(t.contains("screen capture: C:/x/y.txt"));
        assert!(t.contains("screen: WARNING: something / ❯ 1. Yes"));
        assert!(t.contains("open a PR adding c/<id>.json to a/b"));
    }
}
