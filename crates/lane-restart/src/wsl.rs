// SPDX-License-Identifier: MIT OR Apache-2.0

//! Launch mode `wsl` (docs/LANE-RESTART-WSL.md): a lane that runs inside a WSL2 distro, in the
//! PM's sandbox wrapper, instead of as a Windows process.
//!
//! This module is the PURE half: the spec's `wsl` fields and their rules (sections 2 and 7), and
//! the `wsl.exe` command line a launch runs (section 3). It starts nothing and reads nothing, so
//! none of it needs a distro. The fields are checked at read as well as at write: a spec file is
//! data, and a hand-edited one must not be able to make lane-restart run another program.

use crate::relaunch::first_unsafe_argument;
use serde::{Deserialize, Serialize};

/// The `launch` value of a WSL spec.
pub const LAUNCH_WSL: &str = "wsl";

/// The spec schema a WSL spec is written with. Windows specs stay at `spec::SCHEMA` (1), so an
/// older build refuses a WSL spec as an unsupported schema instead of launching its POSIX `cwd`
/// as a Windows process.
pub const SCHEMA_WSL: u32 = 2;

/// Where the lane user's state directory is inside the distro when the spec names none: the Unix
/// value of `lane_state::platform::state_dir` for that user's home.
pub fn default_state_dir(user: &str) -> String {
    format!("/home/{user}/.local/state/lane-state")
}

/// The `wsl` object of a spec: which distro, which user, and the PM's wrapper script inside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WslTarget {
    pub distro: String,
    pub user: String,
    /// An absolute POSIX path inside the distro. lane-restart invokes it and nothing else.
    pub wrapper: String,
    /// Absolute POSIX path of the lane's state directory inside the distro; `None` is
    /// [`default_state_dir`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_dir: Option<String>,
}

/// Why a `wsl` field, or the command built from them, was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslError {
    pub field: &'static str,
    pub why: String,
}

impl std::fmt::Display for WslError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "wsl {}: {}", self.field, self.why)
    }
}

fn refuse(field: &'static str, why: impl Into<String>) -> WslError {
    WslError {
        field,
        why: why.into(),
    }
}

impl WslTarget {
    /// The state directory inside the distro: the spec's, else the default for the user.
    pub fn state_dir_or_default(&self) -> String {
        self.state_dir
            .clone()
            .unwrap_or_else(|| default_state_dir(&self.user))
    }

    /// Every rule of section 2 for the target's own fields.
    pub fn validate(&self) -> Result<(), WslError> {
        if !valid_distro(&self.distro) {
            return Err(refuse(
                "distro",
                format!(
                    "{:?} is not a plain distro name (1 to 64 of letters, digits, `.`, `_`, `-`; \
                     not starting with a symbol, so `wsl.exe` cannot read it as an option)",
                    self.distro
                ),
            ));
        }
        if !valid_user(&self.user) {
            return Err(refuse(
                "user",
                format!(
                    "{:?} is not an account name (lowercase, starts with a letter or `_`, up to \
                     32 of `a-z0-9_-`) or it is root, which a lane never runs as",
                    self.user
                ),
            ));
        }
        strict_path(&self.wrapper).map_err(|why| refuse("wrapper", why))?;
        if let Some(dir) = &self.state_dir {
            posix_path(dir).map_err(|why| refuse("state_dir", why))?;
            if has_dotdot(dir) {
                return Err(refuse("state_dir", "has a `..` segment"));
            }
        }
        Ok(())
    }
}

/// `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`: no leading `-` (it would be read as a `wsl.exe` option).
fn valid_distro(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// `^[a-z_][a-z0-9_-]{0,31}$`, and not `root`.
fn valid_user(s: &str) -> bool {
    let mut chars = s.chars();
    s != "root"
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && s.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
}

fn has_dotdot(path: &str) -> bool {
    path.split('/').any(|seg| seg == "..")
}

/// An absolute POSIX path with nothing a command line could reread: starts with `/`, no control
/// character, no `;` (`wt.exe` reads it as a separator).
fn posix_path(path: &str) -> Result<(), &'static str> {
    if !path.starts_with('/') {
        return Err(
            "is not an absolute POSIX path (it must start with `/`; a Windows path is not one)",
        );
    }
    if path.chars().any(char::is_control) {
        return Err("holds a control character");
    }
    if path.contains(';') {
        return Err("holds a `;`");
    }
    Ok(())
}

/// [`posix_path`] for one file: also no whitespace, no `..` segment, not the root, not a directory.
fn strict_path(path: &str) -> Result<(), &'static str> {
    posix_path(path)?;
    if path.chars().any(char::is_whitespace) {
        return Err("holds whitespace");
    }
    if has_dotdot(path) {
        return Err("has a `..` segment");
    }
    if path.len() < 2 || path.ends_with('/') {
        return Err("does not name a file");
    }
    Ok(())
}

/// A WSL lane's `cwd` is a POSIX path inside the distro: absolute, no control character, no `;`.
/// The Windows rules for `cwd` (drive letters, case folding) do not apply to it.
pub fn validate_cwd(cwd: &str) -> Result<(), WslError> {
    posix_path(cwd).map_err(|why| refuse("cwd", why))
}

/// The command a WSL launch runs (section 3), not yet spawned:
/// `wsl.exe -d <distro> -u <user> --cd <cwd> --exec <wrapper> -- <claude argv...>`.
/// `claude_argv` is the argv `launch::launch_argv` built, `claude` first, prompt second, the
/// variadic dev-channels flag last; it is carried as it is.
pub fn build_command(
    target: &WslTarget,
    cwd: &str,
    claude_argv: &[String],
) -> Result<Vec<String>, WslError> {
    target.validate()?;
    validate_cwd(cwd)?;
    let mut argv: Vec<String> = [
        "wsl.exe",
        "-d",
        target.distro.as_str(),
        "-u",
        target.user.as_str(),
        "--cd",
        cwd,
        "--exec",
        target.wrapper.as_str(),
        "--",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    argv.extend(claude_argv.iter().cloned());
    // `--exec` hands the elements to the program as an argv, but the line passes `wt.exe` first.
    if let Some(bad) = first_unsafe_argument(argv.iter().map(String::as_str)) {
        return Err(refuse("argv", format!("{bad:?} holds a `;`")));
    }
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> WslTarget {
        WslTarget {
            distro: "lane-spike".to_string(),
            user: "lane-commysdk".to_string(),
            wrapper: "/usr/local/bin/lane-sandbox".to_string(),
            state_dir: None,
        }
    }

    const CWD: &str = "/home/lane-commysdk/work/commy-sdk-rust";

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn the_pilot_target_is_valid() {
        assert_eq!(target().validate(), Ok(()));
        assert_eq!(validate_cwd(CWD), Ok(()));
    }

    #[test]
    fn a_distro_is_a_plain_name_that_cannot_be_read_as_an_option() {
        for bad in [
            "",
            "-x",
            "--shutdown",
            "a b",
            "a;b",
            "a/b",
            "a\\b",
            ".hidden",
            "_x",
            "d\u{0}",
            &"a".repeat(65),
        ] {
            let mut t = target();
            t.distro = bad.to_string();
            let e = t.validate().expect_err(&format!("distro {bad:?} accepted"));
            assert_eq!(e.field, "distro", "{bad:?}");
        }
        for good in ["lane-spike", "Ubuntu-24.04", "a", "d_1.x", &"a".repeat(64)] {
            let mut t = target();
            t.distro = good.to_string();
            assert_eq!(t.validate(), Ok(()), "{good:?}");
        }
    }

    #[test]
    fn a_user_is_a_lowercase_account_name_and_never_root() {
        for bad in [
            "",
            "root",
            "Root",
            "1abc",
            "-abc",
            "a b",
            "a;b",
            "A",
            "a.b",
            &"a".repeat(33),
        ] {
            let mut t = target();
            t.user = bad.to_string();
            let e = t.validate().expect_err(&format!("user {bad:?} accepted"));
            assert_eq!(e.field, "user", "{bad:?}");
        }
        for good in ["lane-commysdk", "_svc", "a", "a1_-", &"a".repeat(32)] {
            let mut t = target();
            t.user = good.to_string();
            assert_eq!(t.validate(), Ok(()), "{good:?}");
        }
    }

    #[test]
    fn a_wrapper_is_one_absolute_posix_path_with_nothing_to_reparse() {
        for bad in [
            "",
            "lane-sandbox",
            "./lane-sandbox",
            "C:\\Projects\\lane-sandbox.sh",
            "C:/Projects/lane-sandbox.sh",
            "\\\\wsl$\\lane-spike\\x",
            "/usr/local/bin/../bin/x",
            "/usr/local/bin/lane sandbox",
            "/usr/local/bin/x;y",
            "/usr/local/bin/x\ny",
            "/usr/local/bin/x\u{0}y",
            "/",
            "/usr/local/bin/",
        ] {
            let mut t = target();
            t.wrapper = bad.to_string();
            let e = t
                .validate()
                .expect_err(&format!("wrapper {bad:?} accepted"));
            assert_eq!(e.field, "wrapper", "{bad:?}");
        }
    }

    #[test]
    fn a_state_dir_when_given_is_an_absolute_posix_path_without_dotdot() {
        let mut t = target();
        t.state_dir = Some("/home/lane-commysdk/.local/state/lane-state".to_string());
        assert_eq!(t.validate(), Ok(()));
        for bad in ["", "state", "C:/x", "/a/../b", "/a;b", "/a\nb"] {
            t.state_dir = Some(bad.to_string());
            let e = t
                .validate()
                .expect_err(&format!("state_dir {bad:?} accepted"));
            assert_eq!(e.field, "state_dir", "{bad:?}");
        }
    }

    #[test]
    fn the_default_state_dir_is_under_the_lane_users_home() {
        assert_eq!(
            target().state_dir_or_default(),
            "/home/lane-commysdk/.local/state/lane-state"
        );
        let mut t = target();
        t.state_dir = Some("/srv/state".to_string());
        assert_eq!(t.state_dir_or_default(), "/srv/state");
    }

    #[test]
    fn a_cwd_is_a_posix_path_not_a_windows_one() {
        for bad in [
            "",
            "work",
            "C:\\Projects\\x",
            "C:/Projects/x",
            "/a;b",
            "/a\nb",
            "/a\u{0}b",
        ] {
            let e = validate_cwd(bad).expect_err(&format!("cwd {bad:?} accepted"));
            assert_eq!(e.field, "cwd", "{bad:?}");
        }
        // a path with a space is a path; --exec and the argv carry it as one element
        assert_eq!(validate_cwd("/home/u/my work"), Ok(()));
    }

    #[test]
    fn the_command_is_exactly_the_pilot_shape() {
        let claude = s(&[
            "claude",
            "read agentlife HANDOFF and continue",
            "--name",
            "agentlife",
            "--model",
            "sonnet",
            "--permission-mode",
            "auto",
            "--mcp-config",
            "/home/lane-commysdk/synapse-home/lane.mcp.json",
            "--dangerously-load-development-channels",
            "server:synapse",
        ]);
        let cmd = build_command(&target(), CWD, &claude).unwrap();
        let mut expect = s(&[
            "wsl.exe",
            "-d",
            "lane-spike",
            "-u",
            "lane-commysdk",
            "--cd",
            CWD,
            "--exec",
            "/usr/local/bin/lane-sandbox",
            "--",
        ]);
        expect.extend(claude.clone());
        assert_eq!(cmd, expect);
        // the prompt stays first after `claude`, the variadic flag stays last
        assert_eq!(cmd[10], "claude");
        assert_eq!(cmd[11], "read agentlife HANDOFF and continue");
        assert_eq!(
            cmd[cmd.len() - 2],
            "--dangerously-load-development-channels"
        );
        assert_eq!(cmd[cmd.len() - 1], "server:synapse");
    }

    #[test]
    fn a_semicolon_anywhere_in_the_command_is_refused() {
        let e = build_command(&target(), CWD, &s(&["claude", "a;b"])).expect_err("; accepted");
        assert_eq!(e.field, "argv");
    }

    #[test]
    fn an_invalid_target_or_cwd_builds_no_command() {
        let mut t = target();
        t.user = "root".to_string();
        assert_eq!(
            build_command(&t, CWD, &s(&["claude"]))
                .expect_err("root")
                .field,
            "user"
        );
        assert_eq!(
            build_command(&target(), "C:\\x", &s(&["claude"]))
                .expect_err("cwd")
                .field,
            "cwd"
        );
    }

    #[test]
    fn a_target_round_trips_and_omits_an_absent_state_dir() {
        let json = serde_json::to_value(target()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "distro": "lane-spike",
                "user": "lane-commysdk",
                "wrapper": "/usr/local/bin/lane-sandbox"
            })
        );
        assert_eq!(serde_json::from_value::<WslTarget>(json).unwrap(), target());
    }
}
