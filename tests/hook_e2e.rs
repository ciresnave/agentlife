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

    /// A script written verbatim, for lines the typed helper above cannot express (`CMD`).
    fn raw_script(&self, name: &str, text: &str) -> PathBuf {
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

// ---- M2a: the intent rule, marks and park/stop/unpark, against real processes ---------------------

fn cmd_line(args: &[&str]) -> String {
    format!("CMD\t{}\n", args.join("\t"))
}

fn start_line(payload: &Path) -> String {
    format!("SessionStart\t-\t{}\n", payload.display())
}

fn end_line(reason: &str, payload: &Path) -> String {
    format!("SessionEnd\t{reason}\t{}\n", payload.display())
}

fn record_named(rig: &Rig, name: &str) -> agentlife::registry::AgentRecord {
    let recs = rig.registry().list().unwrap().records;
    let mut found: Vec<_> = recs
        .into_iter()
        .filter(|r| r.name.as_deref() == Some(name))
        .collect();
    assert_eq!(found.len(), 1, "agent {name:?}: {}", rig.log());
    found.remove(0)
}

fn json_of(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&out.stdout)))
}

#[test]
fn reconcile_tells_an_exited_agent_from_a_vanished_one_in_the_real_registry() {
    let rig = Rig::new();
    let s1 = rig.payload("s1.json", "SessionStart", "sess-1", Some("startup"));
    let e1 = rig.payload("e1.json", "SessionEnd", "sess-1", None);
    let s2 = rig.payload("s2.json", "SessionStart", "sess-2", Some("startup"));
    // One agent whose person exited from the prompt; one whose process simply vanished (no
    // SessionEnd), which is what a kill looks like.
    rig.run(
        &["-n", "exited-lane"],
        &rig.raw_script(
            "a.txt",
            &(start_line(&s1) + &end_line("prompt_input_exit", &e1)),
        ),
        &[],
    );
    rig.run(
        &["-n", "vanished-lane"],
        &rig.raw_script("b.txt", &start_line(&s2)),
        &[],
    );

    let out = rig.agentlife(&["reconcile", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_of(&out);
    let by_name = |n: &str| {
        v.as_array()
            .unwrap()
            .iter()
            .find(|x| x["name"] == n)
            .cloned()
            .unwrap()
    };
    let exited = by_name("exited-lane");
    assert!(
        exited["verdict"]
            .as_str()
            .unwrap()
            .starts_with("closed on purpose"),
        "{exited}"
    );
    assert_eq!(exited["persisted"], true);
    let vanished = by_name("vanished-lane");
    assert!(
        vanished["verdict"]
            .as_str()
            .unwrap()
            .contains("restore candidate"),
        "{vanished}"
    );
    assert_eq!(
        vanished["persisted"], false,
        "a candidate is derived, never written"
    );

    // The decision is written down: `list --all` now shows it, and the vanished one is still wanted.
    let list = json_of(&rig.agentlife(&["list", "--all", "--json"]));
    let intent_of = |n: &str| {
        list.as_array()
            .unwrap()
            .iter()
            .find(|x| x["name"] == n)
            .unwrap()["intent"]
            .clone()
    };
    assert_eq!(intent_of("exited-lane"), "exited");
    assert_eq!(intent_of("vanished-lane"), "wanted");
    // And it is stable: a second reconcile does not re-derive it.
    let again = json_of(&rig.agentlife(&["reconcile", "--json"]));
    let second = again
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["name"] == "exited-lane")
        .unwrap();
    assert!(
        second["verdict"]
            .as_str()
            .unwrap()
            .starts_with("not evaluated"),
        "{second}"
    );
    assert_eq!(second["persisted"], false);
    // A dry run writes nothing at all.
    let rig2 = Rig::new();
    let s = rig2.payload("s.json", "SessionStart", "sess-1", Some("startup"));
    let e = rig2.payload("e.json", "SessionEnd", "sess-1", None);
    rig2.run(
        &["-n", "dry"],
        &rig2.raw_script("a.txt", &(start_line(&s) + &end_line("logout", &e))),
        &[],
    );
    let dry = json_of(&rig2.agentlife(&["reconcile", "--dry-run", "--json"]));
    assert!(dry[0]["verdict"]
        .as_str()
        .unwrap()
        .starts_with("closed on purpose"));
    assert_eq!(
        record_named(&rig2, "dry").intent,
        agentlife::registry::Intent::Wanted
    );
}

#[test]
fn a_lane_can_mark_itself_pinned_and_waiting_and_list_shows_it() {
    let rig = Rig::new();
    let s = rig.payload("s.json", "SessionStart", "sess-1", Some("startup"));
    let script = rig.raw_script(
        "a.txt",
        &(start_line(&s)
            + &cmd_line(&["waiting", "--note", "needs your OK"])
            + &cmd_line(&["pin", "marker-lane"])),
    );
    let results = rig.run(&["-n", "marker-lane"], &script, &[]);
    let cmds: Vec<_> = results.iter().filter(|r| r.0 == "CMD").collect();
    assert_eq!(cmds.len(), 2);
    assert!(
        cmds.iter().all(|c| c.2 == 0),
        "a lane may mark itself: {results:?}\n{}",
        rig.log()
    );

    let rec = record_named(&rig, "marker-lane");
    let w = rec.waiting.expect("the waiting mark was written");
    assert_eq!(
        (w.on.as_str(), w.note.as_deref()),
        ("user", Some("needs your OK"))
    );
    let pin = rec.pinned.expect("the pin was written");
    assert_eq!(
        pin.by,
        format!("agent:{}", rec.agent_id),
        "recorded as the agent itself"
    );

    let list = json_of(&rig.agentlife(&["list", "--json"]));
    assert_eq!(list[0]["pinned"], "explicit");
    assert_eq!(list[0]["waiting"], true);
}

#[test]
fn a_lane_cannot_park_another_agent_and_nothing_changes() {
    let rig = Rig::new();
    let t = rig.payload("t.json", "SessionStart", "sess-t", Some("startup"));
    let i = rig.payload("i.json", "SessionStart", "sess-i", Some("startup"));
    rig.run(
        &["-n", "target"],
        &rig.raw_script("a.txt", &start_line(&t)),
        &[],
    );
    // The intruder is a registered, ordinary lane. It parks the (stopped) target, and also pins
    // ITSELF in the same run, so the refusal below is policy and not a broken rig.
    let script = rig.raw_script(
        "b.txt",
        &(start_line(&i) + &cmd_line(&["park", "target"]) + &cmd_line(&["pin", "intruder"])),
    );
    let results = rig.run(&["-n", "intruder"], &script, &[]);
    let cmds: Vec<_> = results.iter().filter(|r| r.0 == "CMD").collect();
    assert_ne!(
        cmds[0].2, 0,
        "parking another agent must be refused: {results:?}"
    );
    assert_eq!(
        cmds[1].2, 0,
        "the positive control (pinning itself) succeeded"
    );
    assert_eq!(
        record_named(&rig, "target").intent,
        agentlife::registry::Intent::Wanted
    );
    let kinds: Vec<_> = Journal::new(rig.home.join("registry"), std::sync::Arc::new(SystemClock))
        .read_all()
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert!(!kinds.iter().any(|k| k == "closed"), "{kinds:?}");
}

#[test]
fn the_pm_by_the_visible_rule_can_park_and_unpark_a_stopped_agent_and_a_lookalike_cannot() {
    let rig = Rig::new();
    // The PM is named by the rule: pin_roles + portfolio_root. Point the root at the test's
    // directory, where the stand-in's payload says it was launched.
    std::fs::create_dir_all(&rig.home).unwrap();
    std::fs::write(
        rig.home.join("config.json"),
        serde_json::json!({"portfolio_root": rig.cwd(), "pin_roles": ["pm"]}).to_string(),
    )
    .unwrap();
    let t = rig.payload("t.json", "SessionStart", "sess-t", Some("startup"));
    let p = rig.payload("p.json", "SessionStart", "sess-p", Some("startup"));
    rig.run(
        &["-n", "target"],
        &rig.raw_script("a.txt", &start_line(&t)),
        &[],
    );
    let script = rig.raw_script(
        "b.txt",
        &(start_line(&p)
            + &cmd_line(&["park", "target"])
            + &cmd_line(&["unpark", "target", "--no-start"])
            + &cmd_line(&["park", "target"])),
    );
    let results = rig.run(&["-n", "PM"], &script, &[]);
    let cmds: Vec<_> = results.iter().filter(|r| r.0 == "CMD").collect();
    assert_eq!(cmds.len(), 3);
    assert!(
        cmds.iter().all(|c| c.2 == 0),
        "the PM may park and unpark a stopped agent: {results:?}\n{}",
        rig.log()
    );
    let pm_id = record_named(&rig, "PM").agent_id;
    let target = record_named(&rig, "target");
    assert!(
        matches!(
            &target.intent,
            agentlife::registry::Intent::Closed { how: agentlife::registry::ClosedHow::Parked, by, .. }
                if *by == format!("pm-agent:{pm_id}")
        ),
        "{:?}",
        target.intent
    );
    let kinds: Vec<_> = Journal::new(rig.home.join("registry"), std::sync::Arc::new(SystemClock))
        .read_all()
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(
        kinds
            .iter()
            .filter(|k| *k == "closed" || *k == "reopened")
            .collect::<Vec<_>>(),
        ["closed", "reopened", "closed"]
    );

    // The lookalike: the same name, launched with NO config naming this directory as the portfolio
    // root, is just a lane, and is refused. (Same flow, fresh home, default rule.)
    let rig2 = Rig::new();
    let t2 = rig2.payload("t.json", "SessionStart", "sess-t", Some("startup"));
    let p2 = rig2.payload("p.json", "SessionStart", "sess-p", Some("startup"));
    rig2.run(
        &["-n", "target"],
        &rig2.raw_script("a.txt", &start_line(&t2)),
        &[],
    );
    let r2 = rig2.run(
        &["-n", "PM"],
        &rig2.raw_script("b.txt", &(start_line(&p2) + &cmd_line(&["park", "target"]))),
        &[],
    );
    let c2: Vec<_> = r2.iter().filter(|r| r.0 == "CMD").collect();
    assert_ne!(
        c2[0].2, 0,
        "a lane named PM in the wrong directory is not the PM: {r2:?}"
    );
    assert_eq!(
        record_named(&rig2, "target").intent,
        agentlife::registry::Intent::Wanted
    );
}

// ---- M2b: graceful stop of a RUNNING agent, against real processes -------------------------------
//
// A stand-in `claude` plays the target lane (registered through the real hook, with a long-lived
// child standing in for its MCP helper). A fake claude-peers broker listens on REAL loopback and
// speaks the broker's two endpoints. The PM is another stand-in that runs the real `agentlife park`
// as its child. A test thread plays the lane's side: when the wrap-up message arrives it writes a
// HANDOFF and an idle claim, exactly what the message asks. The kill at the end is real.

use agentlife::down::{SysinfoTerminator, Terminator};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

struct FakeBroker {
    addr: SocketAddr,
    /// `(to_id, text)` of every message the broker was asked to deliver.
    received: Arc<Mutex<Vec<(String, String)>>>,
    peers: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
}

impl FakeBroker {
    fn start() -> FakeBroker {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let peers = Arc::new(Mutex::new("[]".to_string()));
        let stop = Arc::new(AtomicBool::new(false));
        let (r, p, st) = (received.clone(), peers.clone(), stop.clone());
        std::thread::spawn(move || {
            while !st.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut conn, _)) => {
                        conn.set_nonblocking(false).ok();
                        conn.set_read_timeout(Some(Duration::from_secs(5))).ok();
                        let mut buf = Vec::new();
                        let mut chunk = [0u8; 4096];
                        // Read the head, then exactly Content-Length body bytes.
                        loop {
                            let n = conn.read(&mut chunk).unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            buf.extend_from_slice(&chunk[..n]);
                            let text = String::from_utf8_lossy(&buf).into_owned();
                            if let Some((head, body)) = text.split_once("\r\n\r\n") {
                                let want: usize = head
                                    .lines()
                                    .find_map(|l| {
                                        l.to_ascii_lowercase()
                                            .strip_prefix("content-length:")
                                            .map(|v| v.trim().parse().unwrap_or(0))
                                    })
                                    .unwrap_or(0);
                                if body.len() >= want {
                                    break;
                                }
                            }
                        }
                        let text = String::from_utf8_lossy(&buf).into_owned();
                        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
                        let reply = if head.starts_with("POST /list-peers") {
                            p.lock().unwrap().clone()
                        } else if head.starts_with("POST /send-message") {
                            let v: serde_json::Value =
                                serde_json::from_str(body).unwrap_or_default();
                            r.lock().unwrap().push((
                                v["to_id"].as_str().unwrap_or("").to_string(),
                                v["text"].as_str().unwrap_or("").to_string(),
                            ));
                            r#"{"ok":true}"#.to_string()
                        } else {
                            "{}".to_string()
                        };
                        let _ = conn.write_all(
                            format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).as_bytes(),
                        );
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(20)),
                }
            }
        });
        FakeBroker {
            addr,
            received,
            peers,
            stop,
        }
    }

    fn set_peers(&self, peers: serde_json::Value) {
        *self.peers.lock().unwrap() = peers.to_string();
    }

    fn messages(&self) -> Vec<(String, String)> {
        self.received.lock().unwrap().clone()
    }
}

impl Drop for FakeBroker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn pid_alive(pid: u32) -> bool {
    SysinfoTable.identity_of(pid).is_some()
}

fn wait_until(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Kills a stand-in (and whatever it started) when the test ends, pass or fail.
struct Cleanup(Vec<u32>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        for pid in &self.0 {
            if let Some(id) = SysinfoTable.identity_of(*pid) {
                let _ = SysinfoTerminator.kill_verified(&id);
            }
        }
    }
}

struct Down {
    rig: Rig,
    broker: FakeBroker,
    lane_state: PathBuf,
    target: Child,
    target_pid: u32,
    helper_pid: u32,
    _cleanup: Cleanup,
}

/// A running target lane `target`, registered through the real hook, with a helper child, plus a
/// broker that knows the helper as the lane's peer and a config pointing at it.
fn running_target(extra_target_args: &[&str], peers_match: bool) -> Down {
    let rig = Rig::new();
    let broker = FakeBroker::start();
    let lane_state = rig.dir.path().join("lane-state");
    std::fs::create_dir_all(&lane_state).unwrap();
    std::fs::create_dir_all(&rig.home).unwrap();
    std::fs::write(
        rig.home.join("config.json"),
        serde_json::json!({
            "portfolio_root": rig.cwd(),
            "pin_roles": ["pm"],
            "peers_addr": broker.addr.to_string(),
            "lane_state_dir": lane_state.display().to_string(),
            "down_poll_secs": 1
        })
        .to_string(),
    )
    .unwrap();
    let start = rig.payload("t-start.json", "SessionStart", "sess-t", Some("startup"));
    let script = rig.raw_script("t.txt", &start_line(&start));
    let report = rig.dir.path().join("t-report.txt");
    let pidfile = rig.dir.path().join("t-helper.pid");
    let mut args: Vec<&str> = vec!["-n", "target"];
    args.extend_from_slice(extra_target_args);
    let pidfile_s = pidfile.display().to_string();
    args.extend_from_slice(&["--fake-child", &pidfile_s]);
    let target = rig.spawn(&args, &script, &report, 120, &[]);
    let target_pid = target.id();
    wait_for(&report);
    let helper_pid: u32 = wait_for(&pidfile).trim().parse().unwrap();
    let peer_pid = if peers_match { helper_pid } else { 999_999 };
    broker.set_peers(serde_json::json!([
        {"id": "peer-t", "pid": peer_pid, "cwd": rig.cwd(), "git_root": null, "tty": null,
         "registered_at": "t", "last_seen": "t", "summary": "target lane"}
    ]));
    Down {
        rig,
        broker,
        lane_state,
        target,
        target_pid,
        helper_pid,
        _cleanup: Cleanup(vec![target_pid, helper_pid]),
    }
}

/// The lane's side of the protocol: once a wrap-up message arrives, write a HANDOFF and an idle
/// claim for the lane's session. Returns a handle to join.
fn play_the_lane(d: &Down) -> std::thread::JoinHandle<()> {
    let received = d.broker.received.clone();
    let handoff = d.rig.dir.path().join("HANDOFF.md");
    let state = d.lane_state.join("target.json");
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(60);
        while received.lock().unwrap().is_empty() {
            if Instant::now() > deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        std::thread::sleep(Duration::from_millis(400)); // "working"
        std::fs::write(&handoff, "# HANDOFF - target\nwritten on request\n").unwrap();
        std::fs::write(
            &state,
            serde_json::json!({
                "role": "target", "session_id": "sess-t", "pid": 1,
                "busy": false, "subagents_running": 0, "no_background_shells": true,
                "updated_at": chrono::Utc::now().to_rfc3339(),
                "updated_by_event": "Stop"
            })
            .to_string(),
        )
        .unwrap();
    })
}

/// Runs the PM stand-in, which runs `agentlife park target <args>` as its child. Returns that
/// command's exit code.
fn pm_runs(d: &Down, park_args: &[&str]) -> i32 {
    let rig = &d.rig;
    let p = rig.payload("pm-start.json", "SessionStart", "sess-pm", Some("startup"));
    let mut cmd = vec!["park", "target"];
    cmd.extend_from_slice(park_args);
    let script = rig.raw_script("pm.txt", &(start_line(&p) + &cmd_line(&cmd)));
    let results = rig.run(&["-n", "PM"], &script, &[]);
    let c: Vec<_> = results.iter().filter(|r| r.0 == "CMD").collect();
    assert_eq!(c.len(), 1, "{results:?}\n{}", rig.log());
    c[0].2
}

/// A person at a terminal runs `park target ...`: not under any `claude`, so the caller is a person.
/// (Run from inside a real Claude Code session this is NOT a person and the park is refused; CI is
/// not under one.) Returns the exit code and what it printed.
fn person_runs(d: &Down, park_args: &[&str]) -> (i32, String) {
    let mut args = vec!["park", "target"];
    args.extend_from_slice(park_args);
    let out = d.rig.agentlife(&args);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code().unwrap_or(-1), text)
}

/// Everything the stand-ins' commands printed, for failure messages.
fn command_output(rig: &Rig) -> String {
    let mut out = String::new();
    if let Ok(rd) = std::fs::read_dir(rig.dir.path()) {
        let mut files: Vec<_> = rd
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".cmdout"))
            .collect();
        files.sort_by_key(|e| e.file_name());
        for f in files {
            out.push_str(&std::fs::read_to_string(f.path()).unwrap_or_default());
        }
    }
    out
}

fn journal_kinds(rig: &Rig) -> Vec<String> {
    Journal::new(rig.home.join("registry"), std::sync::Arc::new(SystemClock))
        .read_all()
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.kind)
        .collect()
}

#[test]
fn a_person_gracefully_parks_a_running_lane_and_the_process_really_ends() {
    let mut d = running_target(&[], true);
    assert!(
        pid_alive(d.target_pid),
        "the target is running before the park"
    );
    let lane = play_the_lane(&d);
    let (code, printed) = person_runs(&d, &["--yes", "--timeout", "60"]);
    lane.join().unwrap();
    assert_eq!(
        code,
        0,
        "park must succeed:\n{}\n--- what the command printed:\n{}",
        d.rig.log(),
        printed
    );

    // The REAL process is gone: pid and start time, not merely "the pid changed".
    wait_until("the target process to end", 15, || !pid_alive(d.target_pid));
    let _ = d.target.wait();
    // The helper below it was not the target and is untouched by agentlife.
    assert!(
        pid_alive(d.helper_pid),
        "agentlife ended only the lane's own process"
    );

    // It asked exactly once, to the right peer, in the words the protocol needs.
    let msgs = d.broker.messages();
    assert_eq!(msgs.len(), 1, "{msgs:?}");
    assert_eq!(msgs[0].0, "peer-t");
    assert!(
        msgs[0].1.contains("assert-idle") && msgs[0].1.contains("HANDOFF"),
        "{}",
        msgs[0].1
    );
    assert!(
        msgs[0].1.contains("person"),
        "it names who asked: {}",
        msgs[0].1
    );

    // The registry says parked, by a person, and the journal shows the order of events.
    let rec = record_named(&d.rig, "target");
    assert!(
        matches!(&rec.intent, agentlife::registry::Intent::Closed { how: agentlife::registry::ClosedHow::Parked, by, .. } if by == "person"),
        "{:?}",
        rec.intent
    );
    let kinds = journal_kinds(&d.rig);
    let pos = |k: &str| {
        kinds
            .iter()
            .position(|x| x == k)
            .unwrap_or_else(|| panic!("no {k} in {kinds:?}"))
    };
    assert!(
        pos("down-requested") < pos("closed") && pos("closed") < pos("stopped"),
        "{kinds:?}"
    );
}

/// Who may stop a RUNNING agent: only a person. The PM agent is refused; nothing is asked, nothing
/// ends, and the intent is untouched. The happy-path test above is the control (same rig, a person).
#[test]
fn the_pm_agent_cannot_stop_a_running_lane() {
    let d = running_target(&[], true);
    let lane = play_the_lane(&d);
    let code = pm_runs(&d, &["--yes", "--timeout", "4"]);
    drop(lane);
    assert_ne!(code, 0, "{}", d.rig.log());
    let printed = command_output(&d.rig);
    assert!(
        printed.contains("only a person may stop a RUNNING agent"),
        "{printed}"
    );
    assert!(d.broker.messages().is_empty(), "nobody was messaged");
    assert!(pid_alive(d.target_pid), "the lane is still running");
    assert_eq!(
        record_named(&d.rig, "target").intent,
        agentlife::registry::Intent::Wanted
    );
    assert!(!journal_kinds(&d.rig)
        .iter()
        .any(|k| k == "closed" || k == "stopped"));
}

#[test]
fn without_yes_nothing_is_asked_and_nothing_is_stopped() {
    let d = running_target(&[], true);
    let (code, _) = person_runs(&d, &[]);
    assert_eq!(
        code, 2,
        "a plan only: exit 2, distinct from success and refusal"
    );
    assert!(d.broker.messages().is_empty(), "nothing was sent");
    assert!(pid_alive(d.target_pid), "the target is untouched");
    assert_eq!(
        record_named(&d.rig, "target").intent,
        agentlife::registry::Intent::Wanted
    );
    assert!(!journal_kinds(&d.rig)
        .iter()
        .any(|k| k == "closed" || k == "stopped"));
}

#[test]
fn a_lane_that_never_wraps_up_is_left_running() {
    let d = running_target(&[], true);
    // No lane simulation: nobody writes a HANDOFF or an idle claim.
    let (code, _) = person_runs(&d, &["--yes", "--timeout", "4"]);
    assert_ne!(code, 0, "{}", d.rig.log());
    assert_eq!(d.broker.messages().len(), 1, "it did ask");
    assert!(pid_alive(d.target_pid), "and it left the lane running");
    assert_eq!(
        record_named(&d.rig, "target").intent,
        agentlife::registry::Intent::Wanted
    );
    let kinds = journal_kinds(&d.rig);
    assert!(kinds.iter().any(|k| k == "down-timeout"), "{kinds:?}");
    assert!(
        !kinds.iter().any(|k| k == "closed" || k == "stopped"),
        "{kinds:?}"
    );
}

#[test]
fn a_live_shell_below_the_lane_blocks_the_stop_even_after_it_wrote_everything() {
    let d = running_target(&["--fake-shell-child"], true);
    let lane = play_the_lane(&d);
    let (code, _) = person_runs(&d, &["--yes", "--timeout", "5"]);
    lane.join().unwrap();
    assert_ne!(
        code,
        0,
        "a backgrounded command is a real child process: {}",
        d.rig.log()
    );
    assert!(pid_alive(d.target_pid), "the lane was left running");
    assert_eq!(
        record_named(&d.rig, "target").intent,
        agentlife::registry::Intent::Wanted
    );
    // The recorded blocker must NAME the live shell. Otherwise a timeout here could just as well be
    // the lane simulation failing; the happy-path test above is the control (same rig, no shell).
    let entries = Journal::new(
        d.rig.home.join("registry"),
        std::sync::Arc::new(SystemClock),
    )
    .read_all()
    .unwrap()
    .entries;
    let timeout = entries
        .iter()
        .find(|e| e.kind == "down-timeout")
        .unwrap_or_else(|| {
            panic!(
                "no down-timeout in {:?}",
                entries.iter().map(|e| &e.kind).collect::<Vec<_>>()
            )
        });
    let blockers: Vec<String> = timeout.data["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        blockers.len(),
        1,
        "the lane wrote everything, so the shell is the only blocker: {blockers:?}"
    );
    assert!(
        blockers[0].contains("a live shell is running below it"),
        "{blockers:?}"
    );
}

#[test]
fn a_lane_that_cannot_be_found_among_the_peers_is_not_asked_and_not_stopped() {
    // The broker has a peer, but its helper is not below the target's process: the join is by
    // parent process, so this peer is nobody's.
    let d = running_target(&[], false);
    let lane = play_the_lane(&d);
    let (code, _) = person_runs(&d, &["--yes", "--timeout", "4"]);
    assert_ne!(code, 0, "{}", d.rig.log());
    assert!(d.broker.messages().is_empty(), "nobody was messaged");
    assert!(pid_alive(d.target_pid));
    assert_eq!(
        record_named(&d.rig, "target").intent,
        agentlife::registry::Intent::Wanted
    );
    drop(lane); // the lane thread gives up on its own after its own deadline
}
