use super::*;

fn wf(on: &str, job_body: &str) -> String {
    format!("name: t\non: {on}\njobs:\n  j:\n{job_body}\n")
}

const GATE: &str = "    if: github.event.pull_request.head.repo.full_name == github.repository";
const ENV: &str = "    environment: device-tests";

fn n(text: &str) -> usize {
    lint("w.yml", text).findings.len()
}

fn self_hosted(extra: &[&str]) -> String {
    let mut s = String::from("    runs-on: [self-hosted, windows, gpu]\n");
    for e in extra {
        s.push_str(e);
        s.push('\n');
    }
    s.push_str("    steps:\n      - run: nvidia-smi");
    s
}

#[test]
fn the_three_allowed_triggers_pass_in_every_yaml_shape() {
    let job = self_hosted(&[]);
    assert_eq!(n(&wf("push", &job)), 0, "string");
    assert_eq!(
        n(&wf("[push, schedule, workflow_dispatch]", &job)),
        0,
        "list"
    );
    assert_eq!(
        n(&wf("\n  push:\n    branches: [main]\n  schedule:\n    - cron: '0 3 * * *'\n  workflow_dispatch:", &job)),
        0,
        "map"
    );
}

#[test]
fn a_pull_request_reaches_a_runner_job_only_behind_both_gates_planted_every_way() {
    for pr in [
        "pull_request",
        "pull_request_target",
        "[push, pull_request]",
        "\n  pull_request:\n    branches: [main]",
    ] {
        // Neither: fails.
        assert_eq!(n(&wf(pr, &self_hosted(&[]))), 1, "{pr}: neither");
        // Only the condition: fails.
        assert_eq!(n(&wf(pr, &self_hosted(&[GATE]))), 1, "{pr}: condition only");
        // Only the environment: fails.
        assert_eq!(
            n(&wf(pr, &self_hosted(&[ENV]))),
            1,
            "{pr}: environment only"
        );
        // Both: passes (positive control).
        assert_eq!(n(&wf(pr, &self_hosted(&[GATE, ENV]))), 0, "{pr}: both");
    }
}

#[test]
fn the_finding_names_the_file_the_job_and_what_is_missing() {
    let r = lint("ci.yml", &wf("pull_request", &self_hosted(&[GATE])));
    let f = &r.findings[0];
    assert_eq!((f.file.as_str(), f.job.as_str()), ("ci.yml", "j"));
    assert!(f.reason.contains("environment"), "{f}");
    assert!(!f.reason.contains("`if:"), "the condition was present: {f}");
}

#[test]
fn a_hosted_job_passes_under_any_trigger() {
    let hosted = "    runs-on: ubuntu-latest\n    steps:\n      - run: cargo test";
    for on in [
        "pull_request",
        "pull_request_target",
        "[push, pull_request]",
        "workflow_call",
        "issue_comment",
    ] {
        assert_eq!(n(&wf(on, hosted)), 0, "{on}");
    }
    let r = lint("w.yml", &wf("pull_request", hosted));
    assert_eq!((r.jobs, r.runner_jobs), (1, 0));
}

#[test]
fn a_label_only_the_runner_has_counts_even_without_self_hosted() {
    let gpu = "    runs-on: gpu\n    steps:\n      - run: x";
    assert_eq!(n(&wf("pull_request", gpu)), 1);
    let mapped = "    runs-on:\n      labels: [windows-latest, gpu]\n    steps:\n      - run: x";
    assert_eq!(n(&wf("pull_request", mapped)), 1);
    let upper = "    runs-on: Self-Hosted\n    steps:\n      - run: x";
    assert_eq!(n(&wf("pull_request", upper)), 1);
}

#[test]
fn what_cannot_be_shown_hosted_is_treated_as_the_runner() {
    for runs_on in [
        "${{ matrix.os }}",
        "[\"${{ matrix.os }}\"]",
        "windows-gpu",
        "my-own-box",
        "{ group: big }",
        "[]",
    ] {
        let job = format!("    runs-on: {runs_on}\n    steps:\n      - run: x");
        assert_eq!(n(&wf("pull_request", &job)), 1, "{runs_on}");
        assert_eq!(n(&wf("push", &job)), 0, "{runs_on} is fine on push");
    }
    // A job that calls a reusable workflow has no runs-on in this file.
    let called = "    uses: ./.github/workflows/dev.yml";
    assert_eq!(n(&wf("pull_request", called)), 1);
    assert_eq!(n(&wf("push", called)), 0);
}

#[test]
fn the_standard_hosted_images_are_recognised() {
    for l in [
        "ubuntu-latest",
        "ubuntu-24.04",
        "ubuntu-24.04-arm",
        "windows-2025",
        "windows-latest",
        "macos-15-intel",
        "macos-14-xlarge",
    ] {
        assert!(hosted_label(l), "{l}");
    }
    for l in [
        "windows-gpu",
        "ubuntu",
        "ubuntu-",
        "windows-.",
        "linux-latest",
        "gpu",
        "self-hosted",
        "ubuntu-latest-gpu",
    ] {
        assert!(!hosted_label(l), "{l}");
    }
}

#[test]
fn any_other_trigger_fails_for_a_runner_job_and_names_it() {
    let job = self_hosted(&[]);
    for on in [
        "workflow_call",
        "workflow_run",
        "issue_comment",
        "pull_request_review",
        "[push, issue_comment]",
        "release",
    ] {
        let r = lint("w.yml", &wf(on, &job));
        assert_eq!(r.findings.len(), 1, "{on}");
        assert!(
            r.findings[0].reason.contains("trigger"),
            "{on}: {}",
            r.findings[0]
        );
    }
}

#[test]
fn a_gate_that_can_be_satisfied_another_way_is_not_a_gate() {
    for cond in [
        "github.event.pull_request.head.repo.full_name == github.repository || github.actor == 'x'",
        "github.actor == 'x' || github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository",
        "!(github.event.pull_request.head.repo.full_name != github.repository)",
        "(github.event.pull_request.head.repo.full_name == github.repository)",
        "github.event.pull_request.head.repo.full_name != github.repository",
        "github.event.pull_request.head.repo.full_name == github.repository_owner",
        "true",
        "github.event.pull_request.head.repo.full_name",
    ] {
        let job = self_hosted(&[&format!("    if: {cond}"), ENV]);
        assert_eq!(n(&wf("pull_request", &job)), 1, "{cond}");
    }
    for cond in [
        "github.event.pull_request.head.repo.full_name == github.repository",
        "${{ github.event.pull_request.head.repo.full_name == github.repository }}",
        "github.repository == github.event.pull_request.head.repo.full_name",
        "github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository",
        "github.event.pull_request.head.repo.full_name   ==   github.repository",
    ] {
        let job = self_hosted(&[&format!("    if: {cond}"), ENV]);
        assert_eq!(n(&wf("pull_request", &job)), 0, "{cond}");
    }
    // A boolean `if:` is not a gate.
    let job = self_hosted(&["    if: true", ENV]);
    assert_eq!(n(&wf("pull_request", &job)), 1);
}

#[test]
fn an_environment_must_name_something() {
    for env in [
        "    environment: ''",
        "    environment:\n      url: https://x",
        "    environment:\n      name: ''",
    ] {
        let job = self_hosted(&[GATE, env]);
        assert_eq!(n(&wf("pull_request", &job)), 1, "{env}");
    }
    let job = self_hosted(&[GATE, "    environment:\n      name: device-tests"]);
    assert_eq!(n(&wf("pull_request", &job)), 0, "map form with a name");
}

#[test]
fn each_job_is_judged_alone() {
    let text = "name: t\non: pull_request\njobs:\n  hosted:\n    runs-on: ubuntu-latest\n    steps:\n      - run: a\n  device:\n    runs-on: [self-hosted, gpu]\n    steps:\n      - run: b\n  gated:\n    runs-on: [self-hosted, gpu]\n    if: github.event.pull_request.head.repo.full_name == github.repository\n    environment: e\n    steps:\n      - run: c\n";
    let r = lint("w.yml", text);
    assert_eq!((r.jobs, r.runner_jobs), (3, 2));
    assert_eq!(r.findings.len(), 1);
    assert_eq!(r.findings[0].job, "device");
}

#[test]
fn the_key_on_is_read_as_a_string_not_the_yaml_1_1_boolean() {
    // A YAML 1.1 reader turns `on:` into `true` and sees no triggers: this would then pass. It must fail.
    assert_eq!(n(&wf("pull_request", &self_hosted(&[]))), 1);
}

#[test]
fn a_file_that_cannot_be_read_fails_closed() {
    assert_eq!(n("jobs: [unclosed"), 1);
    assert_eq!(n(""), 1);
    // A job without readable triggers on a runner label fails; the same job when `on:` is absent too.
    let no_on = "jobs:\n  j:\n    runs-on: self-hosted\n    steps:\n      - run: x\n";
    assert_eq!(n(no_on), 1);
    // A file with no jobs (an action.yml) has nothing to run on a runner.
    assert_eq!(n("name: a\nruns:\n  using: composite\n  steps: []\n"), 0);
}

#[test]
fn a_matrix_label_is_resolved_from_the_jobs_own_matrix() {
    let job = |values: &str, extra: &str| {
        format!("    runs-on: ${{{{ matrix.os }}}}\n    strategy:\n      matrix:\n        os: {values}\n{extra}    steps:\n      - run: x")
    };
    // The repo's own CI shape: all hosted, so it passes under a pull request.
    assert_eq!(
        n(&wf(
            "pull_request",
            &job("[ubuntu-latest, windows-latest]", "")
        )),
        0
    );
    // One self-hosted value anywhere in the matrix makes the job a runner job.
    assert_eq!(
        n(&wf(
            "pull_request",
            &job("[ubuntu-latest, self-hosted]", "")
        )),
        1
    );
    assert_eq!(n(&wf("pull_request", &job("[ubuntu-latest, gpu]", ""))), 1);
    // A value added by include counts too.
    let inc = "        include:\n          - os: self-hosted\n";
    assert_eq!(n(&wf("pull_request", &job("[ubuntu-latest]", inc))), 1);
    // A matrix that is an expression, or a value that is, cannot be shown hosted.
    assert_eq!(
        n(&wf("pull_request", &job("${{ fromJSON(inputs.m) }}", ""))),
        1
    );
    assert_eq!(
        n(&wf("pull_request", &job("[\"${{ inputs.os }}\"]", ""))),
        1
    );
    // A matrix key that does not exist cannot be shown hosted.
    let missing = "    runs-on: ${{ matrix.nope }}\n    strategy:\n      matrix:\n        os: [ubuntu-latest]\n    steps:\n      - run: x";
    assert_eq!(n(&wf("pull_request", missing)), 1);
    // Any other expression still cannot.
    let other = "    runs-on: ${{ inputs.runner }}\n    steps:\n      - run: x";
    assert_eq!(n(&wf("pull_request", other)), 1);
}

#[test]
fn a_private_repo_needs_the_same_repo_condition_but_not_an_environment() {
    let private = |text: &str| lint_as("w.yml", text, Mode::Private).findings.len();
    for pr in [
        "pull_request",
        "pull_request_target",
        "[push, pull_request]",
    ] {
        // The condition alone passes in private mode and still fails in public mode.
        assert_eq!(
            private(&wf(pr, &self_hosted(&[GATE]))),
            0,
            "{pr}: private, condition only"
        );
        assert_eq!(
            n(&wf(pr, &self_hosted(&[GATE]))),
            1,
            "{pr}: public, condition only"
        );
        // Neither, or only the environment, fails in private mode too: the condition is the gate.
        assert_eq!(
            private(&wf(pr, &self_hosted(&[]))),
            1,
            "{pr}: private, neither"
        );
        assert_eq!(
            private(&wf(pr, &self_hosted(&[ENV]))),
            1,
            "{pr}: private, environment only"
        );
        // Both passes everywhere.
        assert_eq!(
            private(&wf(pr, &self_hosted(&[GATE, ENV]))),
            0,
            "{pr}: private, both"
        );
    }
    // Private mode relaxes only the environment: other triggers still fail.
    for on in ["workflow_call", "workflow_run", "issue_comment"] {
        assert_eq!(private(&wf(on, &self_hosted(&[GATE, ENV]))), 1, "{on}");
    }
    // A private repo's custom label (an existing `humboldt` runner) is a runner job: gated like any other.
    let custom = "    runs-on: [self-hosted, humboldt]\n    steps:\n      - run: x";
    assert_eq!(private(&wf("pull_request", custom)), 1);
    assert_eq!(private(&wf("push", custom)), 0);
}
