// SPDX-License-Identifier: MIT OR Apache-2.0
//! A minimal client for the local claude-peers broker (`127.0.0.1:7899`), enough to ask a lane to
//! wrap up before it is stopped.
//!
//! The API, as observed from the live broker on 2026-10-07 (not copied from OverMind's client):
//!
//! * `POST /list-peers` with `{"scope":"machine","cwd":"...","git_root":null}` answers a JSON array of
//!   `{id, pid, cwd, git_root, tty, registered_at, last_seen, summary}`. **`pid` is the MCP helper
//!   process, not the lane**: its *parent* is the lane's `claude` (verified on three live lanes), so
//!   a peer is joined to an agent by that parent, never by `cwd` (two live lanes share
//!   `C:\Projects\auth-framework`) and never by the peer id (ids rotate on every restart).
//! * `POST /send-message` with `{"from_id","to_id","text"}` answers `{"ok":true}` or
//!   `{"ok":false,"error":...}`. The sender id is not a peer, so a reply goes nowhere, and the message
//!   says so.
//!
//! Everything speaks plain HTTP/1.1 over loopback with `Connection: close` and short timeouts, and the
//! address is checked to be loopback by the configuration, never here.

use lane_state::claude_proc::ParentProcess;
use serde::Deserialize;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// The `from_id` agentlife sends as. It is not a registered peer.
pub const SENDER_ID: &str = "agentlife";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Peer {
    pub id: String,
    pub pid: u32,
    pub cwd: String,
}

/// Talking to the broker, as a trait so the stop sequence is testable without one.
pub trait Messenger {
    fn peers(&self) -> Result<Vec<Peer>, String>;
    fn send(&self, to_id: &str, text: &str) -> Result<(), String>;

    /// Who to ask to wrap up for the lane `role` whose `claude` is `claude_pid`. The broker joins
    /// by process parent; a role-addressed transport (Synapse) overrides this and answers with
    /// the role itself, so `send` takes whichever `id` this returns.
    fn recipients(
        &self,
        _role: &str,
        claude_pid: u32,
        parents: &dyn ParentProcess,
    ) -> Result<Vec<Peer>, String> {
        let all = self.peers()?;
        Ok(peers_of_claude(&all, parents, claude_pid)
            .into_iter()
            .cloned()
            .collect())
    }
}

pub struct Broker {
    addr: SocketAddr,
    timeout: Duration,
}

impl Broker {
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            timeout: Duration::from_secs(5),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn post(&self, path: &str, body: &serde_json::Value) -> Result<String, String> {
        let mut stream = TcpStream::connect_timeout(&self.addr, self.timeout)
            .map_err(|e| format!("claude-peers broker at {}: {e}", self.addr))?;
        let _ = stream.set_read_timeout(Some(self.timeout));
        let _ = stream.set_write_timeout(Some(self.timeout));
        let payload = body.to_string();
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            self.addr,
            payload.len()
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|e| format!("broker write: {e}"))?;
        let mut raw = Vec::new();
        stream
            .read_to_end(&mut raw)
            .map_err(|e| format!("broker read: {e}"))?;
        parse_response(&raw, path)
    }
}

/// The body of a 200 response, de-chunked if the broker chunked it.
pub fn parse_response(raw: &[u8], path: &str) -> Result<String, String> {
    let text = String::from_utf8_lossy(raw);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or("the broker's reply has no header terminator")?;
    let status_line = head.lines().next().unwrap_or("");
    if status_line.split_whitespace().nth(1) != Some("200") {
        return Err(format!("broker {path}: {status_line}"));
    }
    let chunked = head.lines().any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    if chunked {
        dechunk(body)
    } else {
        Ok(body.to_string())
    }
}

fn dechunk(mut s: &str) -> Result<String, String> {
    let mut out = String::new();
    loop {
        let (size_line, rest) = s.split_once("\r\n").ok_or("bad chunked body")?;
        let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| "bad chunk size".to_string())?;
        if size == 0 {
            return Ok(out);
        }
        out.push_str(rest.get(..size).ok_or("short chunk")?);
        s = rest.get(size + 2..).ok_or("short chunk")?;
    }
}

impl Messenger for Broker {
    fn peers(&self) -> Result<Vec<Peer>, String> {
        let body = self.post(
            "/list-peers",
            &serde_json::json!({"scope": "machine", "cwd": "", "git_root": null}),
        )?;
        serde_json::from_str(&body).map_err(|e| format!("unexpected /list-peers reply: {e}"))
    }

    fn send(&self, to_id: &str, text: &str) -> Result<(), String> {
        #[derive(Deserialize)]
        struct Reply {
            ok: bool,
            error: Option<String>,
        }
        let body = self.post(
            "/send-message",
            &serde_json::json!({"from_id": SENDER_ID, "to_id": to_id, "text": text}),
        )?;
        let reply: Reply = serde_json::from_str(&body)
            .map_err(|e| format!("unexpected /send-message reply: {e}"))?;
        if reply.ok {
            Ok(())
        } else {
            Err(reply.error.unwrap_or_else(|| "refused".into()))
        }
    }
}

/// The role `agentlife down` sends as on Synapse: claimed per run (PM-run, rare), so it never
/// collides with a lane's own role or the launcher service's.
pub const CLI_ROLE: &str = "agentlife-cli";

/// Asking a lane to wrap up through the Synapse daemon: addressed by role, no pid join. The role
/// must be online (a down lane has nothing to wrap up) and have claimed once on this daemon.
pub struct SynapseMessenger {
    daemon: synapsed_client::Daemon,
}

impl SynapseMessenger {
    /// Finds this home's daemon (starting `synapsed` if nothing answers, as every Synapse client
    /// does) and proves it. The role is claimed on first use.
    pub fn connect(home: &std::path::Path) -> Result<Self, String> {
        synapsed_client::Daemon::connect(home)
            .map(|daemon| Self { daemon })
            .map_err(|e| e.to_string())
    }

    fn online(&self) -> Result<std::collections::BTreeSet<String>, String> {
        self.daemon
            .call(CLI_ROLE, "/v1/list", None)
            .map(|v| crate::launcher_service::parse_online(&v))
            .map_err(|e| e.to_string())
    }
}

impl Messenger for SynapseMessenger {
    fn peers(&self) -> Result<Vec<Peer>, String> {
        Ok(self
            .online()?
            .into_iter()
            .map(|role| Peer {
                id: role,
                pid: 0,
                cwd: String::new(),
            })
            .collect())
    }

    fn send(&self, to_id: &str, text: &str) -> Result<(), String> {
        let id = format!(
            "agentlife-down-{}-{}",
            chrono::Utc::now().timestamp_millis(),
            std::process::id()
        );
        self.daemon
            .call(
                CLI_ROLE,
                "/v1/send",
                Some(&crate::launcher_service::send_body(to_id, &id, text)),
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn recipients(
        &self,
        role: &str,
        claude_pid: u32,
        _parents: &dyn ParentProcess,
    ) -> Result<Vec<Peer>, String> {
        Ok(self
            .online()?
            .contains(role)
            .then(|| Peer {
                id: role.to_string(),
                pid: claude_pid,
                cwd: String::new(),
            })
            .into_iter()
            .collect())
    }
}

/// Where this home's daemon says it listens (`<home>/synapsed.json`, `{"addr": ...}`), for a
/// reachability probe that must not start a daemon. `None`: no such file, or no address in it.
pub fn synapse_addr(home: &std::path::Path) -> Option<SocketAddr> {
    let text = std::fs::read_to_string(home.join("synapsed.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("addr")?.as_str()?.parse().ok()
}

/// The peers whose MCP helper is a direct child of `claude_pid`.
pub fn peers_of_claude<'a>(
    peers: &'a [Peer],
    parents: &dyn ParentProcess,
    claude_pid: u32,
) -> Vec<&'a Peer> {
    peers
        .iter()
        .filter(|p| {
            parents
                .parent_of(p.pid)
                .is_some_and(|(parent, _)| parent == claude_pid)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::net::TcpListener;

    /// Serves `replies` (raw HTTP responses) to successive connections, recording each request.
    fn serve(replies: Vec<String>) -> (SocketAddr, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for reply in replies {
                let Ok((mut s, _)) = listener.accept() else {
                    return;
                };
                let mut buf = vec![0u8; 8192];
                let n = s.read(&mut buf).unwrap_or(0);
                let _ = tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
                let _ = s.write_all(reply.as_bytes());
            }
        });
        (addr, rx)
    }

    fn ok(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn the_daemon_address_is_read_from_synapsed_json_or_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(synapse_addr(dir.path()), None, "no file");
        let p = dir.path().join("synapsed.json");
        std::fs::write(&p, r#"{"addr":"127.0.0.1:7920","instance_id":"x","pid":1}"#).unwrap();
        assert_eq!(
            synapse_addr(dir.path()),
            Some(SocketAddr::from(([127, 0, 0, 1], 7920)))
        );
        std::fs::write(&p, r#"{"addr":"nonsense"}"#).unwrap();
        assert_eq!(synapse_addr(dir.path()), None, "bad address");
        std::fs::write(&p, "not json").unwrap();
        assert_eq!(synapse_addr(dir.path()), None, "bad json");
    }

    struct Fixed(Vec<Peer>);
    impl Messenger for Fixed {
        fn peers(&self) -> Result<Vec<Peer>, String> {
            Ok(self.0.clone())
        }
        fn send(&self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn the_default_recipients_are_the_peers_below_the_claude_process() {
        let peers = vec![
            Peer {
                id: "a".into(),
                pid: 1,
                cwd: String::new(),
            },
            Peer {
                id: "b".into(),
                pid: 2,
                cwd: String::new(),
            },
        ];
        let parents = Parents(HashMap::from([
            (1, (10, "claude.exe".to_string())),
            (2, (20, "claude.exe".to_string())),
        ]));
        let got = Fixed(peers).recipients("pm", 10, &parents).unwrap();
        assert_eq!(got.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn it_lists_peers_and_sends_a_message_over_real_loopback_http() {
        let peers = r#"[{"id":"abc12345","pid":4242,"cwd":"C:\\Projects\\x","git_root":null,"tty":null,"registered_at":"t","last_seen":"t","summary":"s"}]"#;
        let (addr, rx) = serve(vec![ok(peers), ok(r#"{"ok":true}"#)]);
        let broker = Broker::new(addr);
        let got = broker.peers().unwrap();
        assert_eq!(
            got,
            vec![Peer {
                id: "abc12345".into(),
                pid: 4242,
                cwd: "C:\\Projects\\x".into()
            }]
        );
        broker.send("abc12345", "hello").unwrap();
        let first = rx.recv().unwrap();
        assert!(first.starts_with("POST /list-peers HTTP/1.1"), "{first}");
        let second = rx.recv().unwrap();
        assert!(
            second.starts_with("POST /send-message HTTP/1.1"),
            "{second}"
        );
        assert!(
            second.contains(r#""from_id":"agentlife""#) && second.contains(r#""to_id":"abc12345""#),
            "{second}"
        );
        assert!(second.contains(r#""text":"hello""#), "{second}");
    }

    #[test]
    fn a_chunked_reply_is_decoded() {
        let body = r#"[{"id":"a","pid":1,"cwd":"c"}]"#;
        let chunked = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n",
            body.len(),
            body
        );
        let (addr, _rx) = serve(vec![chunked]);
        assert_eq!(Broker::new(addr).peers().unwrap().len(), 1);
    }

    #[test]
    fn a_refusal_and_a_non_200_are_errors_with_the_reason() {
        let (addr, _rx) = serve(vec![
            ok(r#"{"ok":false,"error":"no such peer"}"#),
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n".to_string(),
        ]);
        let b = Broker::new(addr);
        assert_eq!(b.send("x", "t").unwrap_err(), "no such peer");
        assert!(b.send("x", "t").unwrap_err().contains("500"));
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        let (addr, _rx) = serve(vec![
            ok("not json"),
            "garbage with no header end".to_string(),
        ]);
        let b = Broker::new(addr);
        assert!(b
            .peers()
            .unwrap_err()
            .contains("unexpected /list-peers reply"));
        assert!(b.peers().is_err());
    }

    #[test]
    fn a_broker_that_is_not_there_is_an_error() {
        // Bind then drop, so the port is closed.
        let addr = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let result = Broker::new(addr)
            .with_timeout(Duration::from_millis(300))
            .peers();
        // A parallel test's listener may have taken the freed port and answered: then nothing was
        // closed, and there is no closed-port error to check. Otherwise only that it is an error: the
        // text depends on the OS (Windows reports 10060).
        if let Err(err) = result {
            assert!(!err.is_empty());
        }
    }

    #[test]
    fn a_broker_that_never_answers_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let _hold = std::thread::spawn(move || {
            let _conn = listener.accept();
            std::thread::sleep(Duration::from_secs(3));
        });
        let started = std::time::Instant::now();
        let err = Broker::new(addr)
            .with_timeout(Duration::from_millis(400))
            .peers();
        assert!(err.is_err());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "it waited too long"
        );
    }

    struct Parents(HashMap<u32, (u32, String)>);
    impl ParentProcess for Parents {
        fn parent_of(&self, pid: u32) -> Option<(u32, String)> {
            self.0.get(&pid).cloned()
        }
        fn cmdline_of(&self, _: u32) -> Option<Vec<String>> {
            None
        }
    }

    #[test]
    fn a_peer_is_joined_to_a_lane_by_its_parent_not_by_cwd_or_id() {
        let peers = vec![
            Peer {
                id: "p-a".into(),
                pid: 501,
                cwd: "C:/Projects/auth-framework".into(),
            },
            Peer {
                id: "p-b".into(),
                pid: 502,
                cwd: "C:/Projects/auth-framework".into(),
            },
            Peer {
                id: "p-c".into(),
                pid: 503,
                cwd: "C:/Projects/other".into(),
            },
        ];
        // Two lanes in ONE cwd (claude 10 and 20), plus an unrelated one (30).
        let parents = Parents(HashMap::from([
            (501, (10, "claude.exe".to_string())),
            (502, (20, "claude.exe".to_string())),
            (503, (30, "claude.exe".to_string())),
        ]));
        let for_10: Vec<_> = peers_of_claude(&peers, &parents, 10)
            .into_iter()
            .map(|p| p.id.as_str())
            .collect();
        let for_20: Vec<_> = peers_of_claude(&peers, &parents, 20)
            .into_iter()
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(for_10, ["p-a"], "cwd alone would have matched both peers");
        assert_eq!(for_20, ["p-b"]);
        assert!(peers_of_claude(&peers, &parents, 99).is_empty());
        // A peer whose process cannot be found joins nobody.
        let none = Parents(HashMap::new());
        assert!(peers_of_claude(&peers, &none, 10).is_empty());
    }
}
