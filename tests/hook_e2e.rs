// SPDX-License-Identifier: MIT OR Apache-2.0
//! The hook against the REAL process chain, not fakes.
//!
//! Each test copies `examples/fake_claude` to a temp directory as `claude`/`claude.exe`, launches
//! it with a realistic command line, and lets it run the real `agentlife hook` binary as its child,
//! exactly the `claude -> hook` chain of a live session. That is the one thing a unit test with a
//! fake `HookEnv` cannot show: that the ancestry walk, the command-line read and the start-time
//! read all work against real processes (OverMind's `cwd`/`cmd` bug passed 79 fake-based tests).
//! The acceptance check is a real registry write read back, not a diagnostic.

use agentlife::clock::SystemClock;
use agentlife::identity::{paths_equal, ProcessTable, SysinfoTable};
use agentlife::journal::Journal;
use agentlife::registry::Registry;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const AGENTLIFE: &str = env!("CARGO_BIN_EXE_agentlife");

fn example_path() -> PathBuf {
    let exe = std::env::current_exe().expect("test exe path");
    let debug = exe.parent().and_then(Path::parent).expect("target dir");
    let name = if cfg!(windows) {
        "fake_claude.exe"
    } else {
        "fake_claude"
    };
    debug.join("examples").join(name)
}

struct Rig {
    dir: tempfile::TempDir,
    home: PathBuf,
    claude: PathBuf,
}

impl Rig {
    fn new() -> Rig {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = example_path();
        assert!(
            src.exists(),
            "the fake_claude example is not built at {}: `cargo test` builds examples, `cargo test --test hook_e2e` may not",
            src.display()
        );
        let claude = dir.path().join(if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        });
        std::fs::copy(&src, &claude).expect("copy the stand-in");
        let home = dir.path().join("home");
        Rig { dir, home, claude }
    }

    fn cwd(&self) -> String {
        self.dir.path().display().to_string()
    }

    fn payload(&self, file: &str, event: &str, session: &str, source: Option<&str>) -> PathBuf {
        let mut v = serde_json::json!({
            "hook_event_name": event,
            "session_id": session,
            "cwd": self.cwd(),
        });
        if let Some(s) = source {
            v["source"] = serde_json::json!(s);
        }
        self.raw_payload(file, &v.to_string())
    }

    fn raw_payload(&self, file: &str, text: &str) -> PathBuf {
        let p = self.dir.path().join(file);
        std::fs::write(&p, text).unwrap();
        p
    }

    fn script(&self, name: &str, lines: &[(&str, &str, &Path)]) -> PathBuf {
        let text: String = lines
            .iter()
            .map(|(e, r, j)| format!("{e}\t{r}\t{}\n", j.display()))
            .collect();
        let p = self.dir.path().join(name);
        std::fs::write(&p, text).unwrap();
        p
    }

    fn spawn(
        &self,
        extra: &[&str],
        script: &Path,
        report: &Path,
        hold_secs: u64,
        env: &[(&str, &str)],
    ) -> Child {
        let mut c = Command::new(&self.claude);
        c.args(extra)
            .args(["--fake-agentlife", AGENTLIFE])
            .arg("--fake-script")
            .arg(script)
            .arg("--fake-report")
            .arg(report)
            .args(["--fake-hold", &hold_secs.to_string()])
            .env("AGENTLIFE_HOME", &self.home)
            .env_remove("AGENTLIFE_AGENT_ID")
            .env_remove("LANE_ROLE")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (k, v) in env {
            c.env(k, v);
        }
        spawn_retrying(&mut c)
    }

    /// Runs a stand-in to completion (hold 0) and returns its report lines.
    fn run(&self, extra: &[&str], script: &Path, env: &[(&str, &str)]) -> Vec<(String, u128, i32)> {
        let report = self.dir.path().join(format!("report-{}.txt", unique()));
        let mut child = self.spawn(extra, script, &report, 0, env);
        child.wait().expect("wait for the stand-in");
        parse_report(&std::fs::read_to_string(&report).unwrap_or_default())
    }

    fn registry(&self) -> Registry {
        Registry::new(self.home.join("registry").join("agents"))
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.home.join("hook.log")).unwrap_or_default()
    }

    fn agentlife(&self, args: &[&str]) -> std::process::Output {
        Command::new(AGENTLIFE)
            .args(args)
            .env("AGENTLIFE_HOME", &self.home)
            .output()
            .expect("run agentlife")
    }
}

/// The `total N ms` the hook logged for its last run of `event`.
fn hook_total_ms(log: &str, event: &str) -> Option<u128> {
    log.lines()
        .rev()
        .find(|l| l.contains(&format!(" {event} ")))
        .and_then(|l| l.split("total ").nth(1))
        .and_then(|rest| rest.split(' ').next())
        .and_then(|n| n.parse().ok())
}

/// Starts `c`, retrying while Linux answers "Text file busy" (ETXTBSY, errno 26).
///
/// The tests copy an executable and then run it, in parallel. A `fork` in a sibling test can hold
/// the copy's write handle open for the instant before its own `exec`, and `exec`ing a file that
/// anyone holds open for writing fails with ETXTBSY. It is a property of the test harness, not of
/// the hook, and it showed up only on the Ubuntu CI leg (two of three attempts at one commit).
fn spawn_retrying(c: &mut Command) -> Child {
    for _ in 0..100 {
        match c.spawn() {
            Ok(child) => return child,
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("start the stand-in claude: {e}"),
        }
    }
    panic!("start the stand-in claude: still `Text file busy` after 5 s");
}

fn unique() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn parse_report(text: &str) -> Vec<(String, u128, i32)> {
    text.lines()
        .map(|l| {
            let mut p = l.split('\t');
            (
                p.next().unwrap().to_string(),
                p.next().unwrap().parse().unwrap(),
                p.next().unwrap().parse().unwrap(),
            )
        })
        .collect()
}

fn wait_for(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(t) = std::fs::read_to_string(path) {
            return t;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

const LANE_ARGS: &[&str] = &[
    "-n",
    "e2e-lane",
    "--model",
    "sonnet",
    "--permission-mode",
    "auto",
    "--dangerously-load-development-channels",
    "server:claude-peers",
];

#[test]
fn a_session_start_through_the_real_chain_registers_the_agent_and_list_sees_it_alive() {
    let rig = Rig::new();
    let start = rig.payload("start.json", "SessionStart", "sess-1", Some("startup"));
    let script = rig.script("script.txt", &[("SessionStart", "-", &start)]);
    let report = rig.dir.path().join("report.txt");
    let mut child = rig.spawn(LANE_ARGS, &script, &report, 30, &[]);
    let results = parse_report(&wait_for(&report));
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].2, 0, "the hook must exit 0");

    let recs = rig.registry().list().unwrap();
    assert!(recs.problems.is_empty(), "{:?}", recs.problems);
    assert_eq!(recs.records.len(), 1, "log was:\n{}", rig.log());
    let r = &recs.records[0];
    assert_eq!(r.name.as_deref(), Some("e2e-lane"));
    assert_eq!(r.model, None, "no model in this payload, none invented");
    assert_eq!(r.permission_mode.as_deref(), Some("auto"));
    assert!(
        paths_equal(&r.launch_cwd, &rig.cwd()),
        "{} vs {}",
        r.launch_cwd,
        rig.cwd()
    );
    // The argv is the REAL command line of the real process, verbatim, flags and all.
    let args = r.launch_args.as_ref().expect("launch_args recorded");
    assert!(args
        .iter()
        .any(|a| a == "--dangerously-load-development-channels"));
    assert!(args.iter().any(|a| a == "server:claude-peers"));
    assert!(args.iter().any(|a| a == "-n") && args.iter().any(|a| a == "e2e-lane"));
    // The session is exactly the stand-in's process: its real pid and real start time.
    let ident = SysinfoTable
        .identity_of(child.id())
        .expect("the stand-in is alive");
    assert_eq!(r.sessions.len(), 1);
    assert_eq!(r.sessions[0].pid, child.id());
    assert_eq!(r.sessions[0].process_start_secs, Some(ident.start_secs));
    assert!(r.sessions[0].ended_at.is_none());
    // The O(1) pointer exists for that exact process.
    let pointer = rig.home.join("registry").join("procs").join(format!(
        "{}-{}.id",
        child.id(),
        ident.start_secs
    ));
    assert_eq!(
        std::fs::read_to_string(pointer).unwrap().trim(),
        r.agent_id.as_str()
    );
    assert!(
        rig.log().contains("SessionStart ok registered"),
        "{}",
        rig.log()
    );

    // `list` shows it running while the process lives...
    let out = rig.agentlife(&["list", "--json"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v[0]["liveness"], "running", "{v}");
    assert_eq!(v[0]["name"], "e2e-lane");
    // ...and stopped once it is killed (a kill fires no SessionEnd, like a Windows update).
    child.kill().unwrap();
    child.wait().unwrap();
    let out = rig.agentlife(&["list", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v[0]["liveness"], "stopped", "{v}");
    let text = rig.agentlife(&["list"]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.contains("e2e-lane") && text.contains("stopped"),
        "{text}"
    );
}

#[test]
fn session_end_stamps_the_reason_inside_its_budget_and_deletes_nothing() {
    let rig = Rig::new();
    let start = rig.payload("start.json", "SessionStart", "sess-1", Some("startup"));
    let end = rig.payload("end.json", "SessionEnd", "sess-1", None);
    let script = rig.script(
        "script.txt",
        &[
            ("SessionStart", "-", &start),
            ("SessionEnd", "prompt_input_exit", &end),
        ],
    );
    let results = rig.run(LANE_ARGS, &script, &[]);
    assert_eq!(results.len(), 2);
    assert!(
        results.iter().all(|r| r.2 == 0),
        "every hook exits 0: {results:?}"
    );
    // The documented SessionEnd budget is 1.5 s (hooks docs, fetched 2026-10-06). Two checks, and
    // why there are two:
    //
    // 1. The hook's OWN time, which it logs. This is the part our code controls. Measured on this
    //    machine under load: 110 to 190 ms (a process snapshot is ~80 ms of that).
    // 2. Wall time from spawn to exit, which also contains OS process creation. That is NOT ours:
    //    on this box even `cmd /c exit 0` ranges from 39 ms to 538 ms, a bare `agentlife
    //    --version` from 45 ms to 1.6 s, and the first run of a freshly built or copied exe is
    //    0.7 to 1.7 s (scanning, with 15 live lanes loading the machine). Asserting 1.5 s on it
    //    would test the OS, so it only guards against a catastrophe (a hung hook).
    let own_ms = hook_total_ms(&rig.log(), "SessionEnd").expect("SessionEnd logs its own timing");
    assert!(
        own_ms < 1000,
        "the SessionEnd hook's own work took {own_ms} ms (budget 1500 ms):\n{}",
        rig.log()
    );
    let wall_ms = results[1].1;
    eprintln!("SessionEnd: own {own_ms} ms, wall {wall_ms} ms (includes OS process creation)");
    assert!(
        wall_ms < 10_000,
        "SessionEnd wall time {wall_ms} ms: a hung hook?"
    );

    let recs = rig.registry().list().unwrap().records;
    assert_eq!(recs.len(), 1, "nothing deleted: {}", rig.log());
    let s = &recs[0].sessions[0];
    assert!(s.ended_at.is_some());
    assert_eq!(s.end_reason.as_deref(), Some("prompt_input_exit"));
    let kinds: Vec<_> = Journal::new(rig.home.join("registry"), std::sync::Arc::new(SystemClock))
        .read_all()
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(kinds, ["registered", "session-ended"]);
}

#[test]
fn a_headless_session_is_not_registered_and_the_log_says_why() {
    let rig = Rig::new();
    let start = rig.payload("start.json", "SessionStart", "sess-1", Some("startup"));
    let script = rig.script("script.txt", &[("SessionStart", "-", &start)]);
    let r = rig.run(&["-p", "do a thing"], &script, &[]);
    assert_eq!(r[0].2, 0);
    assert!(rig.registry().list().unwrap().records.is_empty());
    assert!(rig.log().contains("skipped: headless"), "{}", rig.log());
    // Positive control: the same chain without -p does register, so the empty result above is
    // the filter's doing and not a broken rig.
    rig.run(&["-n", "control"], &script, &[]);
    assert_eq!(rig.registry().list().unwrap().records.len(), 1);
}

#[test]
fn a_session_under_another_claude_is_not_registered() {
    let rig = Rig::new();
    let start = rig.payload("start.json", "SessionStart", "sess-1", Some("startup"));
    let script = rig.script("script.txt", &[("SessionStart", "-", &start)]);
    let report = rig.dir.path().join("report-nest.txt");
    // outer claude --fake-nest -> inner claude -> hook
    let mut child = rig.spawn(&["--fake-nest", "-n", "inner"], &script, &report, 0, &[]);
    child.wait().unwrap();
    assert!(
        rig.registry().list().unwrap().records.is_empty(),
        "{}",
        rig.log()
    );
    assert!(rig.log().contains("child session"), "{}", rig.log());
    // Positive control: without the outer claude the same inner registers.
    rig.run(&["-n", "inner"], &script, &[]);
    assert_eq!(rig.registry().list().unwrap().records.len(), 1);
}

#[test]
fn a_hook_run_outside_any_claude_is_skipped_and_still_exits_zero() {
    let rig = Rig::new();
    let mut child = Command::new(AGENTLIFE)
        .args(["hook", "SessionStart"])
        .env("AGENTLIFE_HOME", &rig.home)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            serde_json::json!({"hook_event_name": "SessionStart", "session_id": "s", "cwd": rig.cwd()})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
    assert!(child.wait().unwrap().success());
    assert!(rig.registry().list().unwrap().records.is_empty());
    assert!(
        rig.log().contains("not under a claude process"),
        "{}",
        rig.log()
    );
}

#[test]
fn malformed_hook_input_is_logged_as_an_error_and_never_fails_the_session() {
    let rig = Rig::new();
    let bad = rig.raw_payload("bad.json", "{ this is not json");
    let script = rig.script("script.txt", &[("SessionStart", "-", &bad)]);
    let r = rig.run(LANE_ARGS, &script, &[]);
    assert_eq!(r[0].2, 0, "a hook error must not fail the session");
    assert!(rig.log().contains("ERROR"), "{}", rig.log());
    assert!(rig.registry().list().unwrap().records.is_empty());
}

#[test]
fn the_agent_id_environment_variable_joins_two_different_launches_into_one_agent() {
    let rig = Rig::new();
    let s1 = rig.payload("s1.json", "SessionStart", "sess-1", Some("startup"));
    let s2 = rig.payload("s2.json", "SessionStart", "sess-2", Some("startup"));
    let id = [("AGENTLIFE_AGENT_ID", "e2e-agent")];
    rig.run(
        &["-n", "first"],
        &rig.script("a.txt", &[("SessionStart", "-", &s1)]),
        &id,
    );
    rig.run(
        &["-n", "second"],
        &rig.script("b.txt", &[("SessionStart", "-", &s2)]),
        &id,
    );
    let recs = rig.registry().list().unwrap().records;
    assert_eq!(recs.len(), 1, "{}", rig.log());
    assert_eq!(recs[0].agent_id.as_str(), "e2e-agent");
    assert_eq!(recs[0].sessions.len(), 2);
    assert_ne!(recs[0].sessions[0].pid, 0);
    assert_eq!(recs[0].name.as_deref(), Some("second"));
}

#[test]
fn a_hand_started_agent_started_again_after_its_process_died_is_one_agent() {
    let rig = Rig::new();
    let s1 = rig.payload("s1.json", "SessionStart", "sess-1", Some("startup"));
    let s2 = rig.payload("s2.json", "SessionStart", "sess-2", Some("startup"));
    let s3 = rig.payload("s3.json", "SessionStart", "sess-3", Some("startup"));
    rig.run(
        &["-n", "lane"],
        &rig.script("a.txt", &[("SessionStart", "-", &s1)]),
        &[],
    );
    // The first process is gone (it exited without a SessionEnd, like a killed one).
    rig.run(
        &["-n", "lane"],
        &rig.script("b.txt", &[("SessionStart", "-", &s2)]),
        &[],
    );
    let recs = rig.registry().list().unwrap().records;
    assert_eq!(
        recs.len(),
        1,
        "same name and launch directory: {}",
        rig.log()
    );
    assert_eq!(recs[0].sessions.len(), 2);
    assert!(
        recs[0].sessions[0].ended_at.is_none(),
        "no SessionEnd was seen: that absence is the evidence a later milestone reads"
    );
    // Positive control: a different name is a different agent.
    rig.run(
        &["-n", "other"],
        &rig.script("c.txt", &[("SessionStart", "-", &s3)]),
        &[],
    );
    assert_eq!(rig.registry().list().unwrap().records.len(), 2);
}

#[test]
fn clear_in_the_same_process_is_one_agent_whose_first_session_ended_with_clear() {
    let rig = Rig::new();
    let s1 = rig.payload("s1.json", "SessionStart", "sess-1", Some("startup"));
    let s2 = rig.payload("s2.json", "SessionStart", "sess-2", Some("clear"));
    let script = rig.script(
        "a.txt",
        &[("SessionStart", "-", &s1), ("SessionStart", "-", &s2)],
    );
    rig.run(&["-n", "lane"], &script, &[]);
    let recs = rig.registry().list().unwrap().records;
    assert_eq!(recs.len(), 1, "{}", rig.log());
    assert_eq!(recs[0].sessions.len(), 2);
    assert_eq!(recs[0].sessions[0].end_reason.as_deref(), Some("clear"));
    assert!(recs[0].sessions[1].ended_at.is_none());
}
