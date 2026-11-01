// SPDX-License-Identifier: MIT OR Apache-2.0

//! Waits v2: the supervisor's side of a `WAIT:` line (`waits.rs`). One pass, no daemon.
//!
//! A pass reads the HANDOFF of every live role, decides for each wait whether it is due, and tells
//! the lane. The callback comes from outside the lane, so the lane cannot ignore it.
//!
//! ⚠️ **A supervisor never runs lane-written text** (PM ruling 1A, 2026-11-01). A `check:` is text a
//! lane (or anything that wrote that file) chose, and running it would execute it with the
//! supervisor's authority. So a check is run only if it parses as one of a fixed set of read-only
//! forms ([`parse_check`]): every token drawn from a small character set (no shell metacharacter
//! survives it), exec'd directly with no shell, with a timeout. Anything else is never run: at its
//! wake time the lane is told "unverified, look yourself".
//!
//! Allowed forms (arguments are fixed; the only freedom is the number, repo, remote and branch):
//!
//! ```text
//! gh pr view <N> [--repo <owner>/<name>] --json state,mergedAt     passes when state is MERGED
//! gh run view <N> [--repo <owner>/<name>] --json conclusion,status passes when status is completed
//! git ls-remote --heads <remote> <branch>                          passes when the branch exists
//! ```
//!
//! A lane is told once per (role, event, wake): the ledger keys on the event text and the wake time,
//! not the line number, because a HANDOFF is rewritten at every task boundary and the line moves.

use crate::waits::{self, Wait};
use chrono::{DateTime, Utc};
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a check may run before it is killed.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// The ledger keeps this many of the most recent keys.
const LEDGER_CAP: usize = 500;

/// A check that is in the allowlist, with its arguments already validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    PrMerged { repo: Option<String>, number: u64 },
    RunCompleted { repo: Option<String>, number: u64 },
    BranchExists { remote: String, branch: String },
}

impl Check {
    /// The program and arguments, for a direct exec.
    pub fn argv(&self) -> Vec<String> {
        let s = |x: &str| x.to_string();
        match self {
            Check::PrMerged { repo, number } => {
                let mut v = vec![s("gh"), s("pr"), s("view"), number.to_string()];
                if let Some(r) = repo {
                    v.extend([s("--repo"), r.clone()]);
                }
                v.extend([s("--json"), s("state,mergedAt")]);
                v
            }
            Check::RunCompleted { repo, number } => {
                let mut v = vec![s("gh"), s("run"), s("view"), number.to_string()];
                if let Some(r) = repo {
                    v.extend([s("--repo"), r.clone()]);
                }
                v.extend([s("--json"), s("conclusion,status")]);
                v
            }
            Check::BranchExists { remote, branch } => {
                vec![
                    s("git"),
                    s("ls-remote"),
                    s("--heads"),
                    remote.clone(),
                    branch.clone(),
                ]
            }
        }
    }

    /// Whether the check's output says the event has happened.
    pub fn passed(&self, stdout: &str) -> Result<bool, String> {
        let field = |name: &str| -> Result<Option<String>, String> {
            let v: serde_json::Value =
                serde_json::from_str(stdout).map_err(|e| format!("output is not JSON: {e}"))?;
            Ok(v.get(name).and_then(|x| x.as_str()).map(str::to_string))
        };
        match self {
            Check::PrMerged { .. } => Ok(field("state")?.as_deref() == Some("MERGED")),
            Check::RunCompleted { .. } => Ok(field("status")?.as_deref() == Some("completed")),
            Check::BranchExists { .. } => Ok(!stdout.trim().is_empty()),
        }
    }
}

/// Every token is drawn from this set; `;`, `&`, `|`, backtick, `$`, `(`, quotes, redirects, glob
/// characters and whitespace other than the single separators are all outside it.
fn token_ok(t: &str) -> bool {
    !t.is_empty()
        && t.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | ':' | '@' | '=' | '-' | ',')
        })
}

fn number(t: &str) -> Option<u64> {
    (t.len() <= 12 && t.chars().all(|c| c.is_ascii_digit()))
        .then(|| t.parse().ok())
        .flatten()
}

fn repo_ok(t: &str) -> bool {
    let part = |p: &str| {
        !p.is_empty()
            && !p.starts_with('-')
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    t.split_once('/')
        .is_some_and(|(o, n)| part(o) && part(n) && !n.contains('/'))
}

/// A remote NAME, never a URL: a URL would let a check contact any host.
fn remote_ok(t: &str) -> bool {
    !t.starts_with('-')
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

fn branch_ok(t: &str) -> bool {
    !t.starts_with('-')
        && !t.contains("..")
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'))
}

/// `Ok` when `text` is exactly one of the allowlisted forms; `Err` says why it will not be run.
pub fn parse_check(text: &str) -> Result<Check, String> {
    // Split on single spaces only: a tab, newline or any other separator is not a token boundary
    // and falls outside `token_ok`.
    let tokens: Vec<&str> = text.trim().split(' ').collect();
    if let Some(bad) = tokens.iter().find(|t| !token_ok(t)) {
        return Err(format!(
            "`{}` has characters outside the allowlist (no shell is ever involved)",
            bad.escape_debug()
        ));
    }
    let refused = || Err("not one of the allowlisted read-only forms".to_string());
    match tokens.as_slice() {
        ["gh", "pr", "view", n, rest @ ..] => {
            let (Some(number), Some(repo)) = (number(n), repo_arg(rest, "state,mergedAt")) else {
                return refused();
            };
            Ok(Check::PrMerged { repo, number })
        }
        ["gh", "run", "view", n, rest @ ..] => {
            let (Some(number), Some(repo)) = (number(n), repo_arg(rest, "conclusion,status"))
            else {
                return refused();
            };
            Ok(Check::RunCompleted { repo, number })
        }
        ["git", "ls-remote", "--heads", remote, branch]
            if remote_ok(remote) && branch_ok(branch) =>
        {
            Ok(Check::BranchExists {
                remote: (*remote).to_string(),
                branch: (*branch).to_string(),
            })
        }
        _ => refused(),
    }
}

/// `[--repo <o>/<n>] --json <fields>`: the optional repo, or `None` for a form that does not match.
fn repo_arg(rest: &[&str], fields: &str) -> Option<Option<String>> {
    match rest {
        ["--json", f] if *f == fields => Some(None),
        ["--repo", r, "--json", f] if repo_ok(r) && *f == fields => Some(Some((*r).to_string())),
        _ => None,
    }
}

/// How a check is executed. The real one is [`ProcessRunner`]; tests supply a fake.
pub trait CheckRunner {
    /// Runs `argv` (program first) in `cwd`; its stdout, or why it could not be read.
    fn run(&self, cwd: &Path, argv: &[String]) -> Result<String, String>;
}

/// Direct exec, no shell, stdin closed, killed after [`CHECK_TIMEOUT`].
pub struct ProcessRunner {
    pub timeout: Duration,
}

impl Default for ProcessRunner {
    fn default() -> Self {
        Self {
            timeout: CHECK_TIMEOUT,
        }
    }
}

impl CheckRunner for ProcessRunner {
    fn run(&self, cwd: &Path, argv: &[String]) -> Result<String, String> {
        let (prog, args) = argv.split_first().ok_or("empty command")?;
        let mut child = std::process::Command::new(prog)
            .args(args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start {prog}: {e}"))?;
        let mut out = child.stdout.take().ok_or("no stdout")?;
        // Read on a thread so a chatty child cannot fill the pipe and stall the wait below.
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = out.by_ref().take(256 * 1024).read_to_end(&mut buf);
            buf
        });
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if start.elapsed() >= self.timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("timed out after {} s", self.timeout.as_secs()));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                Err(e) => return Err(e.to_string()),
            }
        };
        let bytes = reader.join().unwrap_or_default();
        if !status.success() {
            return Err(format!("exited with {status}"));
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// Why a lane is being told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The check ran and passed.
    CheckPassed,
    /// The wake time came and the check was not true (or could not be run).
    WakeReached,
    /// The wake time came and the check is not allowlisted, so it was never run.
    Unverified,
}

impl Reason {
    pub fn slug(self) -> &'static str {
        match self {
            Reason::CheckPassed => "check-passed",
            Reason::WakeReached => "wake-reached",
            Reason::Unverified => "unverified",
        }
    }
}

/// One live role and its HANDOFF.
pub struct Subject {
    pub role: String,
    /// The HANDOFF's directory, where checks run so `gh` and `git` see the lane's repo.
    pub cwd: PathBuf,
    pub handoff: String,
}

/// A wait that is due, and what to tell the lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Due {
    pub role: String,
    pub wait: Wait,
    pub reason: Reason,
    pub detail: String,
}

impl Due {
    /// The ledger key: stable across a HANDOFF rewrite, which moves the line number.
    pub fn key(&self) -> String {
        format!(
            "{}|{}|{}",
            self.role,
            self.wait.event,
            self.wait.wake.to_rfc3339()
        )
    }

    /// The Synapse message (`C:\Projects\CLAUDE.md` section 10 `[NOTICE]` header, then notes).
    pub fn message(&self) -> String {
        format!(
            "[NOTICE] wait-due affects={} when={} action={}\nnote: waiting for: {}\nnote: {}",
            self.role,
            self.wait.wake.to_rfc3339(),
            self.reason.slug(),
            one_line(&self.wait.event),
            one_line(&self.detail)
        )
    }
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Decides every due wait of `subjects` at `now`. Malformed wait lines are skipped here (they are
/// `assert-idle`'s to refuse). Never runs text that is not allowlisted.
pub fn poll(subjects: &[Subject], now: DateTime<Utc>, runner: &dyn CheckRunner) -> Vec<Due> {
    let mut out = Vec::new();
    for s in subjects {
        for w in waits::parse(&s.handoff).into_iter().flatten() {
            let due = now >= w.wake;
            let decided = match parse_check(&w.check) {
                Err(why) => due.then(|| {
                    (
                        Reason::Unverified,
                        format!(
                            "the check was not run ({why}); look yourself: {}",
                            one_line(&w.check)
                        ),
                    )
                }),
                Ok(check) => match runner
                    .run(&s.cwd, &check.argv())
                    .and_then(|o| check.passed(&o))
                {
                    Ok(true) => Some((
                        Reason::CheckPassed,
                        format!("the check passed: {}", one_line(&w.check)),
                    )),
                    Ok(false) => due.then(|| {
                        (
                            Reason::WakeReached,
                            format!(
                                "wake time reached and the check is not true yet: {}",
                                one_line(&w.check)
                            ),
                        )
                    }),
                    Err(e) => due.then(|| {
                        (
                            Reason::WakeReached,
                            format!("wake time reached and the check could not be read ({e}): look yourself"),
                        )
                    }),
                },
            };
            if let Some((reason, detail)) = decided {
                out.push(Due {
                    role: s.role.clone(),
                    wait: w,
                    reason,
                    detail,
                });
            }
        }
    }
    out
}

/// The keys already delivered. Stored beside the lane state; a missing or unreadable file is an
/// empty ledger (a repeat message is the safe failure, a lost wait is not).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Ledger {
    sent: Vec<String>,
}

impl Ledger {
    pub fn path(state_dir: &Path) -> PathBuf {
        state_dir.join("waits-ledger.json")
    }

    pub fn load(state_dir: &Path) -> Self {
        let sent = std::fs::read_to_string(Self::path(state_dir))
            .ok()
            .and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok())
            .unwrap_or_default();
        Self { sent }
    }

    pub fn contains(&self, key: &str) -> bool {
        self.sent.iter().any(|k| k == key)
    }

    pub fn record(&mut self, key: String) {
        if !self.contains(&key) {
            self.sent.push(key);
        }
        if self.sent.len() > LEDGER_CAP {
            let drop = self.sent.len() - LEDGER_CAP;
            self.sent.drain(..drop);
        }
    }

    /// Temp file then rename, so a reader never sees half a ledger.
    pub fn save(&self, state_dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(state_dir).map_err(|e| e.to_string())?;
        let path = Self::path(state_dir);
        let tmp = path.with_extension("tmp");
        let json = serde_json::to_string(&self.sent).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, json).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// What happened to one due wait in a pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Sent,
    /// Already delivered in an earlier pass.
    Already,
    /// Delivery failed; it stays out of the ledger and is retried next pass.
    Failed(String),
    /// A dry run: nothing sent, nothing recorded.
    Dry,
}

/// Delivers each due wait not already in `ledger` through `send`, recording only the ones `send`
/// accepted. `dry_run` sends and records nothing.
pub fn deliver(
    due: &[Due],
    ledger: &mut Ledger,
    dry_run: bool,
    send: &mut dyn FnMut(&str, &str) -> Result<(), String>,
) -> Vec<(Due, Outcome)> {
    let mut seen = BTreeSet::new();
    due.iter()
        .map(|d| {
            let key = d.key();
            let outcome = if ledger.contains(&key) || !seen.insert(key.clone()) {
                Outcome::Already
            } else if dry_run {
                Outcome::Dry
            } else {
                match send(&d.role, &d.message()) {
                    Ok(()) => {
                        ledger.record(key);
                        Outcome::Sent
                    }
                    Err(e) => Outcome::Failed(e),
                }
            };
            (d.clone(), outcome)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    const NOW: &str = "2026-11-01T12:00:00Z";
    const PAST: &str = "2026-11-01T11:00:00Z";
    const FUTURE: &str = "2026-11-01T13:00:00Z";

    #[test]
    fn every_allowlisted_form_parses_and_round_trips_to_a_direct_argv() {
        let cases = [
            "gh pr view 220 --json state,mergedAt",
            "gh pr view 220 --repo ciresnave/agentlife --json state,mergedAt",
            "gh run view 4126 --json conclusion,status",
            "gh run view 4126 --repo ciresnave/agentlife --json conclusion,status",
            "git ls-remote --heads origin waits-v2",
        ];
        for c in cases {
            let parsed = parse_check(c).unwrap_or_else(|e| panic!("{c}: {e}"));
            assert_eq!(parsed.argv().join(" "), c, "argv is the text, tokenised");
        }
    }

    #[test]
    fn shell_metacharacters_are_refused_not_run() {
        // The ruling's cases (`;`, `&&`, backtick, `$(`) and the rest of the shell's alphabet, each
        // appended to an otherwise valid check, in a token of its own and glued to a token.
        for meta in [
            ";", "&&", "||", "|", "`", "$(", "$", ">", "<", "\"", "'", "\\", "*", "?", "\n", "\t",
            "&", "(", ")", "{", "}", "~", "!",
        ] {
            for line in [
                format!("gh pr view 220 --json state,mergedAt {meta} rm -rf x"),
                format!("gh pr view 220{meta} --json state,mergedAt"),
                format!("git ls-remote --heads origin main{meta}id"),
            ] {
                let err = parse_check(&line).unwrap_err();
                assert!(
                    err.contains("outside the allowlist"),
                    "{line:?} must be refused for its characters, got: {err}"
                );
            }
        }
        // And the same refusal is what the poll acts on: the runner is never called.
        struct Boom;
        impl CheckRunner for Boom {
            fn run(&self, _: &Path, _: &[String]) -> Result<String, String> {
                panic!("a refused check must never reach the runner");
            }
        }
        let subject = Subject {
            role: "x".into(),
            cwd: PathBuf::from("."),
            handoff: format!(
                "WAIT: e | check: gh pr view 1 --json state,mergedAt; echo pwned | wake: {PAST}\n"
            ),
        };
        let due = poll(&[subject], t(NOW), &Boom);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].reason, Reason::Unverified);
    }

    #[test]
    fn forms_outside_the_list_and_option_or_url_tricks_are_refused() {
        for bad in [
            "gh pr merge 220",
            "gh pr view 220",
            "gh pr view 220 --json state",
            "gh pr view abc --json state,mergedAt",
            "gh pr view 220 --repo evil --json state,mergedAt",
            "gh pr view 220 --repo -x/y --json state,mergedAt",
            "gh api repos/x/y",
            "git push origin main",
            "git ls-remote --heads https://example.com/x.git main",
            "git ls-remote --heads origin --upload-pack=evil",
            "git ls-remote --heads -x main",
            "git ls-remote --heads origin ../x",
            "git ls-remote origin main",
            "curl http://example.com",
            "",
        ] {
            assert!(parse_check(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn what_passes_is_read_from_the_output() {
        let pr = parse_check("gh pr view 1 --json state,mergedAt").unwrap();
        assert_eq!(pr.passed(r#"{"state":"MERGED","mergedAt":"x"}"#), Ok(true));
        assert_eq!(pr.passed(r#"{"state":"OPEN","mergedAt":null}"#), Ok(false));
        assert!(pr.passed("not json").is_err());
        let run = parse_check("gh run view 1 --json conclusion,status").unwrap();
        assert_eq!(
            run.passed(r#"{"status":"completed","conclusion":"success"}"#),
            Ok(true)
        );
        assert_eq!(
            run.passed(r#"{"status":"in_progress","conclusion":""}"#),
            Ok(false)
        );
        let br = parse_check("git ls-remote --heads origin b").unwrap();
        assert_eq!(br.passed("abc\trefs/heads/b\n"), Ok(true));
        assert_eq!(br.passed(""), Ok(false));
    }

    struct Fake(RefCell<Vec<String>>, Result<String, String>);
    impl CheckRunner for Fake {
        fn run(&self, _: &Path, argv: &[String]) -> Result<String, String> {
            self.0.borrow_mut().push(argv.join(" "));
            self.1.clone()
        }
    }

    fn subject(wait: &str, wake: &str) -> Subject {
        Subject {
            role: "lane".into(),
            cwd: PathBuf::from("."),
            handoff: format!("WAIT: build green | check: {wait} | wake: {wake}\n"),
        }
    }

    const PR: &str = "gh pr view 7 --json state,mergedAt";

    #[test]
    fn a_passing_check_is_due_before_its_wake_time() {
        let f = Fake(RefCell::new(vec![]), Ok(r#"{"state":"MERGED"}"#.into()));
        let due = poll(&[subject(PR, FUTURE)], t(NOW), &f);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].reason, Reason::CheckPassed);
        assert_eq!(f.0.borrow().as_slice(), [PR]);
    }

    #[test]
    fn a_false_check_is_quiet_until_the_wake_time_then_wake_reached() {
        let f = Fake(RefCell::new(vec![]), Ok(r#"{"state":"OPEN"}"#.into()));
        assert!(poll(&[subject(PR, FUTURE)], t(NOW), &f).is_empty());
        let due = poll(&[subject(PR, PAST)], t(NOW), &f);
        assert_eq!(due[0].reason, Reason::WakeReached);
        // Exactly at the wake time counts as reached.
        let due = poll(&[subject(PR, NOW)], t(NOW), &f);
        assert_eq!(due[0].reason, Reason::WakeReached);
    }

    #[test]
    fn a_check_that_cannot_be_read_is_quiet_early_and_wake_reached_late_never_a_pass() {
        let f = Fake(RefCell::new(vec![]), Err("timed out after 30 s".into()));
        assert!(poll(&[subject(PR, FUTURE)], t(NOW), &f).is_empty());
        let due = poll(&[subject(PR, PAST)], t(NOW), &f);
        assert_eq!(due[0].reason, Reason::WakeReached);
        assert!(due[0].detail.contains("timed out"), "{}", due[0].detail);
    }

    #[test]
    fn a_non_allowlisted_check_is_unverified_at_the_wake_time_and_silent_before() {
        let f = Fake(RefCell::new(vec![]), Ok(String::new()));
        let s = |w| subject("make test", w);
        assert!(poll(&[s(FUTURE)], t(NOW), &f).is_empty());
        let due = poll(&[s(PAST)], t(NOW), &f);
        assert_eq!(due[0].reason, Reason::Unverified);
        assert!(
            due[0].detail.contains("look yourself: make test"),
            "{}",
            due[0].detail
        );
        assert!(f.0.borrow().is_empty(), "nothing was run");
    }

    #[test]
    fn the_message_is_a_compact_notice_with_the_event_on_a_note_line() {
        let f = Fake(RefCell::new(vec![]), Ok(r#"{"state":"MERGED"}"#.into()));
        let due = poll(&[subject(PR, FUTURE)], t(NOW), &f);
        let m = due[0].message();
        assert!(
            m.starts_with("[NOTICE] wait-due affects=lane when=2026-11-01T13:00:00+00:00 action=check-passed\n"),
            "{m}"
        );
        assert!(m.contains("\nnote: waiting for: build green\n"), "{m}");
    }

    #[test]
    fn a_wait_is_delivered_once_failures_are_retried_and_a_dry_run_records_nothing() {
        let f = Fake(RefCell::new(vec![]), Ok(r#"{"state":"MERGED"}"#.into()));
        let due = poll(&[subject(PR, FUTURE)], t(NOW), &f);
        let mut ledger = Ledger::default();
        let sent = RefCell::new(Vec::new());
        let mut ok = |to: &str, text: &str| {
            sent.borrow_mut().push((to.to_string(), text.to_string()));
            Ok(())
        };
        // Dry run: reported, not sent, not recorded.
        let r = deliver(&due, &mut ledger, true, &mut ok);
        assert_eq!(r[0].1, Outcome::Dry);
        assert!(sent.borrow().is_empty() && ledger == Ledger::default());
        // A failed send is not recorded, so the next pass tries again.
        let mut bad = |_: &str, _: &str| Err("no peer".to_string());
        assert!(matches!(
            deliver(&due, &mut ledger, false, &mut bad)[0].1,
            Outcome::Failed(_)
        ));
        assert_eq!(ledger, Ledger::default());
        // Then it is sent once, and a repeat pass says Already.
        assert_eq!(
            deliver(&due, &mut ledger, false, &mut ok)[0].1,
            Outcome::Sent
        );
        assert_eq!(
            deliver(&due, &mut ledger, false, &mut ok)[0].1,
            Outcome::Already
        );
        assert_eq!(sent.borrow().len(), 1);
        assert_eq!(sent.borrow()[0].0, "lane");
    }

    #[test]
    fn the_ledger_survives_a_line_number_change_and_a_new_wake_is_a_new_wait() {
        let f = Fake(RefCell::new(vec![]), Ok(r#"{"state":"MERGED"}"#.into()));
        let first = poll(&[subject(PR, FUTURE)], t(NOW), &f);
        let mut moved = subject(PR, FUTURE);
        moved.handoff = format!("# rewritten\n\nmore\n{}", moved.handoff);
        let second = poll(&[moved], t(NOW), &f);
        assert_ne!(first[0].wait.line, second[0].wait.line);
        assert_eq!(
            first[0].key(),
            second[0].key(),
            "the line is not in the key"
        );
        let later = poll(&[subject(PR, "2026-11-01T14:00:00Z")], t(NOW), &f);
        assert_ne!(first[0].key(), later[0].key(), "a new wake is a new wait");
    }

    #[test]
    fn the_ledger_round_trips_on_disk_is_capped_and_a_bad_file_is_empty() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(Ledger::load(d.path()), Ledger::default());
        let mut l = Ledger::default();
        for i in 0..(LEDGER_CAP + 7) {
            l.record(format!("k{i}"));
        }
        l.save(d.path()).unwrap();
        let back = Ledger::load(d.path());
        assert_eq!(back, l);
        assert!(!back.contains("k0") && back.contains(&format!("k{}", LEDGER_CAP + 6)));
        std::fs::write(Ledger::path(d.path()), "{{{").unwrap();
        assert_eq!(Ledger::load(d.path()), Ledger::default());
    }

    #[test]
    fn the_real_runner_reads_stdout_reports_failure_and_kills_a_slow_check() {
        let r = ProcessRunner::default();
        let ok = r
            .run(Path::new("."), &["git".into(), "--version".into()])
            .unwrap();
        assert!(ok.contains("git version"), "{ok}");
        assert!(r
            .run(
                Path::new("."),
                &["git".into(), "no-such-subcommand-xyz".into()]
            )
            .is_err());
        assert!(r
            .run(Path::new("."), &["no-such-program-xyz".into()])
            .unwrap_err()
            .contains("cannot start"));
        let slow: Vec<String> = if cfg!(windows) {
            ["ping", "-n", "6", "127.0.0.1"].map(String::from).to_vec()
        } else {
            ["sleep", "5"].map(String::from).to_vec()
        };
        let fast = ProcessRunner {
            timeout: Duration::from_millis(200),
        };
        let began = Instant::now();
        let err = fast.run(Path::new("."), &slow).unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert!(
            began.elapsed() < Duration::from_secs(4),
            "killed, not waited for"
        );
    }
}
