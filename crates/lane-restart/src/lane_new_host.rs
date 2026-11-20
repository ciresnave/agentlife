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
    /// `--no-start`: steps 0 to 6 only; the spec stays `suspended` and step 7 is skipped. Only [`NoStart`] says yes.
    fn no_start(&self) -> bool {
        false
    }
    /// The role's epoch in the daemon's roster: `Some(n)` (n >= 1) when the daemon knows the role, `None`
    /// when the roster does not list it (never claimed). `Err` if the roster could not be read at all, which
    /// is not the same as "unknown role": a slow daemon must not read as an empty one.
    fn role_epoch(&self, _role: &str) -> Result<Option<u64>, String> {
        Err("this host cannot read the daemon's roster".to_string())
    }
    /// One `synapse claim --role R` from the lane's own home, retried with back-off when the daemon does not
    /// answer in time. Only ever called for a role `role_epoch` found absent: a claim takes a role over
    /// from whoever holds it, so it is never used on a known one.
    fn claim(&self, _role: &str) -> Result<(), String> {
        Err("this host cannot claim a role".to_string())
    }
    /// [`Host::claim`] from a named Synapse home: a lane with its own setup keeps its identity somewhere
    /// other than `lane_homes/<role>`, and a claim from the wrong home claims the wrong identity. `None`
    /// is the lane-home default. A host that does not know homes ignores it.
    fn claim_from(&self, role: &str, _home: Option<&Path>) -> Result<(), String> {
        self.claim(role)
    }
}

/// The epoch of `role` in `synapse list` output (one JSON object per line, `global_id` `role@account`).
/// `Ok(None)`: the output was read and the role is not in it. `Err`: no line parsed at all, so nothing was
/// read (an empty answer from a daemon that did not reply is not "the role is absent").
pub fn epoch_from_list(out: &str, role: &str) -> Result<Option<u64>, String> {
    let mut parsed = 0usize;
    let mut found: Option<u64> = None;
    let mut accounts: Vec<String> = Vec::new();
    for line in out.lines().map(str::trim).filter(|l| l.starts_with('{')) {
        // A row that cannot be read might be the very role asked about: failing here is the safe answer,
        // because "absent" leads to a claim, and a claim takes a role over from its holder.
        let v = serde_json::from_str::<serde_json::Value>(line)
            .map_err(|e| format!("a roster row does not parse ({e}): {line:?}"))?;
        let id = v
            .get("global_id")
            .and_then(|g| g.as_str())
            .ok_or_else(|| format!("a roster row has no global_id: {line:?}"))?;
        parsed += 1;
        let (short, account) = id.split_once('@').unwrap_or((id, ""));
        if short == role {
            // Epoch 0 is a role the roster lists but nobody has claimed: not known. A missing or
            // non-numeric epoch is a row that cannot be read, and that must not read as "absent".
            let epoch = v
                .get("epoch")
                .and_then(|e| e.as_u64())
                .ok_or_else(|| format!("the roster lists {id} with no usable epoch: {line:?}"))?;
            if !accounts.iter().any(|a| a == account) {
                accounts.push(account.to_string());
            }
            found = Some(epoch).filter(|e| *e >= 1);
        }
    }
    if accounts.len() > 1 {
        return Err(format!(
            "the roster lists {role} under {} accounts ({}); not claiming into an ambiguity",
            accounts.len(),
            accounts.join(", ")
        ));
    }
    if parsed == 0 {
        Err("the roster answer held no role lines".to_string())
    } else {
        Ok(found)
    }
}

/// Runs `attempt` up to `tries` times, calling `pause(n)` (n = 1, 2, ...) between failures. The last error
/// is returned with the count, so "the daemon is slow" is told as that and not as a refusal.
pub fn retry_claim(
    tries: u32,
    attempt: &mut dyn FnMut() -> Result<(), String>,
    pause: &mut dyn FnMut(u32),
) -> Result<(), String> {
    let mut last = String::new();
    for n in 1..=tries {
        match attempt() {
            Ok(()) => return Ok(()),
            Err(e) => last = e,
        }
        if n < tries {
            pause(n);
        }
    }
    Err(format!("no claim after {tries} tries: {last}"))
}

/// Runs `cmd` to the end or kills it after `limit`; `Ok(stdout)` only on exit 0. Both pipes are drained
/// on their own threads while the child runs, so a long answer cannot fill a pipe and stall the child.
fn run_limited(
    mut cmd: std::process::Command,
    limit: std::time::Duration,
) -> Result<String, String> {
    use std::io::Read;
    fn drain<R: Read + Send + 'static>(s: Option<R>) -> std::thread::JoinHandle<String> {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut s) = s {
                let _ = s.read_to_end(&mut bytes);
            }
            String::from_utf8_lossy(&bytes).into_owned()
        })
    }
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run synapse: {e}"))?;
    let out_t = drain(child.stdout.take());
    let err_t = drain(child.stderr.take());
    let start = std::time::Instant::now();
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => {
                let out = out_t.join().unwrap_or_default();
                let err = err_t.join().unwrap_or_default();
                return if status.success() {
                    Ok(out)
                } else {
                    let last = err
                        .lines()
                        .rev()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("");
                    Err(format!(
                        "synapse exited {:?}: {}",
                        status.code(),
                        last.trim()
                    ))
                };
            }
            None if start.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "synapse did not answer within {} s",
                    limit.as_secs()
                ));
            }
            None => std::thread::sleep(std::time::Duration::from_millis(200)),
        }
    }
}

/// `lane-new --no-start`: the same host, except that step 7 (start) is skipped and the plan hash differs, so a
/// dry run with the flag cannot be applied without it (or the reverse).
pub struct NoStart<'a>(pub &'a dyn Host);

impl Host for NoStart<'_> {
    fn disk_floor(&self, state_dir: &Path) -> Result<(), String> {
        self.0.disk_floor(state_dir)
    }
    fn provision(&self, roles_file: &Path) -> Result<ProvisionCounts, String> {
        self.0.provision(roles_file)
    }
    fn start(&self, _role: &str, _model: &str) -> Result<i32, String> {
        Err("--no-start: refusing to start the lane".to_string())
    }
    fn claude_alive(&self, pid: u32) -> bool {
        self.0.claude_alive(pid)
    }
    fn no_start(&self) -> bool {
        true
    }
    fn role_epoch(&self, role: &str) -> Result<Option<u64>, String> {
        self.0.role_epoch(role)
    }
    fn claim(&self, role: &str) -> Result<(), String> {
        self.0.claim(role)
    }
    fn claim_from(&self, role: &str, home: Option<&Path>) -> Result<(), String> {
        self.0.claim_from(role, home)
    }
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

    /// `synapse list` through the pilot home as the PM, the only caller of the lane-making commands and
    /// the role that sees every lane. The inherited `LANE_ROLE` is NOT trusted: a value that cannot see the
    /// target would list it as absent, and an absent role gets claimed (taken over).
    fn role_epoch(&self, role: &str) -> Result<Option<u64>, String> {
        let mut c = std::process::Command::new("synapse");
        c.args(["list", "--role", "pm"])
            .env("SYNAPSE_HOME", &self.pilot_home);
        let out = run_limited(c, std::time::Duration::from_secs(60))?;
        epoch_from_list(&out, role)
    }

    fn claim(&self, role: &str) -> Result<(), String> {
        self.claim_from(role, None)
    }

    fn claim_from(&self, role: &str, home: Option<&Path>) -> Result<(), String> {
        let home = home.map_or_else(|| self.lane_homes.join(role), Path::to_path_buf);
        let mut tried = 0u32;
        retry_claim(
            4,
            &mut || {
                // A claim that timed out may still have been accepted, and another provisioner may have
                // claimed meanwhile: before every retry read the roster, and stop if the role is known now,
                // because claiming again would take it over from its holder.
                if tried > 0 && matches!(self.role_epoch(role), Ok(Some(_))) {
                    return Ok(());
                }
                tried += 1;
                let mut c = std::process::Command::new("synapse");
                c.args(["claim", "--role", role]).env("SYNAPSE_HOME", &home);
                run_limited(c, std::time::Duration::from_secs(60)).map(|_| ())
            },
            // 5 s, 10 s, 20 s: a daemon busy rewriting a large access.json answers late, not never.
            &mut |n| std::thread::sleep(std::time::Duration::from_secs(5 << (n - 1))),
        )
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
        // `host-<role>-<pid>.log`: a longer role that begins with this one (`zeta` vs `zetafish`) has a
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
    use std::cell::RefCell;

    #[test]
    fn a_claim_for_a_lane_with_its_own_setup_goes_out_from_that_home() {
        use std::cell::RefCell;
        struct Rec(RefCell<Vec<Option<PathBuf>>>);
        impl Host for Rec {
            fn disk_floor(&self, _: &Path) -> Result<(), String> {
                Ok(())
            }
            fn provision(&self, _: &Path) -> Result<ProvisionCounts, String> {
                Err("unused".into())
            }
            fn start(&self, _: &str, _: &str) -> Result<i32, String> {
                Err("unused".into())
            }
            fn claude_alive(&self, _: u32) -> bool {
                false
            }
            fn role_epoch(&self, _: &str) -> Result<Option<u64>, String> {
                Ok(if self.0.borrow().is_empty() {
                    None
                } else {
                    Some(1)
                })
            }
            fn claim_from(&self, _: &str, home: Option<&Path>) -> Result<(), String> {
                self.0.borrow_mut().push(home.map(Path::to_path_buf));
                Ok(())
            }
        }
        let h = Rec(RefCell::new(Vec::new()));
        let own = Path::new("C:/own/synapse-home");
        crate::lane_new::step_claim_from(&h, "kiss", Some(own), true).unwrap();
        assert_eq!(*h.0.borrow(), vec![Some(own.to_path_buf())]);
    }

    #[test]
    fn a_roster_that_cannot_be_read_whole_never_reads_as_the_role_being_absent() {
        let good = r#"{"global_id":"easyants@ciresnave","epoch":3}"#;
        // control: the same roster without the target is a clean absence
        assert_eq!(epoch_from_list(good, "kiss"), Ok(None));
        // a truncated row beside a valid one
        let torn = format!(
            "{good}
{{\"global_id\":\"kiss@cires"
        );
        assert!(epoch_from_list(&torn, "kiss").is_err());
        // the target listed with a missing, zero or malformed epoch
        for bad in [
            r#"{"global_id":"kiss@ciresnave"}"#,
            r#"{"global_id":"kiss@ciresnave","epoch":"7"}"#,
        ] {
            let text = format!(
                "{good}
{bad}"
            );
            assert!(epoch_from_list(&text, "kiss").is_err(), "{bad}");
        }
        // the same short name under two accounts is an ambiguity, not an answer
        let two = format!(
            "{good}
{}
{}",
            r#"{"global_id":"kiss@ciresnave","epoch":7}"#,
            r#"{"global_id":"kiss@other","epoch":2}"#
        );
        assert!(epoch_from_list(&two, "kiss").is_err());
        // control: one well-formed row for the target reads as its epoch
        let one = format!(
            "{good}
{}",
            r#"{"global_id":"kiss@ciresnave","epoch":7}"#
        );
        assert_eq!(epoch_from_list(&one, "kiss"), Ok(Some(7)));
    }

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

    const ROSTER: &str = concat!(
        r#"{"epoch":7,"global_id":"kiss@ciresnave","online":true}"#,
        "\n",
        r#"{"epoch":0,"global_id":"easyants@ciresnave","online":false}"#,
        "\n",
        r#"{"epoch":1,"global_id":"aocl@ciresnave","online":false}"#,
        "\n"
    );

    #[test]
    fn the_roster_gives_a_claimed_roles_epoch_and_nothing_for_an_unclaimed_one() {
        assert_eq!(epoch_from_list(ROSTER, "kiss"), Ok(Some(7)));
        assert_eq!(epoch_from_list(ROSTER, "aocl"), Ok(Some(1)));
        // listed but epoch 0 is not known; absent is not known; a prefix is not the role
        assert_eq!(epoch_from_list(ROSTER, "easyants"), Ok(None));
        assert_eq!(epoch_from_list(ROSTER, "kiss-ref"), Ok(None));
        assert_eq!(epoch_from_list(ROSTER, "kis"), Ok(None));
    }

    #[test]
    fn an_empty_or_unparseable_roster_is_an_error_not_an_absent_role() {
        for bad in ["", "\n", "error: no daemon answered", "{not json}"] {
            assert!(
                epoch_from_list(bad, "kiss").is_err(),
                "{bad:?} read as a roster"
            );
        }
    }

    #[test]
    fn a_slow_daemon_is_retried_with_back_off_and_a_dead_one_is_told_with_the_count() {
        let mut calls = 0;
        let mut pauses = Vec::new();
        let ok = retry_claim(
            4,
            &mut || {
                calls += 1;
                if calls < 3 {
                    Err("did not answer within 60 s".into())
                } else {
                    Ok(())
                }
            },
            &mut |n| pauses.push(n),
        );
        assert_eq!((ok, calls, pauses.clone()), (Ok(()), 3, vec![1, 2]));
        let mut n = 0;
        let mut pauses = Vec::new();
        let err = retry_claim(
            3,
            &mut || {
                n += 1;
                Err("no daemon answered".into())
            },
            &mut |p| pauses.push(p),
        )
        .unwrap_err();
        assert_eq!((n, pauses), (3, vec![1, 2]));
        assert!(
            err.contains("no claim after 3 tries") && err.contains("no daemon answered"),
            "{err}"
        );
    }

    /// A roster fake for `step_claim`: `known` is the epoch it answers before any claim; `claim_works`
    /// says whether a claim makes the roster list the role.
    struct RosterHost {
        known: Option<u64>,
        roster_error: bool,
        claim_works: bool,
        claimed: RefCell<bool>,
        claims: RefCell<u32>,
    }
    impl RosterHost {
        fn new(known: Option<u64>) -> RosterHost {
            RosterHost {
                known,
                roster_error: false,
                claim_works: true,
                claimed: RefCell::new(false),
                claims: RefCell::new(0),
            }
        }
    }
    impl Host for RosterHost {
        fn disk_floor(&self, _: &Path) -> Result<(), String> {
            Ok(())
        }
        fn provision(&self, _: &Path) -> Result<ProvisionCounts, String> {
            Err("unused".into())
        }
        fn start(&self, _: &str, _: &str) -> Result<i32, String> {
            Err("unused".into())
        }
        fn claude_alive(&self, _: u32) -> bool {
            false
        }
        fn role_epoch(&self, _: &str) -> Result<Option<u64>, String> {
            if self.roster_error {
                return Err("synapse did not answer within 60 s".into());
            }
            Ok(self
                .known
                .or_else(|| (*self.claimed.borrow() && self.claim_works).then_some(1)))
        }
        fn claim(&self, _: &str) -> Result<(), String> {
            *self.claims.borrow_mut() += 1;
            *self.claimed.borrow_mut() = true;
            Ok(())
        }
    }

    #[test]
    fn step_claim_registers_an_unknown_role_and_reads_it_back() {
        use crate::lane_new::{step_claim, Status};
        let h = RosterHost::new(None);
        // dry run: would, and nothing claimed
        assert_eq!(step_claim(&h, "aocl", false).unwrap().0, Status::Would);
        assert_eq!(*h.claims.borrow(), 0);
        // apply: claimed once, read back
        let (s, d) = step_claim(&h, "aocl", true).unwrap();
        assert_eq!((s, *h.claims.borrow()), (Status::Ok, 1));
        assert!(d.contains("epoch 1"), "{d}");
        // run again: now known, not claimed a second time
        assert_eq!(step_claim(&h, "aocl", true).unwrap().0, Status::Already);
        assert_eq!(*h.claims.borrow(), 1);
    }

    #[test]
    fn step_claim_never_takes_over_a_known_role_and_never_reads_a_slow_daemon_as_empty() {
        use crate::lane_new::{step_claim, Status};
        let known = RosterHost::new(Some(7));
        assert_eq!(step_claim(&known, "kiss", true).unwrap().0, Status::Already);
        assert_eq!(*known.claims.borrow(), 0, "a known role was claimed over");
        let mut slow = RosterHost::new(None);
        slow.roster_error = true;
        let e = step_claim(&slow, "aocl", true).unwrap_err();
        assert!(e.contains("did not answer"), "{e}");
        assert_eq!(
            *slow.claims.borrow(),
            0,
            "claimed while the roster was unreadable"
        );
        let mut liar = RosterHost::new(None);
        liar.claim_works = false;
        assert!(step_claim(&liar, "aocl", true)
            .unwrap_err()
            .contains("still does not list"));
    }

    #[test]
    fn the_host_log_is_this_roles_numbered_non_empty_and_new() {
        let d = tempfile::tempdir().unwrap();
        let before = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        let w = |n: &str, t: &str| std::fs::write(d.path().join(n), t).unwrap();
        w("host-zetafish-100.log", "");
        w("host-zetafishy-200.log", "not ours");
        w("host-zetafish-abc.log", "not a pid");
        w("host-zetafish-300.txt", "wrong ext");
        assert_eq!(host_log_since(d.path(), "zetafish", before), None);
        w("host-zetafish-400.log", "boot");
        let got = host_log_since(d.path(), "zetafish", before).unwrap();
        assert_eq!(got, d.path().join("host-zetafish-400.log"));
        assert!(got.is_absolute());
        // older than the step: not this run's
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
        assert_eq!(host_log_since(d.path(), "zetafish", later), None);
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
        fn role_epoch(&self, role: &str) -> Result<Option<u64>, String> {
            let claimed = self.calls.borrow().contains(&format!("claim {role}"));
            Ok(claimed.then_some(1))
        }
        fn claim(&self, role: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("claim {role}"));
            Ok(())
        }

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
            let path = dir.join(format!("host-{role}-7777.log"));
            std::fs::write(&path, log).unwrap();
            // Linux stamps a file with a coarse kernel clock that can read a few ms BEHIND the `SystemTime::now()` the caller took
            // just before `start`, so an honest "written since the start" log looked older than `since` (CI, main, 2026-10-30).
            // Stamp it clearly after, as a real host's seconds-later write is.
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
                .unwrap();
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
            "zetafish",
            "zetafish",
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
        assert!(detail.contains("host-zetafish-7777.log"), "{detail}");
        // order, and the roles file held this role and no other
        assert_eq!(
            h.calls(),
            vec!["disk", "provision", "claim zetafish", "start", "alive 4242"]
        );
        assert_eq!(h.roles_file_text.borrow().as_deref(), Some("zetafish\n"));
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
    fn no_start_runs_steps_0_to_6_leaves_the_spec_suspended_and_has_its_own_plan_hash() {
        let d = tempfile::tempdir().unwrap();
        let r = world(&d);
        let h = FakeHost::new(Lie::Honest, &r);
        let ns = NoStart(&h);
        let run_ns = |apply: bool, plan: Option<&str>| {
            if apply {
                crate::lane_new::run_apply(
                    &r,
                    None,
                    Some(&ns),
                    "zetafish",
                    "zetafish",
                    "sonnet",
                    plan,
                    Utc::now(),
                )
            } else {
                run_full(
                    &r,
                    None,
                    Some(&ns),
                    "zetafish",
                    "zetafish",
                    "sonnet",
                    false,
                    Utc::now(),
                )
            }
        };
        let plain = go(&r, &FakeHost::new(Lie::Honest, &r), false)
            .plan
            .expect("plain plan");
        let dry = run_ns(false, None);
        let hash = dry.plan.clone().expect("no-start plan");
        assert_ne!(hash, plain, "the two modes must not share a plan");
        assert_eq!(row(&dry, 7).0, Status::Skipped, "{}", dry.render(false));
        assert!(
            dry.render(false).contains("skipped"),
            "{}",
            dry.render(false)
        );
        h.calls.borrow_mut().clear(); // the dry run read the disk; what follows must run nothing
                                      // the plain hash cannot apply a no-start run, and the no-start hash cannot apply a plain run
        let wrong = run_ns(true, Some(&plain));
        assert!(wrong.failed(), "{}", wrong.render(true));
        assert!(h.calls().is_empty(), "{:?}", h.calls());
        let wrong = apply_with(&r, &h, "zetafish", "zetafish", Some(&hash));
        assert!(wrong.failed(), "{}", wrong.render(true));
        assert!(h.calls().is_empty(), "{:?}", h.calls());
        // the honest apply: identity made, start never called, spec suspended
        let rep = run_ns(true, Some(&hash));
        assert!(!rep.failed(), "{}", rep.render(true));
        assert_eq!(row(&rep, 6).0, Status::Ok);
        assert_eq!(row(&rep, 7).0, Status::Skipped);
        assert_eq!(h.calls(), vec!["disk", "provision", "claim zetafish"]);
        let have = spec::read(&r.state_dir, "zetafish").unwrap();
        assert_eq!(have.state, SpecState::Suspended);
        // control: without the flag the same run starts the lane (the spec goes running)
        let d2 = tempfile::tempdir().unwrap();
        let r2 = world(&d2);
        let h2 = FakeHost::new(Lie::Honest, &r2);
        let rep = go(&r2, &h2, true);
        assert_eq!(row(&rep, 7).0, Status::Ok, "{}", rep.render(true));
        assert!(h2.calls().contains(&"start".to_string()));
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
        let rep = apply_with(&r, &first, "zetafish", "zetafish", Some(&hash));
        assert_eq!(row(&rep, 7).0, Status::Fail, "{}", rep.render(true));
        let again = go(&r, &FakeHost::new(Lie::Honest, &r), false);
        assert_eq!(again.plan.as_deref(), Some(hash.as_str()));
        // control: the right hash is accepted and acts
        let h = FakeHost::new(Lie::Honest, &r);
        let rep = apply_with(&r, &h, "zetafish", "zetafish", Some(&hash));
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
            ("zetafish", "zetafish", "sonnet", None, "needs --plan"),
            ("rockfsh", "zetafish", "sonnet", right, "not the plan"),
            ("zetafish", "rockfsh", "sonnet", right, "not the plan"),
            ("zetafish", "zetafish", "haiku", right, "not the plan"),
            (
                "zetafish",
                "zetafish",
                "sonnet",
                Some("0123456789abcdef"),
                "not the plan",
            ),
            ("zetafish", "zetafish", "sonnet", Some(""), "not the plan"),
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
        let base = hash("zetafish", "zetafish", "sonnet");
        assert_eq!(base, hash("zetafish", "zetafish", "sonnet"));
        for other in [
            hash("rockfis", "zetafish", "sonnet"),
            hash("zetafish", "rockfis", "sonnet"),
            hash("zetafish", "zetafish", "haiku"),
        ] {
            assert_ne!(base, other);
        }
        let d2 = tempfile::tempdir().unwrap();
        let r2 = world(&d2);
        assert_ne!(
            base,
            crate::lane_new::plan_hash(&r2, "zetafish", "zetafish", "sonnet").unwrap(),
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
            (Lie::StartEmptyLog, "no non-empty host-zetafish"),
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
