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
//! * `--fake-script <file>`    tab-separated lines `EVENT<TAB>REASON-or-<TAB>JSON-FILE`, run in order;
//! * `--fake-report <file>`    written when the script is done: `EVENT<TAB>millis<TAB>exit-code`;
//! * `--fake-hold <secs>`      stay alive this long afterwards (default 0);
//! * `--fake-nest`             do not run the script: start a copy of this same program (so the
//!   copy has a `claude` parent) with the other arguments and wait for it.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |flag: &str| -> Option<String> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1).cloned())
    };

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

    let agentlife = get("--fake-agentlife").expect("--fake-agentlife <exe>");
    let mut report = String::new();
    if let Some(script) = get("--fake-script") {
        let text = std::fs::read_to_string(&script).expect("read the script");
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let mut parts = line.split('\t');
            let event = parts.next().expect("event");
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
        std::fs::write(path, report).expect("write the report");
    }
    if let Some(secs) = get("--fake-hold").and_then(|s| s.parse::<u64>().ok()) {
        std::thread::sleep(Duration::from_secs(secs));
    }
}
