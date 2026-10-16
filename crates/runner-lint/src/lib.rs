// SPDX-License-Identifier: MIT OR Apache-2.0

//! The self-hosted runner lint (agentlife `docs/RUNNER-SETUP.md` sections 1 and 6).
//!
//! A job that can run on our own machine must not be reachable from an untrusted pull request. For each job
//! whose `runs-on` is, contains, or cannot be shown not to be a self-hosted runner, the workflow's triggers
//! decide: `push`, `schedule` and `workflow_dispatch` pass; `pull_request` and `pull_request_target` pass only
//! behind BOTH the same-repo condition and an `environment:`; anything else fails. It fails closed: what it
//! cannot read, it refuses. It is a lint, not a sandbox: it reads the file, not what GitHub will do with it, and
//! it cannot see that the environment has a required reviewer.

use saphyr::{LoadableYamlNode, Yaml};

/// One policy break: which file and job, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub file: String,
    /// The job id, or `-` for a whole-file problem.
    pub job: String,
    pub reason: String,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: job {}: {}", self.file, self.job, self.reason)
    }
}

/// What `lint` saw, so a caller can print a count beside "no findings".
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub findings: Vec<Finding>,
    /// Jobs examined.
    pub jobs: usize,
    /// Jobs that can run on the runner (a subset of `jobs`).
    pub runner_jobs: usize,
}

/// The only condition that makes a same-repo PR job acceptable.
pub const SAME_REPO: &str = "github.event.pull_request.head.repo.full_name == github.repository";

/// Labels that mark a runner as ours. A job asking for one can only run on a self-hosted machine.
const RUNNER_LABELS: &[&str] = &["self-hosted", "gpu"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Where {
    /// Shown to be a GitHub-hosted standard image.
    Hosted,
    /// Asks for a self-hosted label.
    Runner,
    /// Cannot be shown either way (an expression, a group, an unknown label).
    Unknown,
}

/// A standard GitHub-hosted label: `ubuntu|windows|macos` + `-latest` or `-<digits[.digits]>` + an optional
/// `-arm`, `-intel`, `-large` or `-xlarge`. Anything else (`windows-gpu`, `my-box`) is not shown to be hosted.
fn hosted_label(label: &str) -> bool {
    let l = label.to_ascii_lowercase();
    let Some((os, rest)) = l.split_once('-') else {
        return false;
    };
    if !matches!(os, "ubuntu" | "windows" | "macos") {
        return false;
    }
    let version = ["-arm", "-intel", "-large", "-xlarge"]
        .iter()
        .find_map(|s| rest.strip_suffix(s))
        .unwrap_or(rest);
    version == "latest"
        || (!version.is_empty()
            && version.chars().all(|c| c.is_ascii_digit() || c == '.')
            && !version.starts_with('.')
            && !version.ends_with('.'))
}

/// `${{ matrix.KEY }}` (any spacing) -> `KEY`.
fn matrix_key(label: &str) -> Option<&str> {
    let inner = label.trim().strip_prefix("${{")?.strip_suffix("}}")?.trim();
    let key = inner.strip_prefix("matrix.")?;
    (!key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
    .then_some(key)
}

/// Where a `${{ matrix.KEY }}` label can run: every literal value the job's own matrix gives KEY, from the
/// list under `strategy.matrix.KEY` and from each `include` entry. Not provable (so `Unknown`) when the matrix
/// is itself an expression, KEY is missing, or any value is not a plain string.
fn matrix_where(key: &str, job: &Yaml) -> Where {
    let Some(matrix) = job
        .as_mapping_get("strategy")
        .and_then(|s| s.as_mapping_get("matrix"))
    else {
        return Where::Unknown;
    };
    let mut values: Vec<&Yaml> = Vec::new();
    match matrix.as_mapping_get(key).and_then(Yaml::as_sequence) {
        Some(seq) => values.extend(seq.iter()),
        // KEY may come only from `include`.
        None if matrix.as_mapping_get(key).is_some() => return Where::Unknown,
        None => {}
    }
    if let Some(inc) = matrix.as_mapping_get("include") {
        let Some(entries) = inc.as_sequence() else {
            return Where::Unknown;
        };
        values.extend(entries.iter().filter_map(|e| e.as_mapping_get(key)));
    }
    if values.is_empty() {
        return Where::Unknown;
    }
    values
        .into_iter()
        .map(|v| match v.as_str() {
            Some(s) if !s.contains("${{") => classify_label(s, None),
            _ => Where::Unknown,
        })
        .fold(Where::Hosted, combine)
}

fn classify_label(label: &str, job: Option<&Yaml>) -> Where {
    if let (Some(key), Some(job)) = (matrix_key(label), job) {
        matrix_where(key, job)
    } else if label.contains("${{") {
        Where::Unknown
    } else if RUNNER_LABELS.contains(&label.to_ascii_lowercase().as_str()) {
        Where::Runner
    } else if hosted_label(label) {
        Where::Hosted
    } else {
        Where::Unknown
    }
}

fn combine(a: Where, b: Where) -> Where {
    match (a, b) {
        (Where::Runner, _) | (_, Where::Runner) => Where::Runner,
        (Where::Unknown, _) | (_, Where::Unknown) => Where::Unknown,
        _ => Where::Hosted,
    }
}

fn runs_on(node: &Yaml, job: &Yaml) -> Where {
    if let Some(s) = node.as_str() {
        return classify_label(s, Some(job));
    }
    if let Some(seq) = node.as_sequence() {
        return seq
            .iter()
            .map(|n| runs_on(n, job))
            .fold(Where::Hosted, combine)
            // An empty list matches nothing we can name.
            .max_unknown(seq.is_empty());
    }
    if let Some(map) = node.as_mapping() {
        // `group:` names a runner group, which may be ours: never provably hosted.
        let mut w = Where::Hosted;
        let mut saw_labels = false;
        for (k, v) in map {
            match k.as_str() {
                Some("labels") => {
                    saw_labels = true;
                    w = combine(w, runs_on(v, job));
                }
                Some("group") => w = combine(w, Where::Unknown),
                _ => {}
            }
        }
        return if saw_labels || w != Where::Hosted {
            w
        } else {
            Where::Unknown
        };
    }
    Where::Unknown
}

trait MaxUnknown {
    fn max_unknown(self, cond: bool) -> Self;
}
impl MaxUnknown for Where {
    fn max_unknown(self, cond: bool) -> Self {
        if cond {
            combine(self, Where::Unknown)
        } else {
            self
        }
    }
}

/// The trigger names of a workflow, in all three YAML shapes. `None` when `on:` is missing or unreadable.
fn triggers(doc: &Yaml) -> Option<Vec<String>> {
    // YAML 1.2: the key `on` is the string "on". (A YAML 1.1 reader makes it the boolean `true`.)
    let on = doc.as_mapping_get("on")?;
    if let Some(s) = on.as_str() {
        return Some(vec![s.to_string()]);
    }
    if let Some(seq) = on.as_sequence() {
        return seq.iter().map(|n| n.as_str().map(str::to_string)).collect();
    }
    if let Some(map) = on.as_mapping() {
        return map.keys().map(|k| k.as_str().map(str::to_string)).collect();
    }
    None
}

/// Whether `cond` is the same-repo condition, alone or as one `&&` term. Any `||`, `!` or parenthesis refuses:
/// a condition that can be satisfied another way is not a gate.
pub fn is_same_repo_gate(cond: &str) -> bool {
    let c = cond.trim();
    let c = c
        .strip_prefix("${{")
        .and_then(|r| r.strip_suffix("}}"))
        .unwrap_or(c);
    if c.contains("||") || c.contains('!') || c.contains('(') || c.contains(')') {
        return false;
    }
    let swapped = "github.repository == github.event.pull_request.head.repo.full_name";
    c.split("&&").any(|t| {
        let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
        t == SAME_REPO || t == swapped
    })
}

fn job_has_environment(job: &Yaml) -> bool {
    match job.as_mapping_get("environment") {
        Some(e) => {
            e.as_str().is_some_and(|s| !s.trim().is_empty())
                || e.as_mapping_get("name")
                    .and_then(Yaml::as_str)
                    .is_some_and(|s| !s.trim().is_empty())
        }
        None => false,
    }
}

fn job_if(job: &Yaml) -> Option<String> {
    let n = job.as_mapping_get("if")?;
    // `if: true` is a boolean, not a gate; only a string can be one.
    n.as_str().map(str::to_string)
}

/// Which kind of repo the workflow lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// A public repo (the default, and the only safe assumption): a pull-request job on a runner needs the
    /// same-repo condition AND an `environment:`.
    #[default]
    Public,
    /// A private repo, whose PRs all come from inside it (it takes no outside forks; keep the fork-PR setting
    /// off) and which on a Free plan cannot have an environment with a required reviewer. A pull-request job
    /// on a runner needs the same-repo condition only. The caller must have PROVEN the repo private
    /// (`runner-lint --private` does, with `gh api`): a public repo linted as private is a hole.
    Private,
}

/// Lints one workflow file's text as a public repo's. `file` is only a label for the findings.
pub fn lint(file: &str, text: &str) -> Report {
    lint_as(file, text, Mode::Public)
}

/// Lints one workflow file's text as `mode`'s kind of repo.
pub fn lint_as(file: &str, text: &str, mode: Mode) -> Report {
    let mut report = Report::default();
    let whole_file = |reason: String| Report {
        findings: vec![Finding {
            file: file.to_string(),
            job: "-".to_string(),
            reason,
        }],
        ..Report::default()
    };
    let docs = match Yaml::load_from_str(text) {
        Ok(d) => d,
        Err(e) => {
            return whole_file(format!(
                "does not parse as YAML, so it cannot be shown safe: {e}"
            ))
        }
    };
    let Some(doc) = docs.first() else {
        return whole_file("is empty".into());
    };
    let Some(jobs) = doc.as_mapping_get("jobs").and_then(Yaml::as_mapping) else {
        // A composite-action file or a non-workflow YAML has no jobs: nothing can run on a runner.
        return report;
    };
    let trig = triggers(doc);
    // (job, reason) pairs computed after the borrow of `push` ends.
    let mut findings: Vec<(String, String)> = Vec::new();
    let mut count = 0usize;
    let mut runner_jobs = 0usize;
    for (id, job) in jobs {
        let id = id.as_str().unwrap_or("?");
        count += 1;
        let where_ = match job.as_mapping_get("runs-on") {
            Some(r) => runs_on(r, job),
            // A job-level `uses:` calls a reusable workflow, whose own runs-on is not in this file.
            None if job.as_mapping_get("uses").is_some() => Where::Unknown,
            None => Where::Unknown,
        };
        if where_ == Where::Hosted {
            continue;
        }
        runner_jobs += 1;
        let what = if where_ == Where::Runner {
            "runs on a self-hosted label"
        } else {
            "cannot be shown to run on a GitHub-hosted image (expression, group, unknown label or reusable workflow)"
        };
        let Some(trig) = &trig else {
            findings.push((
                id.into(),
                format!("{what}, and the workflow's `on:` cannot be read"),
            ));
            continue;
        };
        let mut pr_like = false;
        for t in trig {
            match t.as_str() {
                "push" | "schedule" | "workflow_dispatch" => {}
                "pull_request" | "pull_request_target" => pr_like = true,
                other => findings.push((
                    id.into(),
                    format!("{what}, and trigger `{other}` is not push, schedule, workflow_dispatch or a gated pull_request"),
                )),
            }
        }
        if pr_like {
            let gate = job_if(job).is_some_and(|c| is_same_repo_gate(&c));
            let env = job_has_environment(job);
            let mut missing = Vec::new();
            if !gate {
                missing.push(format!(
                    "`if: {SAME_REPO}` (alone or as one && term, no || ! or parentheses)"
                ));
            }
            if !env && mode == Mode::Public {
                missing.push("an `environment:` (with a required reviewer)".to_string());
            }
            if !missing.is_empty() {
                findings.push((
                    id.into(),
                    format!(
                        "{what}, and a pull_request/pull_request_target trigger reaches it without {}",
                        missing.join(" and ")
                    ),
                ));
            }
        }
    }
    report.jobs = count;
    report.runner_jobs = runner_jobs;
    for (job, reason) in findings {
        report.findings.push(Finding {
            file: file.to_string(),
            job,
            reason,
        });
    }
    report
}

#[cfg(test)]
mod tests;
