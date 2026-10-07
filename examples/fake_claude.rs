// SPDX-License-Identifier: MIT OR Apache-2.0
//! A stand-in for `claude`, for tests that need the real process chain.
//!
//! The hook finds its agent by walking up from itself to a process **named `claude`**, then reads
//! that process's real command line and start time. A fake cannot prove any of that, so the test
//! copies this binary to a temp directory as `claude` / `claude.exe`, launches it with a realistic
//! argv, and lets it run the REAL `agentlife hook` as its child, exactly the chain
//! `claude -> (shell) -> hook` a real session has.
//!
//! Flags it understands (all stay in its own command line, which is the point: the hook records
//! that argv as `launch_args`):
//! * `--fake-agentlife <exe>`  the `agentlife` binary to run as the hook;
//! * `--fake-script <file>`    tab-separated lines, run in order: `EVENT<TAB>REASON-or-<TAB>JSON-FILE` runs
//!   the hook; `CMD<TAB>arg<TAB>arg...` runs `agentlife <args>` as this process's child;
//! * `--fake-report <file>`    written when the script is done: `EVENT<TAB>millis<TAB>exit-code`;
//! * `--fake-hold <secs>`      stay alive this long afterwards (default 0);
//! * `--fake-child <pidfile>`  start a long-lived non-shell child (the stand-in for a lane's MCP helper,
//!   whose parent is the `claude`) and write its pid to `<pidfile>`;
//! * `--fake-shell-child`     also start a long-lived SHELL child (a backgrounded command);
//!
//! Launched by the restore launcher the stand-in has no flags of its own (the launcher builds its argv),
//! so each setting also has an environment fallback: `FAKE_AGENTLIFE`, `FAKE_REPORT`, `FAKE_HOLD`, and
//! `FAKE_AUTO_START=1`, which runs a `SessionStart` hook for itself with a payload it builds (session id
//! `sess-<pid>-<nanos>`, unique even when Windows hands a pid to a new process, `cwd` = its own working directory). `FAKE_ENVLOG=<file>` appends one line saying which
//! agent id and which session-identity variables this process was started with.
//!
//! * `--fake-nest`             do not run the script: start a copy of this same program (so the
//!   copy has a `claude` parent) with the other arguments and wait for it.

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A child that lives for a minute. `shell` makes it a SHELL (`cmd` / `sh`), so it counts as a
/// backgrounded command; otherwise it is `ping` / `sleep`, which does not.
fn long_child(shell: bool) -> Child {
    #[cfg(windows)]
    let mut c = if shell {
        let mut c = Command::new("cmd");
        c.args(["/C", "ping -n 60 127.0.0.1 > nul"]);
        c
    } else {
        let mut c = Command::new("ping");
        c.args(["-n", "60", "127.0.0.1"]);
        c
    };
    #[cfg(not(windows))]
    let mut c = if shell {
        // `; true` keeps the shell alive: a lone `sleep` would be exec'd in place of it.
        let mut c = Command::new("sh");
        c.args(["-c", "sleep 60; true"]);
        c
    } else {
        let mut c = Command::new("sleep");
        c.arg("60");
        c
    };
    c.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start a long-lived child")
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env_for = |flag: &str| -> Option<String> {
        let key = match flag {
            "--fake-agentlife" => "FAKE_AGENTLIFE",
            "--fake-report" => "FAKE_REPORT",
            "--fake-hold" => "FAKE_HOLD",
            _ => return None,
        };
        std::env::var(key).ok().filter(|v| !v.is_empty())
    };
    let get = |flag: &str| -> Option<String> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1).cloned())
            .or_else(|| env_for(flag))
    };

    if let Ok(log) = std::env::var("FAKE_ENVLOG") {
        const IDENTITY: &[&str] = &[
            "CLAUDECODE",
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_ENTRYPOINT",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_SESSION_ATTENDED",
            "CLAUDE_CODE_BRIDGE_SESSION_ID",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "CLAUDE_CODE_MESSAGING_TOKEN",
            "CLAUDE_CODE_SSE_PORT",
            "CLAUDE_PID",
        ];
        let present: Vec<&str> = IDENTITY
            .iter()
            .copied()
            .filter(|k| std::env::var_os(k).is_some())
            .collect();
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
        {
            let _ = writeln!(
                f,
                "agent_id={} identity={}",
                std::env::var("AGENTLIFE_AGENT_ID").unwrap_or_else(|_| "-".into()),
                present.join(",")
            );
        }
    }

    if args.iter().any(|a| a == "--fake-nest") {
        let rest: Vec<String> = args
            .iter()
            .filter(|a| *a != "--fake-nest")
            .cloned()
            .collect();
        let exe = std::env::current_exe().expect("own path");
        // Retry on "Text file busy" (ETXTBSY): see `spawn_retrying` in tests/hook_e2e.rs.
        let mut tries = 0;
        let status = loop {
            match Command::new(&exe).args(&rest).status() {
                Ok(s) => break s,
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 100 => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("start the nested copy: {e}"),
            }
        };
        std::process::exit(status.code().unwrap_or(1));
    }

    // Children first, so they exist before any script line runs.
    let mut _children: Vec<Child> = Vec::new();
    if let Some(pidfile) = get("--fake-child") {
        let child = long_child(false);
        let part = format!("{pidfile}.part");
        std::fs::write(&part, child.id().to_string()).expect("write the child's pid");
        std::fs::rename(&part, &pidfile).expect("publish the child's pid");
        _children.push(child);
    }
    if args.iter().any(|a| a == "--fake-shell-child") {
        _children.push(long_child(true));
    }

    let agentlife = get("--fake-agentlife").expect("--fake-agentlife <exe> (or FAKE_AGENTLIFE)");
    let mut report = String::new();
    let script_text = if let Some(script) = get("--fake-script") {
        Some(std::fs::read_to_string(&script).expect("read the script"))
    } else if std::env::var("FAKE_AUTO_START").is_ok_and(|v| v == "1") {
        // Build this session's own SessionStart payload and write it where the script can read it.
        let cwd = std::env::current_dir().expect("cwd").display().to_string();
        let payload = serde_json::json!({
            "hook_event_name": "SessionStart",
            "session_id": format!(
                "sess-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ),
            "cwd": cwd,
            "source": "startup",
        })
        .to_string();
        let file =
            std::env::temp_dir().join(format!("fake-claude-start-{}.json", std::process::id()));
        std::fs::write(&file, payload).expect("write the payload");
        Some(format!("SessionStart\t-\t{}\n", file.display()))
    } else {
        None
    };
    if let Some(text) = script_text {
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let mut parts = line.split('\t');
            let event = parts.next().expect("event");
            if event == "CMD" {
                // `CMD<TAB>arg<TAB>arg...`: run `agentlife <args>` as this process's child, so the
                // caller is a real registered agent (the only honest way to test who-may-do-what).
                let args: Vec<&str> = parts.collect();
                let started = Instant::now();
                let output = Command::new(&agentlife)
                    .args(&args)
                    .stdin(Stdio::null())
                    .output()
                    .expect("run the command");
                let status = output.status;
                // Keep what the command said: when a test fails on another OS, this is the only
                // way to learn WHY (the exit code alone does not say).
                if let Some(path) = get("--fake-report") {
                    let mut f = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(format!("{path}.cmdout"))
                        .expect("open the command log");
                    let _ = writeln!(
                        f,
                        "$ agentlife {}\n{}{}[exit {}]\n",
                        args.join(" "),
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr),
                        status.code().unwrap_or(-1)
                    );
                }
                report.push_str(&format!(
                    "CMD\t{}\t{}\n",
                    started.elapsed().as_millis(),
                    status.code().unwrap_or(-1)
                ));
                continue;
            }
            let reason = parts.next().expect("reason or -");
            let json_file = parts.next().expect("json file");
            let json = std::fs::read(json_file).expect("read the payload");
            let mut cmd = Command::new(&agentlife);
            cmd.args(["hook", event]);
            if reason != "-" {
                cmd.args(["--reason", reason]);
            }
            let started = Instant::now();
            let mut child = cmd
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("start the hook");
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(&json)
                .expect("write the payload");
            let status = child.wait().expect("wait for the hook");
            report.push_str(&format!(
                "{event}\t{}\t{}\n",
                started.elapsed().as_millis(),
                status.code().unwrap_or(-1)
            ));
        }
    }
    if let Some(path) = get("--fake-report") {
        // Atomic: a test polls for this file, and a plain `fs::write` creates it before its content is
        // there, so a poller could read it empty (it did, once, on the Windows CI leg: the test saw
        // zero report lines). Write beside it, then rename into place.
        let part = format!("{path}.part");
        std::fs::write(&part, report).expect("write the report");
        std::fs::rename(&part, &path).expect("publish the report");
    }
    if let Some(secs) = get("--fake-hold").and_then(|s| s.parse::<u64>().ok()) {
        std::thread::sleep(Duration::from_secs(secs));
    }
}
