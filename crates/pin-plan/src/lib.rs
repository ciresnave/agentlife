// SPDX-License-Identifier: MIT OR Apache-2.0

//! The pure half of the SUP-008 pin planner: finding `uses:` references in workflow text,
//! classifying each ref, choosing a release tag, and reading an action's declared runtime. Nothing
//! here touches git, the network or the disk, so every rule is tested without them.

/// How a `uses:` reference is pinned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// `./path`: a file in the same repo; nothing to pin.
    Local,
    /// `docker://image...`: pinned by digest or not, outside this tool's scope.
    Docker,
    /// A full 40-hex commit SHA: immutable.
    Pinned,
    /// A tag (`v4`, `v1.2.3`, `1.0`): a tag can be moved.
    MutableTag,
    /// A branch name (`main`, `master`, `release/v1`): moves with every push.
    MutableBranch,
    /// `owner/repo` with no `@ref` at all (invalid for a step, valid text for a diagnosis).
    Unversioned,
}

/// One `uses:` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Use {
    pub file: String,
    /// 1-based.
    pub line: usize,
    /// The value after `uses:`, comment and quotes removed.
    pub raw: String,
    pub kind: Kind,
    /// `owner/repo[/path]` before the `@`, empty for local and docker.
    pub target: String,
    /// The ref after the `@`, empty when there is none.
    pub reference: String,
}

impl Use {
    /// `(owner, repo, path-within-repo)` for a remote action or reusable workflow.
    pub fn parts(&self) -> Option<(&str, &str, &str)> {
        if matches!(self.kind, Kind::Local | Kind::Docker) {
            return None;
        }
        let mut it = self.target.splitn(3, '/');
        let owner = it.next()?;
        let repo = it.next()?;
        if owner.is_empty() || repo.is_empty() {
            return None;
        }
        Some((owner, repo, it.next().unwrap_or("")))
    }

    /// A reusable workflow (`.../x.yml@ref`) has no `runs.using`.
    pub fn is_reusable_workflow(&self) -> bool {
        self.parts()
            .is_some_and(|(_, _, p)| p.ends_with(".yml") || p.ends_with(".yaml"))
    }

    /// Does this need a pin proposal?
    pub fn is_mutable(&self) -> bool {
        matches!(
            self.kind,
            Kind::MutableTag | Kind::MutableBranch | Kind::Unversioned
        )
    }
}

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Classifies the text after `uses:`. Returns `(kind, target, ref)`.
pub fn classify(value: &str) -> (Kind, String, String) {
    if value.starts_with("./") || value == "." {
        return (Kind::Local, String::new(), String::new());
    }
    if value.starts_with("docker://") {
        return (Kind::Docker, String::new(), String::new());
    }
    match value.rsplit_once('@') {
        None => (Kind::Unversioned, value.to_string(), String::new()),
        Some((target, r)) => {
            let kind = if is_sha(r) {
                Kind::Pinned
            } else if r
                .strip_prefix('v')
                .unwrap_or(r)
                .starts_with(|c: char| c.is_ascii_digit())
            {
                Kind::MutableTag
            } else {
                Kind::MutableBranch
            };
            (kind, target.to_string(), r.to_string())
        }
    }
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The value of a `uses:` line without its trailing comment or quotes, or `None` when the line is
/// not a `uses:` key.
fn uses_value(line: &str) -> Option<String> {
    let t = line.trim_start();
    let t = t.strip_prefix("- ").map(str::trim_start).unwrap_or(t);
    let rest = t.strip_prefix("uses:")?.trim();
    if rest.is_empty() {
        return None;
    }
    let v = if let Some(q) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') {
        let inner = &rest[1..];
        inner.split(q).next().unwrap_or("").to_string()
    } else {
        rest.split(" #").next().unwrap_or("").trim().to_string()
    };
    (!v.is_empty()).then_some(v)
}

/// Every `uses:` in `text`. Lines inside a block scalar (`run: |`, `script: >`) are skipped, so
/// shell text that happens to contain `uses:` is not read as a reference.
pub fn scan(file: &str, text: &str) -> Vec<Use> {
    let mut out = Vec::new();
    let mut block_indent: Option<usize> = None;
    for (i, line) in text.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        if let Some(bi) = block_indent {
            if indent_of(line) > bi {
                continue;
            }
            block_indent = None;
        }
        let t = line.trim_start();
        if t.starts_with('#') {
            continue;
        }
        if let Some(v) = uses_value(line) {
            let (kind, target, reference) = classify(&v);
            out.push(Use {
                file: file.to_string(),
                line: i + 1,
                raw: v,
                kind,
                target,
                reference,
            });
            continue;
        }
        let body = t.split(" #").next().unwrap_or(t).trim_end();
        let body = body.strip_prefix("- ").map(str::trim_start).unwrap_or(body);
        if body.ends_with(": |")
            || body.ends_with(": >")
            || body.ends_with(": |-")
            || body.ends_with(": >-")
            || body.ends_with(": |+")
            || body.ends_with(": >+")
        {
            // The key's own indent, counting a leading `- `.
            let lead = indent_of(line) + if t.starts_with("- ") { 2 } else { 0 };
            block_indent = Some(lead);
        }
    }
    out
}

/// `line` (no newline; a trailing CR is kept) with `u`'s `target@ref` replaced by `target@sha`
/// and the old ref written after it as a comment: `uses: a/b@<sha> # v4`. A comment the line
/// already had follows: `# v4 - old note`. `None` when the line does not carry `u.raw`.
pub fn pin_line(line: &str, u: &Use, sha: &str) -> Option<String> {
    let at = line.find(&u.raw)?;
    let (head, rest) = (&line[..at], &line[at + u.raw.len()..]);
    let (rest, cr) = match rest.strip_suffix('\r') {
        Some(r) => (r, "\r"),
        None => (rest, ""),
    };
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'');
    let tail = &rest[quote.map_or(0, char::len_utf8)..];
    let note = tail.trim().strip_prefix('#').map(str::trim);
    let q = quote.map(String::from).unwrap_or_default();
    let comment = match note {
        Some(n) if !n.is_empty() => format!("# {} - {n}", u.reference),
        _ => format!("# {}", u.reference),
    };
    Some(format!("{head}{}@{sha}{q} {comment}{cr}", u.target))
}

/// A unified diff (`git apply` takes it) for `path`, turning each 1-based line in `edits` into the
/// new text given (no newline). Three lines of context; hunks that touch or overlap are merged.
/// Empty when `edits` is empty. An edit replaces one line with one line, so both hunk sides have
/// equal length.
pub fn unified_diff(
    path: &str,
    old: &str,
    edits: &std::collections::BTreeMap<usize, String>,
) -> String {
    const CTX: usize = 3;
    let lines: Vec<&str> = old.split_inclusive('\n').collect();
    let mut hunks: Vec<(usize, usize)> = Vec::new();
    for &e in edits.keys().filter(|e| **e >= 1 && **e <= lines.len()) {
        let (s, t) = (e.saturating_sub(CTX).max(1), (e + CTX).min(lines.len()));
        match hunks.last_mut() {
            Some(h) if s <= h.1 + 1 => h.1 = t,
            _ => hunks.push((s, t)),
        }
    }
    if hunks.is_empty() {
        return String::new();
    }
    let mut out = format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n");
    let emit = |out: &mut String, tag: char, text: &str| {
        out.push(tag);
        out.push_str(text);
        if !text.ends_with('\n') {
            out.push_str("\n\\ No newline at end of file\n");
        }
    };
    for (s, t) in hunks {
        let n = t - s + 1;
        out.push_str(&format!("@@ -{s},{n} +{s},{n} @@\n"));
        for no in s..=t {
            let l = lines[no - 1];
            match edits.get(&no) {
                Some(new) => {
                    emit(&mut out, '-', l);
                    let nl = if l.ends_with('\n') { "\n" } else { "" };
                    emit(&mut out, '+', &format!("{new}{nl}"));
                }
                None => emit(&mut out, ' ', l),
            }
        }
    }
    out
}

/// The `runs.using` value of an `action.yml` (`node20`, `composite`, `docker`...), or `None`.
pub fn runs_using(action_yml: &str) -> Option<String> {
    let mut in_runs = false;
    for line in action_yml.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if indent_of(line) == 0 {
            in_runs = line.trim_end().trim_end_matches(':') == "runs" && line.contains(':');
            continue;
        }
        if in_runs {
            if let Some(v) = line.trim_start().strip_prefix("using:") {
                let v = v
                    .split(" #")
                    .next()
                    .unwrap_or("")
                    .trim()
                    .trim_matches(['"', '\'']);
                return (!v.is_empty()).then(|| v.to_string());
            }
        }
    }
    None
}

/// Does this runtime deserve a flag? `node20` and anything older: GitHub is retiring them.
pub fn runtime_needs_attention(using: &str) -> bool {
    using
        .strip_prefix("node")
        .and_then(|n| n.parse::<u32>().ok())
        .is_some_and(|n| n <= 20)
}

/// The highest plain-semver tag (`v1.2.3` or `1.2.3`, optionally two components) in `names`;
/// pre-release tags (`-rc1`) and non-version names are ignored. Used only when a repo has no
/// published release.
pub fn pick_latest_tag(names: &[String]) -> Option<String> {
    names
        .iter()
        .filter_map(|n| {
            let core = n.strip_prefix('v').unwrap_or(n);
            let parts: Vec<u64> = core
                .split('.')
                .map(|p| p.parse::<u64>().ok())
                .collect::<Option<_>>()?;
            (1..=4).contains(&parts.len()).then(|| (parts, n.clone()))
        })
        .max_by(|a, b| {
            let pad = |v: &Vec<u64>| {
                let mut v = v.clone();
                v.resize(4, 0);
                v
            };
            pad(&a.0).cmp(&pad(&b.0))
        })
        .map(|(_, n)| n)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "11bd71901bbe5b1630ceea73d27597364c9af683";

    #[test]
    fn classification_covers_every_shape() {
        assert_eq!(classify("./.github/actions/x").0, Kind::Local);
        assert_eq!(classify("docker://alpine:3.19").0, Kind::Docker);
        assert_eq!(classify(&format!("actions/checkout@{SHA}")).0, Kind::Pinned);
        assert_eq!(classify("actions/checkout@v4").0, Kind::MutableTag);
        assert_eq!(classify("actions/checkout@4.1.0").0, Kind::MutableTag);
        assert_eq!(
            classify("dtolnay/rust-toolchain@stable").0,
            Kind::MutableBranch
        );
        assert_eq!(classify("a/b@main").0, Kind::MutableBranch);
        assert_eq!(classify("a/b").0, Kind::Unversioned);
        // 39 and 41 hex are not a SHA (what they are read as does not matter, only that
        // neither is trusted as immutable).
        assert_ne!(classify(&format!("a/b@{}", &SHA[1..])).0, Kind::Pinned);
        assert_ne!(classify(&format!("a/b@{SHA}0")).0, Kind::Pinned);
        let (_, t, r) = classify("github/codeql-action/analyze@v3");
        assert_eq!(
            (t.as_str(), r.as_str()),
            ("github/codeql-action/analyze", "v3")
        );
    }

    #[test]
    fn scan_reads_steps_comments_quotes_and_skips_block_scalars() {
        let text = format!(
            "jobs:\n  b:\n    steps:\n      - uses: actions/checkout@v4\n      - name: x\n        uses: \"dtolnay/rust-toolchain@stable\" # note\n      - uses: actions/cache@{SHA} # v4.2.0\n      # - uses: commented/out@v1\n      - run: |\n          echo uses: not/a-step@v1\n          uses: also/not@v2\n      - uses: ./local\n      - uses: org/repo/.github/workflows/w.yml@main\n"
        );
        let u = scan("w.yml", &text);
        let got: Vec<(&str, usize, &Kind)> = u
            .iter()
            .map(|x| (x.raw.as_str(), x.line, &x.kind))
            .collect();
        assert_eq!(
            got,
            vec![
                ("actions/checkout@v4", 4, &Kind::MutableTag),
                ("dtolnay/rust-toolchain@stable", 6, &Kind::MutableBranch),
                (&*format!("actions/cache@{SHA}"), 7, &Kind::Pinned),
                ("./local", 12, &Kind::Local),
                (
                    "org/repo/.github/workflows/w.yml@main",
                    13,
                    &Kind::MutableBranch
                ),
            ]
        );
        assert!(u[4].is_reusable_workflow());
        assert!(!u[0].is_reusable_workflow());
        assert_eq!(u[0].parts(), Some(("actions", "checkout", "")));
        assert_eq!(
            u[4].parts(),
            Some(("org", "repo", ".github/workflows/w.yml"))
        );
        assert_eq!(u[3].parts(), None);
    }

    #[test]
    fn crlf_text_scans_the_same() {
        let a = scan("w", "steps:\n  - uses: a/b@v1\n  - uses: c/d@main\n");
        let b = scan("w", "steps:\r\n  - uses: a/b@v1\r\n  - uses: c/d@main\r\n");
        assert_eq!(a, b);
        assert_eq!(a.len(), 2);
    }

    #[test]
    fn runs_using_reads_only_the_runs_block() {
        let y = "name: x\ninputs:\n  using:\n    description: not this\nruns:\n  using: 'node20'\n  main: dist/index.js\n";
        assert_eq!(runs_using(y).as_deref(), Some("node20"));
        assert_eq!(
            runs_using("runs:\n  using: composite # c\n").as_deref(),
            Some("composite")
        );
        assert_eq!(runs_using("name: x\n"), None);
        // `using:` outside `runs:` alone is not it.
        assert_eq!(runs_using("inputs:\n  using: node20\n"), None);
    }

    #[test]
    fn only_node20_and_older_need_attention() {
        for (u, want) in [
            ("node12", true),
            ("node16", true),
            ("node20", true),
            ("node24", false),
            ("composite", false),
            ("docker", false),
        ] {
            assert_eq!(runtime_needs_attention(u), want, "{u}");
        }
    }

    #[test]
    fn the_latest_tag_is_the_highest_plain_version() {
        let n = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            pick_latest_tag(&n(&[
                "v1.9.0",
                "v1.10.0",
                "v1.2.0",
                "v2.0.0-rc1",
                "latest",
                "v1"
            ])),
            Some("v1.10.0".into())
        );
        assert_eq!(pick_latest_tag(&n(&["nightly", "v1-beta"])), None);
        // v3 vs v3.0.1: the longer one is the same or newer.
        assert_eq!(
            pick_latest_tag(&n(&["v3", "v3.0.1"])),
            Some("v3.0.1".into())
        );
    }
    fn apply(old: &str, edits: &std::collections::BTreeMap<usize, String>) -> String {
        old.split_inclusive('\n')
            .enumerate()
            .map(|(i, l)| match edits.get(&(i + 1)) {
                Some(n) => format!("{n}{}", if l.ends_with('\n') { "\n" } else { "" }),
                None => l.to_string(),
            })
            .collect()
    }

    fn pin_all(text: &str) -> std::collections::BTreeMap<usize, String> {
        let mut edits = std::collections::BTreeMap::new();
        let lines: Vec<&str> = text.split('\n').collect();
        for u in scan("w.yml", text).iter().filter(|u| u.is_mutable()) {
            if let Some(n) = pin_line(lines[u.line - 1], u, SHA) {
                edits.insert(u.line, n);
            }
        }
        edits
    }

    fn mutable_count(text: &str) -> usize {
        scan("w.yml", text)
            .iter()
            .filter(|u| u.is_mutable())
            .count()
    }

    #[test]
    fn pin_line_pins_to_the_sha_and_comments_the_old_ref() {
        let u = |l: &str| scan("w", l).remove(0);
        for (line, want) in [
            (
                "      - uses: actions/checkout@v4",
                format!("      - uses: actions/checkout@{SHA} # v4"),
            ),
            (
                "        uses: a/b@main # note",
                format!("        uses: a/b@{SHA} # main - note"),
            ),
            (
                "  uses: \"a/b@v1\" # n",
                format!("  uses: \"a/b@{SHA}\" # v1 - n"),
            ),
            ("  uses: 'a/b@v1'", format!("  uses: 'a/b@{SHA}' # v1")),
            ("  uses: a/b@v1\r", format!("  uses: a/b@{SHA} # v1\r")),
            ("  uses: a/b/c@v1 #", format!("  uses: a/b/c@{SHA} # v1")),
        ] {
            let got = pin_line(line, &u(line), SHA).unwrap();
            assert_eq!(got, want, "{line}");
            // What was written reads back as pinned, to the same target.
            let back = u(got.trim_end_matches('\r'));
            assert_eq!((back.kind, back.target), (Kind::Pinned, u(line).target));
        }
        assert_eq!(pin_line("nothing here", &u("uses: a/b@v1"), SHA), None);
    }

    #[test]
    fn the_diff_pins_every_mutable_ref_and_nothing_else() {
        let text = "a: 1\nsteps:\n  - uses: x/y@v1\n  - run: ls\n  - uses: p/q@main\nz: 2\nlast: 3\n  - uses: far/away@v9\nend";
        let edits = pin_all(text);
        assert_eq!(edits.len(), 3);
        let d = unified_diff("w.yml", text, &edits);
        assert!(d.starts_with("diff --git a/w.yml b/w.yml\n--- a/w.yml\n+++ b/w.yml\n"));
        // Edits on lines 3, 5 and 8 all lie within context of each other: one hunk.
        assert_eq!(d.matches("\n@@ ").count(), 1, "{d}");
        assert_eq!(d.matches("\n-  - uses:").count(), 3, "{d}");
        assert_eq!(d.matches("\n+  - uses:").count(), 3, "{d}");
        assert_eq!(mutable_count(text), 3);
        assert_eq!(mutable_count(&apply(text, &edits)), 0);
        assert_eq!(unified_diff("w.yml", text, &Default::default()), "");
    }

    #[test]
    fn the_diff_marks_a_missing_final_newline_and_keeps_crlf() {
        let text = "s:\r\n  - uses: x/y@v1";
        let d = unified_diff("w.yml", text, &pin_all(text));
        let want =
            format!("-  - uses: x/y@v1\n\\ No newline at end of file\n+  - uses: x/y@{SHA} # v1\n\\ No newline");
        assert!(d.contains(&want), "{d:?}");
        assert!(d.contains(" s:\r\n"), "{d:?}");
    }

    #[test]
    fn distant_edits_make_separate_hunks() {
        let mut text = String::from("  - uses: a/b@v1\n");
        text.push_str(&"k: v\n".repeat(20));
        text.push_str("  - uses: c/d@v2\n");
        let d = unified_diff("w.yml", &text, &pin_all(&text));
        assert!(d.contains("@@ -1,4 +1,4 @@"), "{d}");
        assert!(d.contains("@@ -19,4 +19,4 @@"), "{d}");
        assert_eq!(d.matches("@@ -").count(), 2, "{d}");
    }
}
