// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart install --from <exe>` - replace the installed `lane-restart.exe` and keep its
//! `.sha256` true. The PM did this by hand (rename the running exe aside, copy the new one, forget
//! the sha file): `.claude-hooks` held 13 `lane-restart*` files under seven naming patterns and a
//! `lane-restart.sha256` that no longer described the exe (measured 2026-10-14, docs/ROADMAP.md
//! slice 3 row 2).
//!
//! The candidate is verified **before** anything is touched: it must run `--version` and report
//! `lane-restart <semver>`, and its `status` must exit 0. Then the installed exe is renamed (not
//! copied: a running exe cannot be overwritten on Windows, but it can be renamed) to the one
//! `lane-restart.exe.bak`, the candidate is copied in, the copy is read back byte for byte, and the
//! sha file is rewritten and read back. A failure after the rename puts the old exe back.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The installed file's name, and the names derived from it (`.exe` only where the OS has it).
#[cfg(windows)]
pub const EXE_NAME: &str = "lane-restart.exe";
#[cfg(not(windows))]
pub const EXE_NAME: &str = "lane-restart";
#[cfg(windows)]
pub const BAK_NAME: &str = "lane-restart.exe.bak";
#[cfg(not(windows))]
pub const BAK_NAME: &str = "lane-restart.bak";
pub const SHA_NAME: &str = "lane-restart.sha256";

/// Where the installed exe lives. Fixed, like `state_dir`: a caller-supplied directory would let
/// the verb install anywhere.
pub fn hooks_dir() -> PathBuf {
    lane_state::platform::hooks_dir()
}

/// Runs the candidate. Injected so tests need no real binary.
pub trait Probe {
    /// The candidate's `--version` stdout.
    fn version_output(&self, exe: &Path) -> Result<String, String>;
    /// The candidate's `status` exit success.
    fn status_ok(&self, exe: &Path) -> Result<(), String>;
}

#[derive(Debug, PartialEq, Eq)]
pub struct Installed {
    pub version: String,
    pub sha256: String,
    /// The old exe's new name, or `None` if nothing was installed before.
    pub kept_as: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum InstallError {
    /// Refused before anything changed; the reason is the whole answer.
    Refused(String),
    /// Something failed after the old exe was moved; `restored` says whether it was put back.
    Failed { why: String, restored: bool },
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::Refused(why) => write!(f, "refused: {why}"),
            InstallError::Failed { why, restored } => write!(
                f,
                "failed: {why} (the previous exe {})",
                if *restored {
                    "was put back"
                } else {
                    "COULD NOT be put back: restore it from lane-restart.exe.bak by hand"
                }
            ),
        }
    }
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Lowercase hex SHA-256 of a file's bytes.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(sha256_hex(&bytes))
}

/// `lane-restart 0.14.21` on the first line -> `0.14.21`.
pub fn parse_version(output: &str) -> Option<String> {
    let first = output.lines().next()?;
    let v = first.strip_prefix("lane-restart ")?.trim();
    let core = v.split(['-', '+']).next()?;
    let parts: Vec<&str> = core.split('.').collect();
    let numeric = |s: &&str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    (parts.len() == 3 && parts.iter().all(numeric)).then(|| v.to_string())
}

/// Put the previous exe back after a failure past the swap.
fn roll_back(target: &Path, bak: &Path, had_previous: bool) -> bool {
    if target.exists() && std::fs::remove_file(target).is_err() {
        return false;
    }
    if !had_previous {
        return true;
    }
    std::fs::rename(bak, target).is_ok() && target.is_file() && !bak.exists()
}

/// Install `candidate` as `<hooks_dir>/lane-restart.exe`. With `dry_run` the candidate is verified
/// and the result reported, and nothing is written.
pub fn install(
    hooks_dir: &Path,
    candidate: &Path,
    probe: &dyn Probe,
    dry_run: bool,
) -> Result<Installed, InstallError> {
    install_with(hooks_dir, candidate, probe, dry_run, &|from, to| {
        std::fs::copy(from, to)
    })
}

/// Makes room at `bak` for the exe about to be renamed onto it. Nothing there: nothing to do. An
/// old `.bak` is deleted; one that cannot be deleted (it is running) is renamed to
/// `<bak>.<now_secs>`, and that name is returned. If neither works the install is refused before
/// anything changed, saying exactly what is in the way. The two file operations are injected.
fn free_bak(
    bak: &Path,
    now_secs: u64,
    remove: &dyn Fn(&Path) -> std::io::Result<()>,
    rename: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> Result<Option<PathBuf>, String> {
    if !bak.exists() {
        return Ok(None);
    }
    if remove(bak).is_ok() {
        return Ok(None);
    }
    let mut name = bak.as_os_str().to_owned();
    name.push(format!(".{now_secs}"));
    let aside = PathBuf::from(name);
    match rename(bak, &aside) {
        Ok(()) => Ok(Some(aside)),
        Err(e) => Err(format!(
            "{} exists, cannot be deleted (it is probably running) and cannot be renamed to {}: {e}. \
             Rename or remove {} by hand, then run the install again",
            bak.display(),
            aside.display(),
            bak.display()
        )),
    }
}

/// `install` with the copy injected, so a copy that lands wrong bytes can be tested.
fn install_with(
    hooks_dir: &Path,
    candidate: &Path,
    probe: &dyn Probe,
    dry_run: bool,
    copy: &dyn Fn(&Path, &Path) -> std::io::Result<u64>,
) -> Result<Installed, InstallError> {
    let refuse = |why: String| InstallError::Refused(why);
    let target = hooks_dir.join(EXE_NAME);
    let bak = hooks_dir.join(BAK_NAME);
    let sha_path = hooks_dir.join(SHA_NAME);

    if !candidate.is_file() {
        return Err(refuse(format!(
            "{} is not a file: nothing to install",
            candidate.display()
        )));
    }
    if !hooks_dir.is_dir() {
        return Err(refuse(format!(
            "{} is not a directory",
            hooks_dir.display()
        )));
    }
    let had_previous = target.is_file();
    if had_previous {
        let same = match (candidate.canonicalize(), target.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        };
        if same {
            return Err(refuse(format!(
                "{} is the installed exe itself: pass the freshly built one",
                candidate.display()
            )));
        }
    }

    // Verify the candidate before anything is touched.
    let out = probe
        .version_output(candidate)
        .map_err(|e| refuse(format!("candidate `--version` failed: {e}")))?;
    let version = parse_version(&out).ok_or_else(|| {
        refuse(format!(
            "candidate `--version` did not start with `lane-restart <x.y.z>`: {:?}",
            out.lines().next().unwrap_or("")
        ))
    })?;
    probe
        .status_ok(candidate)
        .map_err(|e| refuse(format!("candidate `status` failed: {e}")))?;
    let want = sha256_file(candidate).map_err(refuse)?;

    if dry_run {
        return Ok(Installed {
            version,
            sha256: want,
            kept_as: had_previous.then_some(bak),
        });
    }

    // The rename replaces an earlier `.bak`, so there is normally only the one. Windows refuses
    // that when the earlier `.bak` is itself a RUNNING exe (a host started before the previous
    // install still runs from it): "Access is denied" (2026-10-19, installing 0.27.0). A running
    // exe can be renamed but not replaced or deleted, so it is moved aside instead.
    if had_previous {
        if let Some(aside) = free_bak(
            &bak,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            &|p| std::fs::remove_file(p),
            &|a, b| std::fs::rename(a, b),
        )
        .map_err(refuse)?
        {
            eprintln!(
                "lane-restart install: the earlier {} is still in use; moved it to {}",
                bak.display(),
                aside.display()
            );
        }
        std::fs::rename(&target, &bak).map_err(|e| {
            refuse(format!(
                "cannot move the installed exe to {}: {e}",
                bak.display()
            ))
        })?;
    }
    let fail = |why: String| InstallError::Failed {
        restored: roll_back(&target, &bak, had_previous),
        why,
    };
    copy(candidate, &target)
        .map_err(|e| fail(format!("cannot copy to {}: {e}", target.display())))?;
    let got = sha256_file(&target).map_err(fail)?;
    if got != want {
        return Err(fail(format!(
            "the installed copy reads back as {got}, not {want}"
        )));
    }
    let line = format!("{want} *{EXE_NAME}\n");
    std::fs::write(&sha_path, &line)
        .map_err(|e| fail(format!("cannot write {}: {e}", sha_path.display())))?;
    match std::fs::read_to_string(&sha_path) {
        Ok(back) if back == line => {}
        _ => return Err(fail(format!("{} did not read back", sha_path.display()))),
    }
    Ok(Installed {
        version,
        sha256: want,
        kept_as: had_previous.then_some(bak),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use tempfile::tempdir;

    struct FakeProbe {
        version: Result<String, String>,
        status: Result<(), String>,
        calls: RefCell<Vec<String>>,
    }

    impl FakeProbe {
        fn good(v: &str) -> Self {
            FakeProbe {
                version: Ok(format!("lane-restart {v}\nembedded approvals: none\n")),
                status: Ok(()),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl Probe for FakeProbe {
        fn version_output(&self, exe: &Path) -> Result<String, String> {
            self.calls
                .borrow_mut()
                .push(format!("version {}", exe.display()));
            self.version.clone()
        }
        fn status_ok(&self, exe: &Path) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("status {}", exe.display()));
            self.status.clone()
        }
    }

    fn hex(bytes: &[u8]) -> String {
        sha256_hex(bytes)
    }

    /// A hooks dir holding an installed exe and a STALE sha file, plus a candidate elsewhere.
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let t = tempdir().unwrap();
        let hooks = t.path().join("hooks");
        std::fs::create_dir(&hooks).unwrap();
        std::fs::write(hooks.join(EXE_NAME), b"OLD-EXE").unwrap();
        std::fs::write(
            hooks.join(SHA_NAME),
            format!("{} *{EXE_NAME}\n", "0".repeat(64)),
        )
        .unwrap();
        let cand = t.path().join("new.exe");
        std::fs::write(&cand, b"NEW-EXE").unwrap();
        (t, hooks, cand)
    }

    #[test]
    fn installs_keeps_one_bak_and_rewrites_the_stale_sha_file() {
        let (_t, hooks, cand) = fixture();
        let probe = FakeProbe::good("0.14.21");
        let done = install(&hooks, &cand, &probe, false).unwrap();
        assert_eq!(done.version, "0.14.21");
        assert_eq!(std::fs::read(hooks.join(EXE_NAME)).unwrap(), b"NEW-EXE");
        assert_eq!(std::fs::read(hooks.join(BAK_NAME)).unwrap(), b"OLD-EXE");
        assert_eq!(done.kept_as, Some(hooks.join(BAK_NAME)));
        let sha = std::fs::read_to_string(hooks.join(SHA_NAME)).unwrap();
        assert_eq!(sha, format!("{} *{EXE_NAME}\n", hex(b"NEW-EXE")));
        assert_eq!(done.sha256, hex(b"NEW-EXE"));
    }

    #[test]
    fn a_second_install_replaces_the_bak_rather_than_adding_another() {
        let (_t, hooks, cand) = fixture();
        install(&hooks, &cand, &FakeProbe::good("0.14.21"), false).unwrap();
        std::fs::write(&cand, b"NEWER-EXE").unwrap();
        install(&hooks, &cand, &FakeProbe::good("0.14.22"), false).unwrap();
        assert_eq!(std::fs::read(hooks.join(BAK_NAME)).unwrap(), b"NEW-EXE");
        let mut names: Vec<_> = std::fs::read_dir(&hooks)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, [EXE_NAME, BAK_NAME, SHA_NAME]);
    }

    #[test]
    fn free_bak_deletes_an_old_bak_moves_a_running_one_aside_and_refuses_when_neither_works() {
        let d = tempdir().unwrap();
        let bak = d.path().join(BAK_NAME);
        let deny = |_: &Path| Err(std::io::Error::other("Access is denied"));
        let deny2 = |_: &Path, _: &Path| Err(std::io::Error::other("Access is denied"));
        // nothing there: nothing done, and nothing asked of the file system
        assert_eq!(free_bak(&bak, 7, &deny, &deny2), Ok(None));
        // deletable: deleted
        std::fs::write(&bak, b"OLD").unwrap();
        assert_eq!(
            free_bak(&bak, 7, &|p| std::fs::remove_file(p), &deny2),
            Ok(None)
        );
        assert!(!bak.exists());
        // not deletable (running): renamed aside, contents kept
        std::fs::write(&bak, b"RUNNING").unwrap();
        let aside = free_bak(&bak, 7, &deny, &|a, b| std::fs::rename(a, b))
            .unwrap()
            .unwrap();
        assert_eq!(aside, d.path().join(format!("{BAK_NAME}.7")));
        assert_eq!(std::fs::read(&aside).unwrap(), b"RUNNING");
        assert!(!bak.exists());
        // neither works: refused, naming the file
        std::fs::write(&bak, b"STUCK").unwrap();
        let why = free_bak(&bak, 7, &deny, &deny2).unwrap_err();
        assert!(why.contains(BAK_NAME) && why.contains("by hand"), "{why}");
    }

    /// The real failure of 2026-10-19: the earlier `.bak` is the image of a running process.
    #[cfg(windows)]
    #[test]
    fn a_running_bak_is_moved_aside_by_the_real_file_operations() {
        let d = tempdir().unwrap();
        let bak = d.path().join(BAK_NAME);
        std::fs::copy(r"C:\Windows\System32\ping.exe", &bak).unwrap();
        let mut running = std::process::Command::new(&bak)
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            std::fs::remove_file(&bak).is_err(),
            "control: a running exe cannot be deleted, or this test proves nothing"
        );
        let aside = free_bak(&bak, 9, &|p| std::fs::remove_file(p), &|a, b| {
            std::fs::rename(a, b)
        });
        let _ = running.kill();
        let _ = running.wait();
        let aside = aside.unwrap().unwrap();
        assert!(aside.is_file() && !bak.exists());
    }

    #[test]
    fn an_install_whose_old_bak_cannot_be_deleted_still_installs_and_nothing_is_lost() {
        let (_t, hooks, cand) = fixture();
        // the earlier bak exists; the real remove works here, so the move-aside path is the unit
        // test above. This pins that an existing bak never stops an install.
        std::fs::write(hooks.join(BAK_NAME), b"EARLIER").unwrap();
        install(&hooks, &cand, &FakeProbe::good("0.14.21"), false).unwrap();
        assert_eq!(std::fs::read(hooks.join(EXE_NAME)).unwrap(), b"NEW-EXE");
        assert_eq!(std::fs::read(hooks.join(BAK_NAME)).unwrap(), b"OLD-EXE");
    }

    #[test]
    fn a_candidate_that_fails_version_changes_nothing() {
        let (_t, hooks, cand) = fixture();
        let mut probe = FakeProbe::good("0.14.21");
        probe.version = Err("exited 1".to_string());
        let err = install(&hooks, &cand, &probe, false).unwrap_err();
        assert!(matches!(err, InstallError::Refused(_)), "{err:?}");
        assert_eq!(std::fs::read(hooks.join(EXE_NAME)).unwrap(), b"OLD-EXE");
        assert!(!hooks.join(BAK_NAME).exists());
    }

    #[test]
    fn a_candidate_that_is_not_lane_restart_changes_nothing() {
        let (_t, hooks, cand) = fixture();
        let mut probe = FakeProbe::good("0.14.21");
        probe.version = Ok("some-other-tool 1.0.0\n".to_string());
        assert!(matches!(
            install(&hooks, &cand, &probe, false),
            Err(InstallError::Refused(_))
        ));
        assert_eq!(std::fs::read(hooks.join(EXE_NAME)).unwrap(), b"OLD-EXE");
    }

    #[test]
    fn a_candidate_whose_status_fails_changes_nothing() {
        let (_t, hooks, cand) = fixture();
        let mut probe = FakeProbe::good("0.14.21");
        probe.status = Err("exit 1".to_string());
        assert!(matches!(
            install(&hooks, &cand, &probe, false),
            Err(InstallError::Refused(_))
        ));
        assert_eq!(std::fs::read(hooks.join(EXE_NAME)).unwrap(), b"OLD-EXE");
        assert!(std::fs::read_to_string(hooks.join(SHA_NAME))
            .unwrap()
            .starts_with("0000"));
    }

    #[test]
    fn dry_run_verifies_but_writes_nothing() {
        let (_t, hooks, cand) = fixture();
        let probe = FakeProbe::good("0.14.21");
        let done = install(&hooks, &cand, &probe, true).unwrap();
        assert_eq!(done.version, "0.14.21");
        assert_eq!(
            probe.calls.borrow().len(),
            2,
            "dry run still probes the candidate"
        );
        assert_eq!(std::fs::read(hooks.join(EXE_NAME)).unwrap(), b"OLD-EXE");
        assert!(!hooks.join(BAK_NAME).exists());
        assert!(std::fs::read_to_string(hooks.join(SHA_NAME))
            .unwrap()
            .starts_with("0000"));
    }

    #[test]
    fn installing_the_installed_exe_over_itself_is_refused() {
        let (_t, hooks, _cand) = fixture();
        let probe = FakeProbe::good("0.14.21");
        let err = install(&hooks, &hooks.join(EXE_NAME), &probe, false).unwrap_err();
        assert!(matches!(err, InstallError::Refused(_)), "{err:?}");
        assert_eq!(std::fs::read(hooks.join(EXE_NAME)).unwrap(), b"OLD-EXE");
        assert!(probe.calls.borrow().is_empty());
    }

    #[test]
    fn a_missing_candidate_is_refused() {
        let (t, hooks, _cand) = fixture();
        let probe = FakeProbe::good("0.1.0");
        let err = install(&hooks, &t.path().join("nope.exe"), &probe, false).unwrap_err();
        assert!(matches!(err, InstallError::Refused(_)), "{err:?}");
        assert!(probe.calls.borrow().is_empty(), "nothing is run");
        assert_eq!(std::fs::read(hooks.join(EXE_NAME)).unwrap(), b"OLD-EXE");
    }

    #[test]
    fn a_copy_that_lands_the_wrong_bytes_puts_the_old_exe_back() {
        let (_t, hooks, cand) = fixture();
        let truncating = |_from: &Path, to: &Path| {
            std::fs::write(to, b"NEW")?;
            Ok(3)
        };
        let err = install_with(
            &hooks,
            &cand,
            &FakeProbe::good("0.14.21"),
            false,
            &truncating,
        )
        .unwrap_err();
        assert!(
            matches!(err, InstallError::Failed { restored: true, .. }),
            "{err:?}"
        );
        assert_eq!(std::fs::read(hooks.join(EXE_NAME)).unwrap(), b"OLD-EXE");
        assert!(std::fs::read_to_string(hooks.join(SHA_NAME))
            .unwrap()
            .starts_with("0000"));
    }

    #[test]
    fn a_first_install_with_nothing_there_keeps_no_bak() {
        let (_t, hooks, cand) = fixture();
        std::fs::remove_file(hooks.join(EXE_NAME)).unwrap();
        std::fs::remove_file(hooks.join(SHA_NAME)).unwrap();
        let done = install(&hooks, &cand, &FakeProbe::good("0.14.21"), false).unwrap();
        assert_eq!(done.kept_as, None);
        assert!(!hooks.join(BAK_NAME).exists());
        assert_eq!(std::fs::read(hooks.join(EXE_NAME)).unwrap(), b"NEW-EXE");
    }

    #[test]
    fn a_failure_after_the_swap_puts_the_old_exe_back() {
        let (_t, hooks, cand) = fixture();
        // A directory where the sha file must go makes the sha write fail AFTER the swap.
        std::fs::remove_file(hooks.join(SHA_NAME)).unwrap();
        std::fs::create_dir(hooks.join(SHA_NAME)).unwrap();
        let err = install(&hooks, &cand, &FakeProbe::good("0.14.21"), false).unwrap_err();
        assert!(
            matches!(err, InstallError::Failed { restored: true, .. }),
            "{err:?}"
        );
        assert_eq!(std::fs::read(hooks.join(EXE_NAME)).unwrap(), b"OLD-EXE");
    }

    #[test]
    fn parse_version_takes_only_the_first_line_and_wants_three_numbers() {
        assert_eq!(
            parse_version("lane-restart 0.14.21\nx\n").as_deref(),
            Some("0.14.21")
        );
        assert_eq!(
            parse_version("lane-restart 1.2.3-rc.1\n").as_deref(),
            Some("1.2.3-rc.1")
        );
        assert_eq!(parse_version("lane-restart 0.14\n"), None);
        assert_eq!(parse_version("other 0.14.21\n"), None);
        assert_eq!(parse_version(""), None);
    }
}
