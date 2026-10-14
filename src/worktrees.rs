// SPDX-License-Identifier: MIT OR Apache-2.0
//! `agentlife worktrees --stale`: which registered git worktrees belong to a branch that has
//! already been merged, so they can be removed. **Read-only: it never removes anything**; it
//! prints the `git worktree remove` lines for a person to run.
//!
//! The predicate is `C:\Projects\CLAUDE.md` section 4, because the repos squash-merge and
//! `git branch --merged` is wrong about them:
//!
//! * a worktree is **stale** only if its branch has a MERGED pull request whose head commit is
//!   the worktree's own HEAD (so a branch reused after its merge is not stale), no OPEN pull
//!   request, the remote branch is absent (`git ls-remote --heads origin <branch>` returns
//!   nothing), and the worktree is clean (section 3: uncommitted files mean ask, never force);
//! * anything the tool could not read is **unknown**, never "not stale" and never stale.
//!
//! Enumeration is from git itself (`git worktree list --porcelain` in each repo), never by
//! walking the disk for worktrees.

use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Runs a program in a directory and returns its stdout, or why it failed. A trait so the
/// classification is testable without git or gh.
pub trait Runner {
    fn run(&self, program: &str, args: &[&str], cwd: &Path) -> Result<String, String>;
}

pub struct RealRunner;

impl Runner for RealRunner {
    fn run(&self, program: &str, args: &[&str], cwd: &Path) -> Result<String, String> {
        let out = std::process::Command::new(program)
            .args(args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("{program}: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "{program} {}: {} {}",
                args.join(" "),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Merged, same head, remote branch gone, clean: safe to remove.
    Stale,
    /// As `Stale` but with uncommitted files: ask, never force.
    StaleDirty,
    /// Merged, but the remote branch still exists (CLAUDE.md section 3 deletes it at merge).
    MergedRemoteBranchPresent,
    /// The branch has a merged PR but the worktree's HEAD is not that PR's head.
    MergedThenDiverged,
    OpenPr,
    /// No merged PR for the branch (none at all, or only closed ones).
    Unmerged,
    /// Detached HEAD, bare, or no branch: nothing to look a PR up by.
    NoBranch,
    /// Registered but the directory is gone: `git worktree prune`.
    Missing,
    /// A command failed; the reason is in `why`.
    Unknown,
}

impl Verdict {
    fn label(&self) -> &'static str {
        match self {
            Verdict::Stale => "stale",
            Verdict::StaleDirty => "stale-dirty",
            Verdict::MergedRemoteBranchPresent => "merged-remote-branch-present",
            Verdict::MergedThenDiverged => "merged-then-diverged",
            Verdict::OpenPr => "open-pr",
            Verdict::Unmerged => "unmerged",
            Verdict::NoBranch => "no-branch",
            Verdict::Missing => "missing",
            Verdict::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Row {
    pub repo: String,
    pub path: String,
    pub branch: Option<String>,
    pub verdict: Verdict,
    pub why: String,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    /// How the repos were chosen (`--repo` or the children of `--root`).
    pub enumerated_by: String,
    pub repos_scanned: usize,
    /// Registered worktrees seen, main worktrees excluded.
    pub worktrees: usize,
    pub rows: Vec<Row>,
    /// Repos whose `git worktree list` itself failed (they contribute no rows).
    pub repo_errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub bare: bool,
    pub prunable: bool,
}

/// `git worktree list --porcelain`: blank-line separated blocks. The first is the main worktree.
pub fn parse_porcelain(text: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    for block in text.replace("\r\n", "\n").split("\n\n") {
        let mut e = Entry {
            path: String::new(),
            head: None,
            branch: None,
            bare: false,
            prunable: false,
        };
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                e.path = p.to_string();
            } else if let Some(h) = line.strip_prefix("HEAD ") {
                e.head = Some(h.to_string());
            } else if let Some(b) = line.strip_prefix("branch ") {
                e.branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
            } else if line == "bare" {
                e.bare = true;
            } else if line.starts_with("prunable") {
                e.prunable = true;
            }
        }
        if !e.path.is_empty() {
            out.push(e);
        }
    }
    out
}

/// `owner/name` from a github remote URL (https or ssh), or `None`.
pub fn nwo_of(url: &str) -> Option<String> {
    let t = url.trim().trim_end_matches('/');
    let t = t.strip_suffix(".git").unwrap_or(t);
    let rest = t
        .split_once("github.com/")
        .or_else(|| t.split_once("github.com:"))?
        .1;
    let mut parts = rest.split('/');
    let (o, n) = (parts.next()?, parts.next()?);
    if o.is_empty() || n.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(format!("{o}/{n}"))
}

fn unknown(repo: &str, e: &Entry, why: String) -> Row {
    Row {
        repo: repo.to_string(),
        path: e.path.clone(),
        branch: e.branch.clone(),
        verdict: Verdict::Unknown,
        why,
    }
}

/// Classify one non-main worktree of the repo at `repo` (whose GitHub name is `nwo`).
pub fn classify(runner: &dyn Runner, repo: &Path, nwo: &str, e: &Entry) -> Row {
    let repo_s = repo.display().to_string();
    let row = |verdict: Verdict, why: String| Row {
        repo: repo_s.clone(),
        path: e.path.clone(),
        branch: e.branch.clone(),
        verdict,
        why,
    };
    if e.prunable || !Path::new(&e.path).exists() {
        return row(
            Verdict::Missing,
            "the directory is gone; `git worktree prune` clears the registration".into(),
        );
    }
    let Some(branch) = e.branch.as_deref().filter(|_| !e.bare) else {
        return row(
            Verdict::NoBranch,
            "detached or bare: no branch to look up".into(),
        );
    };
    let json = match runner.run(
        "gh",
        &[
            "pr",
            "list",
            "-R",
            nwo,
            "--head",
            branch,
            "--state",
            "all",
            "--json",
            "number,state,headRefOid",
            "--limit",
            "50",
        ],
        repo,
    ) {
        Ok(j) => j,
        Err(why) => return unknown(&repo_s, e, why),
    };
    let prs: Vec<Value> = match serde_json::from_str(&json) {
        Ok(Value::Array(a)) => a,
        _ => return unknown(&repo_s, e, "gh pr list: not a JSON array".into()),
    };
    let state = |p: &Value| p["state"].as_str().unwrap_or("").to_string();
    if let Some(p) = prs.iter().find(|p| state(p) == "OPEN") {
        return row(Verdict::OpenPr, format!("PR #{} is open", p["number"]));
    }
    let merged: Vec<&Value> = prs.iter().filter(|p| state(p) == "MERGED").collect();
    if merged.is_empty() {
        return row(
            Verdict::Unmerged,
            format!("{} PR(s) for this branch, none merged", prs.len()),
        );
    }
    let head = match &e.head {
        Some(h) => h.as_str(),
        None => return unknown(&repo_s, e, "git did not report a HEAD".into()),
    };
    let Some(m) = merged
        .iter()
        .find(|p| p["headRefOid"].as_str() == Some(head))
    else {
        return row(
            Verdict::MergedThenDiverged,
            format!(
                "HEAD {} is not the head of any merged PR for this branch (a reused branch?)",
                &head[..head.len().min(9)]
            ),
        );
    };
    let number = m["number"].to_string();
    match runner.run("git", &["ls-remote", "--heads", "origin", branch], repo) {
        Err(why) => return unknown(&repo_s, e, why),
        Ok(out) if !out.trim().is_empty() => {
            return row(
                Verdict::MergedRemoteBranchPresent,
                format!("PR #{number} is merged but origin still has the branch"),
            )
        }
        Ok(_) => {}
    }
    match runner.run("git", &["-C", &e.path, "status", "--porcelain"], repo) {
        Err(why) => unknown(&repo_s, e, why),
        Ok(out) if !out.trim().is_empty() => row(
            Verdict::StaleDirty,
            format!(
                "PR #{number} merged, branch gone, but {} uncommitted path(s): ask first",
                out.lines().count()
            ),
        ),
        Ok(_) => row(
            Verdict::Stale,
            format!("PR #{number} merged at this HEAD, remote branch absent, clean"),
        ),
    }
}

/// The repo roots to scan: explicit `--repo` paths, else the immediate children of `root` that
/// have a `.git` DIRECTORY (a worktree has a `.git` file, so it is not mistaken for a repo).
pub fn repos(explicit: &[PathBuf], root: &Path) -> (Vec<PathBuf>, String) {
    if !explicit.is_empty() {
        return (explicit.to_vec(), "--repo".into());
    }
    let mut v: Vec<PathBuf> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join(".git").is_dir())
        .collect();
    v.sort();
    (
        v,
        format!("the children of {} with a .git directory", root.display()),
    )
}

pub fn scan(runner: &dyn Runner, repos: &[PathBuf], enumerated_by: String) -> Report {
    let mut report = Report {
        enumerated_by,
        repos_scanned: repos.len(),
        ..Report::default()
    };
    for repo in repos {
        let list = match runner.run("git", &["worktree", "list", "--porcelain"], repo) {
            Ok(l) => l,
            Err(e) => {
                report.repo_errors.push(format!("{}: {e}", repo.display()));
                continue;
            }
        };
        let entries = parse_porcelain(&list);
        let nwo = runner
            .run("git", &["remote", "get-url", "origin"], repo)
            .ok()
            .and_then(|u| nwo_of(&u));
        // The first entry is the main worktree: never a candidate.
        for e in entries.iter().skip(1) {
            report.worktrees += 1;
            report.rows.push(match &nwo {
                Some(n) => classify(runner, repo, n, e),
                None => unknown(
                    &repo.display().to_string(),
                    e,
                    "origin is not a github.com remote".into(),
                ),
            });
        }
    }
    report
}

/// The text form: a count line per verdict, then one line per row, then the `git worktree remove`
/// lines for the stale ones (never run by this tool).
pub fn render(r: &Report) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "scanned {} repo(s) ({}); {} registered worktree(s), main worktrees excluded",
        r.repos_scanned, r.enumerated_by, r.worktrees
    );
    for e in &r.repo_errors {
        let _ = writeln!(s, "repo error (no rows from it): {e}");
    }
    let mut counts: Vec<(&'static str, usize)> = Vec::new();
    for row in &r.rows {
        let l = row.verdict.label();
        match counts.iter_mut().find(|(k, _)| *k == l) {
            Some((_, n)) => *n += 1,
            None => counts.push((l, 1)),
        }
    }
    counts.sort();
    let _ = writeln!(
        s,
        "{}",
        counts
            .iter()
            .map(|(k, n)| format!("{k}={n}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    for row in &r.rows {
        let _ = writeln!(
            s,
            "{:<28} {}  [{}]  {}",
            row.verdict.label(),
            row.path,
            row.branch.as_deref().unwrap_or("-"),
            row.why
        );
    }
    let stale: Vec<&Row> = r
        .rows
        .iter()
        .filter(|x| x.verdict == Verdict::Stale)
        .collect();
    if !stale.is_empty() {
        let _ = writeln!(
            s,
            "\nTo remove the {} stale worktree(s) (not run by this tool):",
            stale.len()
        );
        for row in stale {
            let _ = writeln!(
                s,
                "git -C \"{}\" worktree remove \"{}\"",
                row.repo, row.path
            );
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Answers by `program args...` key; anything absent is an error.
    struct Fake(HashMap<String, Result<String, String>>);

    impl Runner for Fake {
        fn run(&self, program: &str, args: &[&str], _cwd: &Path) -> Result<String, String> {
            let key = format!("{program} {}", args.join(" "));
            self.0
                .get(&key)
                .cloned()
                .unwrap_or_else(|| Err(format!("unscripted: {key}")))
        }
    }

    fn entry(path: &str, branch: Option<&str>, head: &str) -> Entry {
        Entry {
            path: path.into(),
            head: Some(head.into()),
            branch: branch.map(String::from),
            bare: false,
            prunable: false,
        }
    }

    const PR_KEY: &str =
        "gh pr list -R o/r --head b --state all --json number,state,headRefOid --limit 50";

    /// A script for branch `b` at `path` (a real directory so `Missing` does not fire).
    fn script(path: &Path, prs: &str, remote: &str, status: &str) -> Fake {
        let mut m = HashMap::new();
        m.insert(PR_KEY.to_string(), Ok(prs.to_string()));
        m.insert(
            "git ls-remote --heads origin b".to_string(),
            Ok(remote.to_string()),
        );
        m.insert(
            format!("git -C {} status --porcelain", path.display()),
            Ok(status.to_string()),
        );
        Fake(m)
    }

    fn verdict(prs: &str, remote: &str, status: &str) -> Verdict {
        let d = tempfile::tempdir().unwrap();
        let e = entry(&d.path().display().to_string(), Some("b"), "abc");
        classify(
            &script(d.path(), prs, remote, status),
            Path::new("repo"),
            "o/r",
            &e,
        )
        .verdict
    }

    #[test]
    fn merged_at_this_head_with_the_remote_branch_gone_and_clean_is_stale() {
        let prs = r#"[{"number":7,"state":"MERGED","headRefOid":"abc"}]"#;
        assert_eq!(verdict(prs, "", ""), Verdict::Stale);
    }

    #[test]
    fn uncommitted_files_make_it_stale_dirty_never_stale() {
        let prs = r#"[{"number":7,"state":"MERGED","headRefOid":"abc"}]"#;
        assert_eq!(verdict(prs, "", " M f.rs\n"), Verdict::StaleDirty);
    }

    #[test]
    fn a_remote_branch_still_present_is_not_stale() {
        let prs = r#"[{"number":7,"state":"MERGED","headRefOid":"abc"}]"#;
        assert_eq!(
            verdict(prs, "abc\trefs/heads/b\n", ""),
            Verdict::MergedRemoteBranchPresent
        );
    }

    #[test]
    fn a_branch_reused_after_its_merge_is_diverged_not_stale() {
        let prs = r#"[{"number":7,"state":"MERGED","headRefOid":"old"}]"#;
        assert_eq!(verdict(prs, "", ""), Verdict::MergedThenDiverged);
    }

    #[test]
    fn an_open_pr_wins_over_an_older_merged_one() {
        let prs = r#"[{"number":7,"state":"MERGED","headRefOid":"abc"},{"number":9,"state":"OPEN","headRefOid":"abc"}]"#;
        assert_eq!(verdict(prs, "", ""), Verdict::OpenPr);
    }

    #[test]
    fn no_pr_or_only_a_closed_one_is_unmerged() {
        assert_eq!(verdict("[]", "", ""), Verdict::Unmerged);
        let closed = r#"[{"number":3,"state":"CLOSED","headRefOid":"abc"}]"#;
        assert_eq!(verdict(closed, "", ""), Verdict::Unmerged);
    }

    #[test]
    fn a_failed_command_is_unknown_never_stale() {
        let d = tempfile::tempdir().unwrap();
        let e = entry(&d.path().display().to_string(), Some("b"), "abc");
        let r = classify(&Fake(HashMap::new()), Path::new("repo"), "o/r", &e);
        assert_eq!(r.verdict, Verdict::Unknown);
        assert!(r.why.contains("unscripted"), "{}", r.why);
        // gh answers but git ls-remote fails: still unknown.
        let mut m = HashMap::new();
        m.insert(
            PR_KEY.to_string(),
            Ok(r#"[{"number":7,"state":"MERGED","headRefOid":"abc"}]"#.to_string()),
        );
        let r = classify(&Fake(m), Path::new("repo"), "o/r", &e);
        assert_eq!(r.verdict, Verdict::Unknown);
    }

    #[test]
    fn a_missing_directory_and_a_detached_head_are_named_not_looked_up() {
        let gone = entry("Z:/definitely/not/here", Some("b"), "abc");
        let r = classify(&Fake(HashMap::new()), Path::new("repo"), "o/r", &gone);
        assert_eq!(r.verdict, Verdict::Missing);
        let d = tempfile::tempdir().unwrap();
        let det = entry(&d.path().display().to_string(), None, "abc");
        let r = classify(&Fake(HashMap::new()), Path::new("repo"), "o/r", &det);
        assert_eq!(r.verdict, Verdict::NoBranch);
    }

    #[test]
    fn porcelain_parses_main_branches_detached_and_prunable_and_crlf() {
        let text = "worktree C:/p/r\nHEAD aaa\nbranch refs/heads/main\n\n\
                    worktree C:/p/r-wt\r\nHEAD bbb\r\nbranch refs/heads/feat/x\r\n\r\n\
                    worktree C:/p/r-det\nHEAD ccc\ndetached\n\n\
                    worktree C:/p/r-gone\nHEAD ddd\nbranch refs/heads/old\nprunable gitdir file points to non-existent location\n";
        let v = parse_porcelain(text);
        assert_eq!(v.len(), 4);
        assert_eq!(v[1].branch.as_deref(), Some("feat/x"));
        assert_eq!(v[1].path, "C:/p/r-wt");
        assert_eq!(v[2].branch, None);
        assert!(v[3].prunable);
    }

    #[test]
    fn nwo_comes_from_https_and_ssh_remotes_only_for_github() {
        assert_eq!(
            nwo_of("https://github.com/ciresnave/agentlife.git\n").as_deref(),
            Some("ciresnave/agentlife")
        );
        assert_eq!(
            nwo_of("git@github.com:ciresnave/x.git").as_deref(),
            Some("ciresnave/x")
        );
        assert_eq!(nwo_of("https://example.com/a/b"), None);
        assert_eq!(nwo_of("https://github.com/only"), None);
    }

    #[test]
    fn scan_skips_the_main_worktree_counts_the_rest_and_prints_remove_lines_only_for_stale() {
        let d = tempfile::tempdir().unwrap();
        let wt = d.path().join("wt");
        std::fs::create_dir(&wt).unwrap();
        let list = format!(
            "worktree C:/main\nHEAD aaa\nbranch refs/heads/main\n\nworktree {}\nHEAD abc\nbranch refs/heads/b\n",
            wt.display()
        );
        let mut s = script(
            &wt,
            r#"[{"number":7,"state":"MERGED","headRefOid":"abc"}]"#,
            "",
            "",
        );
        s.0.insert("git worktree list --porcelain".into(), Ok(list));
        s.0.insert(
            "git remote get-url origin".into(),
            Ok("https://github.com/o/r.git".into()),
        );
        let rep = scan(&s, &[PathBuf::from("C:/main")], "--repo".into());
        assert_eq!(rep.repos_scanned, 1);
        assert_eq!(rep.worktrees, 1, "the main worktree is not a candidate");
        assert_eq!(rep.rows[0].verdict, Verdict::Stale);
        let text = render(&rep);
        assert!(text.contains("stale=1"), "{text}");
        assert!(text.contains("worktree remove"), "{text}");
        // A report with no stale rows prints no remove line.
        let mut none = rep;
        none.rows[0].verdict = Verdict::Unmerged;
        assert!(!render(&none).contains("worktree remove"));
    }

    #[test]
    fn a_repo_whose_listing_fails_is_named_and_contributes_no_rows() {
        let rep = scan(
            &Fake(HashMap::new()),
            &[PathBuf::from("C:/x")],
            "--repo".into(),
        );
        assert_eq!(rep.repo_errors.len(), 1);
        assert!(rep.rows.is_empty());
    }
}
