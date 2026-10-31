// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart lane-new`, slice 3 (docs/LANE-NEW.md steps 6 and 7 and the disk floor of step 0): the two
//! external commands, Synapse provisioning and `lane-restart start`, behind a [`Host`] trait the way
//! `Vcs` fronts `gh`/`git`. Every test uses a fake; **no test provisions, starts or touches a real lane**,
//! and the first real run is the PM's, on a throwaway role, dry run first.
//!
//! An exit code is never the read-back (CLAUDE.md §5). Step 6 reads the script's printed counts *and* the
//! files it should have left; step 7 reads the lane's own state file, the process table, the spec and the
//! host log. A fake that lies about any one of those must stop the run.

use std::path::{Path, PathBuf};

/// The counts line `scripts/synapse-provision-weekly.ps1` prints: `provision: ok=N created=N ...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProvisionCounts {
    pub ok: u32,
    pub created: u32,
    pub renewed: u32,
    pub refused: u32,
    pub other: u32,
}

impl ProvisionCounts {
    /// Every role line the script counted.
    pub fn total(&self) -> u32 {
        self.ok + self.created + self.renewed + self.refused + self.other
    }
}

/// Parses the counts line out of the script's output. `Err` if there is none or it is incomplete: a
/// script that printed nothing parseable asserts nothing.
pub fn parse_counts(out: &str) -> Result<ProvisionCounts, String> {
    let line = out
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("provision:"))
        .ok_or("the provisioning script printed no `provision:` counts line")?;
    let mut c = ProvisionCounts::default();
    let mut seen = 0u8;
    for kv in line["provision:".len()..].split_whitespace() {
        let Some((k, v)) = kv.split_once('=') else {
            continue;
        };
        let Ok(n) = v.parse::<u32>() else {
            return Err(format!("the counts line has a non-number: {kv:?}"));
        };
        let slot = match k {
            "ok" => &mut c.ok,
            "created" => &mut c.created,
            "renewed" => &mut c.renewed,
            "refused" => &mut c.refused,
            "other" => &mut c.other,
            _ => continue,
        };
        *slot = n;
        seen += 1;
    }
    if seen == 5 {
        Ok(c)
    } else {
        Err(format!(
            "the counts line names {seen} of the 5 counts: {line:?}"
        ))
    }
}

/// What the lane-making steps run outside this process. A real impl shells out; a fake records.
pub trait Host {
    /// The 20 GB disk rule (`disk::guard`): `Err` is the refusal text. An unreadable probe is not a refusal.
    fn disk_floor(&self, state_dir: &Path) -> Result<(), String>;
    /// Runs the provisioning script for the roles in `roles_file` only, with `-Apply`, and returns the counts
    /// it printed. `Err` if it could not run or printed no counts.
    fn provision(&self, roles_file: &Path) -> Result<ProvisionCounts, String>;
    /// Runs `lane-restart start --role R --model M --reason lane-new` and returns its exit code (0 started,
    /// 2 started and awaiting the dev-channels dialog). `Err` if it could not be run at all.
    fn start(&self, role: &str, model: &str) -> Result<i32, String>;
    /// Is `pid` a live `claude` process right now?
    fn claude_alive(&self, pid: u32) -> bool;
}

/// The real thing. Constructed only by `run_lane_new_cmd`, never by a test.
pub struct RealHost {
    pub script: PathBuf,
    pub lane_homes: PathBuf,
    pub pilot_home: PathBuf,
    /// Claude Code's config dir, for the process-table facts (`claude_config_dir` in main.rs).
    pub claude_config: PathBuf,
}

impl RealHost {
    /// The weekly script as checked out in the agentlife primary, which stays on `main`.
    pub fn new(
        projects: &Path,
        lane_homes: &Path,
        pilot_home: &Path,
        claude_config: PathBuf,
    ) -> RealHost {
        RealHost {
            script: projects
                .join("agentlife")
                .join("scripts")
                .join("synapse-provision-weekly.ps1"),
            lane_homes: lane_homes.to_path_buf(),
            pilot_home: pilot_home.to_path_buf(),
            claude_config,
        }
    }
}

impl Host for RealHost {
    fn disk_floor(&self, state_dir: &Path) -> Result<(), String> {
        match crate::disk::guard(state_dir) {
            Ok(_) => Ok(()),
            Err(refusal) => Err(refusal.to_string()),
        }
    }

    fn provision(&self, roles_file: &Path) -> Result<ProvisionCounts, String> {
        if !self.script.is_file() {
            return Err(format!("{} not found", self.script.display()));
        }
        let o = std::process::Command::new("pwsh")
            .args(["-NoProfile", "-NonInteractive", "-File"])
            .arg(&self.script)
            .arg("-RolesFile")
            .arg(roles_file)
            .arg("-LaneHomeRoot")
            .arg(&self.lane_homes)
            .arg("-SynapseHome")
            .arg(&self.pilot_home)
            .arg("-Apply")
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("cannot run pwsh: {e}"))?;
        // The script prints counts and refused role names only, never a key or a path.
        parse_counts(&String::from_utf8_lossy(&o.stdout))
    }

    fn start(&self, role: &str, model: &str) -> Result<i32, String> {
        let exe = std::env::current_exe().map_err(|e| format!("cannot find this exe: {e}"))?;
        let o = std::process::Command::new(exe)
            .args(["start", "--role", role, "--model", model])
            .args(["--reason", "lane-new"])
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("cannot run lane-restart start: {e}"))?;
        let code = o.status.code().unwrap_or(-1);
        if code != 0 && code != 2 {
            let err = String::from_utf8_lossy(&o.stderr);
            let last = err
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("");
            return Err(format!("lane-restart start exited {code}: {}", last.trim()));
        }
        Ok(code)
    }

    fn claude_alive(&self, pid: u32) -> bool {
        use crate::facts::SystemFacts;
        crate::facts::SysinfoFacts::new(self.claude_config.clone()).is_alive_claude_process(pid)
    }
}

/// The newest non-empty `host-<role>-<pid>.log` in `state_dir` written at or after `since`, as an absolute
/// path. `None` if there is none.
pub fn host_log_since(
    state_dir: &Path,
    role: &str,
    since: std::time::SystemTime,
) -> Option<PathBuf> {
    let prefix = format!("host-{role}-");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for e in std::fs::read_dir(state_dir).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix(&prefix) else {
            continue;
        };
        // `host-<role>-<pid>.log`: a longer role that begins with this one (`rock` vs `rockfish`) has a
        // non-numeric tail, so it is not this lane's log.
        let Some(pid) = rest.strip_suffix(".log") else {
            continue;
        };
        if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(m) = e.metadata() else { continue };
        let Ok(t) = m.modified() else { continue };
        if m.len() == 0 || t < since {
            continue;
        }
        if best.as_ref().is_none_or(|(bt, _)| t > *bt) {
            best = Some((t, e.path()));
        }
    }
    best.map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_counts_line_is_parsed_whole_or_refused() {
        let out = "synapse-provision-weekly: APPLY\nprovision: ok=0 created=1 renewed=0 refused=0 other=0\nannounce: current=0 copied=1 no-home=0 failed=0\n";
        let c = parse_counts(out).unwrap();
        assert_eq!((c.created, c.ok, c.refused, c.total()), (1, 0, 0, 1));
        // control: a different line gives different counts
        let c2 = parse_counts("provision: ok=19 created=0 renewed=0 refused=1 other=0").unwrap();
        assert_eq!((c2.ok, c2.refused, c2.total()), (19, 1, 20));
        for bad in [
            "",
            "FAIL: roles file not found",
            "provision: ok=1 created=0",
            "provision: ok=x created=0 renewed=0 refused=0 other=0",
        ] {
            assert!(parse_counts(bad).is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn the_host_log_is_this_roles_numbered_non_empty_and_new() {
        let d = tempfile::tempdir().unwrap();
        let before = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        let w = |n: &str, t: &str| std::fs::write(d.path().join(n), t).unwrap();
        w("host-rockfish-100.log", "");
        w("host-rockfishy-200.log", "not ours");
        w("host-rockfish-abc.log", "not a pid");
        w("host-rockfish-300.txt", "wrong ext");
        assert_eq!(host_log_since(d.path(), "rockfish", before), None);
        w("host-rockfish-400.log", "boot");
        let got = host_log_since(d.path(), "rockfish", before).unwrap();
        assert_eq!(got, d.path().join("host-rockfish-400.log"));
        assert!(got.is_absolute());
        // older than the step: not this run's
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
        assert_eq!(host_log_since(d.path(), "rockfish", later), None);
    }
}

#[cfg(test)]
mod run_tests {
    use super::*;
    use crate::lane_new::{run_full, Report, Roots, Status};
    use crate::spec::{self, SpecState};
    use chrono::Utc;
    use std::cell::RefCell;

    /// What the fake gets wrong, one at a time. `Honest` leaves exactly what the real commands leave.
    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Lie {
        Honest,
        Refused,
        TwoRoles,
        NoCertButSaysCreated,
        NoAnnounceButSaysCreated,
        StartNoState,
        StartDeadPid,
        StartSpecStillSuspended,
        StartEmptyLog,
        StartFails,
        StartAwaitingDialog,
        DiskFull,
    }

    struct FakeHost {
        lie: Lie,
        roots: Roots,
        calls: RefCell<Vec<String>>,
        roles_file_text: RefCell<Option<String>>,
    }

    impl FakeHost {
        fn new(lie: Lie, roots: &Roots) -> FakeHost {
            FakeHost {
                lie,
                roots: roots.clone(),
                calls: RefCell::new(Vec::new()),
                roles_file_text: RefCell::new(None),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl Host for FakeHost {
        fn disk_floor(&self, _: &Path) -> Result<(), String> {
            self.calls.borrow_mut().push("disk".into());
            if self.lie == Lie::DiskFull {
                Err("only 3 GB free".into())
            } else {
                Ok(())
            }
        }

        fn provision(&self, roles_file: &Path) -> Result<ProvisionCounts, String> {
            self.calls.borrow_mut().push("provision".into());
            let text = std::fs::read_to_string(roles_file).map_err(|e| e.to_string())?;
            *self.roles_file_text.borrow_mut() = Some(text.clone());
            let role = text.trim().to_string();
            let home = self.roots.lane_homes.join(&role);
            let mut c = ProvisionCounts {
                created: 1,
                ..Default::default()
            };
            match self.lie {
                Lie::Refused => {
                    c = ProvisionCounts {
                        refused: 1,
                        ..Default::default()
                    }
                }
                Lie::TwoRoles => c.created = 2,
                _ => {}
            }
            if self.lie != Lie::NoCertButSaysCreated && self.lie != Lie::Refused {
                let d = home.join("roles").join(&role);
                std::fs::create_dir_all(&d).unwrap();
                std::fs::write(d.join("cert.pem"), "cert").unwrap();
            }
            if self.lie != Lie::NoAnnounceButSaysCreated && self.lie != Lie::Refused {
                std::fs::create_dir_all(&home).unwrap();
                std::fs::write(home.join("synapsed.json"), "{}").unwrap();
            }
            Ok(c)
        }

        fn start(&self, role: &str, _model: &str) -> Result<i32, String> {
            self.calls.borrow_mut().push("start".into());
            let dir = &self.roots.state_dir;
            if self.lie == Lie::StartFails {
                return Err("lane-restart start exited 1: refused".into());
            }
            if self.lie != Lie::StartNoState {
                let pid = if self.lie == Lie::StartDeadPid {
                    111
                } else {
                    4242
                };
                let v = serde_json::json!({
                    "role": role, "session_id": "s-1", "pid": pid, "cwd": "x", "name": role,
                    "model": null, "permission_mode": null, "remote_control": false, "busy": false,
                    "subagents_running": 0, "no_background_shells": null, "launch_args": null,
                    "updated_at": "2026-10-30T23:00:00Z", "updated_by_event": "SessionStart",
                });
                std::fs::write(dir.join(format!("{role}.json")), v.to_string()).unwrap();
            }
            if self.lie != Lie::StartSpecStillSuspended {
                let mut s = spec::read(dir, role).unwrap();
                s.state = SpecState::Running;
                spec::write_atomic(dir, &s).unwrap();
            }
            let log = if self.lie == Lie::StartEmptyLog {
                ""
            } else {
                "host up"
            };
            std::fs::write(dir.join(format!("host-{role}-7777.log")), log).unwrap();
            Ok(if self.lie == Lie::StartAwaitingDialog {
                2
            } else {
                0
            })
        }

        fn claude_alive(&self, pid: u32) -> bool {
            self.calls.borrow_mut().push(format!("alive {pid}"));
            pid == 4242
        }
    }

    fn world(d: &tempfile::TempDir) -> Roots {
        let b = d.path();
        std::fs::create_dir_all(b.join("state")).unwrap();
        std::fs::write(b.join("state").join("roles-soak.txt"), "fuel\n").unwrap();
        Roots::new(
            b.join("state"),
            b.join("proj"),
            b.join("homes"),
            b.join("pilot"),
            b.join("bin").join("chan"),
        )
        .unwrap()
    }

    fn go(r: &Roots, h: &FakeHost, apply: bool) -> Report {
        run_full(
            r,
            None,
            Some(h),
            "rockfish",
            "rockfish",
            "sonnet",
            apply,
            Utc::now(),
        )
    }

    fn row(rep: &Report, n: u8) -> (Status, String) {
        let r = rep
            .rows
            .iter()
            .find(|r| r.n == n)
            .unwrap_or_else(|| panic!("no step {n}: {}", rep.render(true)));
        (r.status, r.detail.clone())
    }

    fn listing(r: &Roots) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(&r.state_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn an_honest_run_provisions_then_starts_and_cleans_up_its_roles_file() {
        let d = tempfile::tempdir().unwrap();
        let r = world(&d);
        let h = FakeHost::new(Lie::Honest, &r);
        let rep = go(&r, &h, true);
        assert!(!rep.failed(), "{}", rep.render(true));
        assert_eq!(row(&rep, 6).0, Status::Ok);
        let (st, detail) = row(&rep, 7);
        assert_eq!(st, Status::Ok);
        assert!(
            detail.contains("pid 4242 live, spec running, host log "),
            "{detail}"
        );
        assert!(detail.contains("host-rockfish-7777.log"), "{detail}");
        // order, and the roles file held this role and no other
        assert_eq!(h.calls(), vec!["disk", "provision", "start", "alive 4242"]);
        assert_eq!(h.roles_file_text.borrow().as_deref(), Some("rockfish\n"));
        assert!(
            !listing(&r).iter().any(|n| n.contains("roles.tmp")),
            "{:?}",
            listing(&r)
        );
    }

    fn apply_with(r: &Roots, h: &FakeHost, role: &str, repo: &str, plan: Option<&str>) -> Report {
        crate::lane_new::run_apply(r, None, Some(h), role, repo, "sonnet", plan, Utc::now())
    }

    #[test]
    fn a_dry_run_prints_the_plan_hash_and_apply_needs_exactly_that_hash() {
        let d = tempfile::tempdir().unwrap();
        let r = world(&d);
        let dry = go(&r, &FakeHost::new(Lie::Honest, &r), false);
        let hash = dry
            .plan
            .clone()
            .expect("a clean dry run prints a plan hash");
        assert_eq!(hash.len(), 16);
        assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(dry
            .render(false)
            .contains(&format!("--apply --plan {hash}")));
        // a failed dry run prints none
        let full = FakeHost::new(Lie::DiskFull, &r);
        assert!(go(&r, &full, false).plan.is_none());
        // a partial run (step 7 failed) then a new dry run: the same hash, so a resume matches the plan read
        let first = FakeHost::new(Lie::StartFails, &r);
        let rep = apply_with(&r, &first, "rockfish", "rockfish", Some(&hash));
        assert_eq!(row(&rep, 7).0, Status::Fail, "{}", rep.render(true));
        let again = go(&r, &FakeHost::new(Lie::Honest, &r), false);
        assert_eq!(again.plan.as_deref(), Some(hash.as_str()));
        // control: the right hash is accepted and acts
        let h = FakeHost::new(Lie::Honest, &r);
        let rep = apply_with(&r, &h, "rockfish", "rockfish", Some(&hash));
        assert!(!rep.failed(), "{}", rep.render(true));
        assert!(h.calls().contains(&"start".to_string()));
    }

    #[test]
    fn apply_without_the_plan_or_with_a_typod_name_or_a_changed_argument_runs_nothing() {
        let d = tempfile::tempdir().unwrap();
        let r = world(&d);
        let hash = go(&r, &FakeHost::new(Lie::Honest, &r), false).plan.unwrap();
        let before = listing(&r);
        let right = Some(hash.as_str());
        for (role, repo, model, plan, why) in [
            ("rockfish", "rockfish", "sonnet", None, "needs --plan"),
            ("rockfsh", "rockfish", "sonnet", right, "not the plan"),
            ("rockfish", "rockfsh", "sonnet", right, "not the plan"),
            ("rockfish", "rockfish", "haiku", right, "not the plan"),
            (
                "rockfish",
                "rockfish",
                "sonnet",
                Some("0123456789abcdef"),
                "not the plan",
            ),
            ("rockfish", "rockfish", "sonnet", Some(""), "not the plan"),
        ] {
            let h = FakeHost::new(Lie::Honest, &r);
            let rep =
                crate::lane_new::run_apply(&r, None, Some(&h), role, repo, model, plan, Utc::now());
            let (st, detail) = row(&rep, 0);
            let case = format!("{role} {repo} {model} {plan:?}");
            assert!(
                st == Status::Fail && detail.contains(why),
                "{case}: {detail}"
            );
            assert!(h.calls().is_empty(), "{case} ran {:?}", h.calls());
            assert_eq!(listing(&r), before, "{case} wrote something");
        }
    }

    #[test]
    fn the_hash_covers_the_role_the_repo_the_model_and_the_roots() {
        let d = tempfile::tempdir().unwrap();
        let r = world(&d);
        let hash = |role: &str, repo: &str, model: &str| {
            crate::lane_new::plan_hash(&r, role, repo, model).unwrap()
        };
        let base = hash("rockfish", "rockfish", "sonnet");
        assert_eq!(base, hash("rockfish", "rockfish", "sonnet"));
        for other in [
            hash("rockfis", "rockfish", "sonnet"),
            hash("rockfish", "rockfis", "sonnet"),
            hash("rockfish", "rockfish", "haiku"),
        ] {
            assert_ne!(base, other);
        }
        let d2 = tempfile::tempdir().unwrap();
        let r2 = world(&d2);
        assert_ne!(
            base,
            crate::lane_new::plan_hash(&r2, "rockfish", "rockfish", "sonnet").unwrap(),
            "another root"
        );
    }

    #[test]
    fn a_dry_run_runs_nothing_and_says_would() {
        let d = tempfile::tempdir().unwrap();
        let r = world(&d);
        let before = listing(&r);
        let h = FakeHost::new(Lie::Honest, &r);
        let rep = go(&r, &h, false);
        assert!(!rep.failed(), "{}", rep.render(false));
        assert_eq!(
            (row(&rep, 6).0, row(&rep, 7).0),
            (Status::Would, Status::Would)
        );
        assert_eq!(h.calls(), vec!["disk"], "a dry run ran an external command");
        assert_eq!(listing(&r), before);
        assert!(!r.lane_homes.exists());
    }

    #[test]
    fn step_6_stops_the_run_on_each_lie_and_step_7_never_starts() {
        for lie in [
            Lie::Refused,
            Lie::TwoRoles,
            Lie::NoCertButSaysCreated,
            Lie::NoAnnounceButSaysCreated,
        ] {
            let d = tempfile::tempdir().unwrap();
            let r = world(&d);
            let h = FakeHost::new(lie, &r);
            let rep = go(&r, &h, true);
            assert_eq!(
                row(&rep, 6).0,
                Status::Fail,
                "{lie:?}: {}",
                rep.render(true)
            );
            assert!(
                rep.rows.iter().all(|x| x.n != 7),
                "{lie:?}: step 7 reported"
            );
            assert!(
                !h.calls().contains(&"start".to_string()),
                "{lie:?} started a lane"
            );
            assert!(
                !listing(&r).iter().any(|n| n.contains("roles.tmp")),
                "{lie:?} left the roles file"
            );
        }
    }

    #[test]
    fn step_7_fails_on_each_lie_about_the_started_lane() {
        for (lie, why) in [
            (Lie::StartNoState, "wrote no state"),
            (Lie::StartDeadPid, "not a live claude"),
            (Lie::StartSpecStillSuspended, "not running"),
            (Lie::StartEmptyLog, "no non-empty host-rockfish"),
            (Lie::StartFails, "exited 1"),
        ] {
            let d = tempfile::tempdir().unwrap();
            let r = world(&d);
            let h = FakeHost::new(lie, &r);
            let rep = go(&r, &h, true);
            let (st, detail) = row(&rep, 7);
            assert!(
                st == Status::Fail && detail.contains(why),
                "{lie:?}: {}",
                rep.render(true)
            );
            assert_eq!(
                row(&rep, 6).0,
                Status::Ok,
                "{lie:?}: step 6 is before and fine"
            );
        }
    }

    #[test]
    fn a_start_awaiting_the_dialog_is_ok_but_says_so() {
        let d = tempfile::tempdir().unwrap();
        let r = world(&d);
        let h = FakeHost::new(Lie::StartAwaitingDialog, &r);
        let rep = go(&r, &h, true);
        let (st, detail) = row(&rep, 7);
        assert_eq!(st, Status::Ok);
        assert!(
            detail.contains("AWAITING the dev-channels dialog"),
            "{detail}"
        );
    }

    #[test]
    fn a_full_disk_refuses_in_step_0_before_anything_is_written() {
        let d = tempfile::tempdir().unwrap();
        let r = world(&d);
        let before = listing(&r);
        let h = FakeHost::new(Lie::DiskFull, &r);
        let rep = go(&r, &h, true);
        let (st, detail) = row(&rep, 0);
        assert!(
            st == Status::Fail && detail.contains("disk floor: only 3 GB free"),
            "{detail}"
        );
        assert_eq!(listing(&r), before);
        assert_eq!(h.calls(), vec!["disk"]);
    }

    #[test]
    fn a_rerun_after_step_7_refuses_rather_than_starting_a_second_time() {
        let d = tempfile::tempdir().unwrap();
        let r = world(&d);
        assert!(!go(&r, &FakeHost::new(Lie::Honest, &r), true).failed());
        let h2 = FakeHost::new(Lie::Honest, &r);
        let rep = go(&r, &h2, true);
        assert!(
            rep.failed() && row(&rep, 0).1.contains("has run"),
            "{}",
            rep.render(true)
        );
        assert!(!h2.calls().contains(&"start".to_string()));
    }

    #[test]
    fn step_6_is_already_when_the_identity_exists_so_a_failed_7_converges_on_rerun() {
        let d = tempfile::tempdir().unwrap();
        let r = world(&d);
        let first = FakeHost::new(Lie::StartFails, &r);
        let rep = go(&r, &first, true);
        assert_eq!((row(&rep, 6).0, row(&rep, 7).0), (Status::Ok, Status::Fail));
        let second = FakeHost::new(Lie::Honest, &r);
        let rep = go(&r, &second, true);
        assert_eq!(row(&rep, 6).0, Status::Already, "{}", rep.render(true));
        assert_eq!(row(&rep, 7).0, Status::Ok);
        assert!(!second.calls().contains(&"provision".to_string()));
    }
}
