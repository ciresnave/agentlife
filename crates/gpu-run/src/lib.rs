//! Pure parts of the native `gpu-run`: argument parsing, the log line, the lockfile JSON, the
//! stale-pid read and the timestamp. The protocol is the one in baracuda/fuel
//! `scripts/gpu-run.ps1` v3: mutex `Global\gpu-run`, metadata lockfile `%TEMP%\gpu-run.lock`,
//! contention log `%TEMP%\gpu-run.log`, nesting via `GPU_RUN_HELD` / `GPU_RUN_HELD_PID`.

use std::time::{SystemTime, UNIX_EPOCH};

/// Protocol constant shared by every participant. NEVER change it (a different name means no
/// mutual exclusion with the PowerShell copies).
pub const MUTEX_NAME: &str = r"Global\gpu-run";
/// The ps1 version whose behaviour this reproduces; written to the log and lockfile.
pub const GPU_RUN_VERSION: u32 = 3;
/// Exit code when the wait bound passes (EX_TEMPFAIL), same as the ps1.
pub const EXIT_WEDGED: i32 = 75;

#[derive(Debug, PartialEq, Eq)]
pub struct Invocation {
    pub project: String,
    pub command: Vec<String>,
}

/// `-Project <name> [--] <cmd> <args...>`. The project is the first argument; one optional `--`
/// follows; the rest is the command verbatim (its own flags and later `--` are kept).
pub fn parse_args(args: &[String]) -> Result<Invocation, String> {
    const USAGE: &str = "Usage: gpu-run -Project <name> -- <cmd> <args...>";
    let mut rest = args;
    let mut project = None;
    if rest.len() >= 2 && rest[0] == "-Project" {
        project = Some(rest[1].clone());
        rest = &rest[2..];
    }
    if !rest.is_empty() && rest[0] == "--" {
        rest = &rest[1..];
    }
    let project = match project {
        Some(p) if !p.trim().is_empty() => p,
        _ => {
            return Err(format!(
                "gpu-run: -Project <name> must be the first argument. {USAGE}"
            ))
        }
    };
    if rest.is_empty() {
        return Err(format!("gpu-run: no command given. {USAGE}"));
    }
    Ok(Invocation {
        project,
        command: rest.to_vec(),
    })
}

/// Seconds since the epoch as `2026-11-15T13:09:00.1234567Z` (the ps1 uses the local offset;
/// UTC says the same instant).
pub fn iso_utc(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:07}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        d.subsec_nanos() / 100
    )
}

/// `<ts> [<Project> v3 pid=<pid>] <msg>`, the ps1's line.
pub fn log_line(ts: &str, project: &str, pid: u32, msg: &str) -> String {
    format!("{ts} [{project} v{GPU_RUN_VERSION} pid={pid}] {msg}")
}

fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

/// The metadata lockfile: `{"v":1,"sv":3,"pid":..,"project":..,"cmd":..,"start":..}`, the same
/// keys in the same order as the ps1's `ConvertTo-Json -Compress`.
pub fn lock_json(pid: u32, project: &str, cmd: &str, start: &str) -> String {
    format!(
        "{{\"v\":1,\"sv\":{GPU_RUN_VERSION},\"pid\":{pid},\"project\":\"{}\",\"cmd\":\"{}\",\"start\":\"{}\"}}",
        json_escape(project),
        json_escape(cmd),
        json_escape(start)
    )
}

/// The holder pid out of a lockfile (`"pid"\s*:\s*(\d+)`), as the ps1's stale check reads it.
pub fn lock_pid(text: &str) -> Option<u32> {
    let i = text.find("\"pid\"")? + 5;
    let rest = text[i..].trim_start().strip_prefix(':')?.trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// `GPU_RUN_TIMEOUT_MIN`, default 45.
pub fn timeout_minutes(env: Option<&str>) -> u64 {
    env.and_then(|v| v.trim().parse().ok()).unwrap_or(45)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn args_project_then_separator_then_command() {
        let i = parse_args(&s(&[
            "-Project",
            "fuel",
            "--",
            "cargo",
            "test",
            "--",
            "--nocapture",
        ]))
        .unwrap();
        assert_eq!(i.project, "fuel");
        assert_eq!(i.command, s(&["cargo", "test", "--", "--nocapture"]));
    }

    #[test]
    fn args_without_separator_and_errors() {
        assert_eq!(
            parse_args(&s(&["-Project", "x", "echo"])).unwrap().command,
            s(&["echo"])
        );
        assert!(parse_args(&s(&["echo"])).is_err());
        assert!(parse_args(&s(&["-Project", "  ", "--", "echo"])).is_err());
        assert!(parse_args(&s(&["-Project", "x", "--"])).is_err());
        assert!(parse_args(&s(&[])).is_err());
    }

    #[test]
    fn timestamp_known_instants() {
        assert_eq!(iso_utc(UNIX_EPOCH), "1970-01-01T00:00:00.0000000Z");
        // 2026-11-15T13:09:00Z = 1794748140
        let t = UNIX_EPOCH + Duration::new(1_794_748_140, 123_456_700);
        assert_eq!(iso_utc(t), "2026-11-15T13:09:00.1234567Z");
        // leap day
        assert_eq!(
            iso_utc(UNIX_EPOCH + Duration::from_secs(951_782_400)),
            "2000-02-29T00:00:00.0000000Z"
        );
    }

    #[test]
    fn log_and_lock_shapes_match_the_ps1() {
        assert_eq!(
            log_line("T", "fuel", 7, "RUN: cargo test"),
            "T [fuel v3 pid=7] RUN: cargo test"
        );
        let j = lock_json(7, "fuel", r#"say "hi" \ x"#, "T");
        assert_eq!(
            j,
            r#"{"v":1,"sv":3,"pid":7,"project":"fuel","cmd":"say \"hi\" \\ x","start":"T"}"#
        );
        assert_eq!(lock_pid(&j), Some(7));
    }

    #[test]
    fn lock_pid_reads_ps1_json_with_spaces_and_rejects_garbage() {
        assert_eq!(lock_pid(r#"{"v":1,"pid" : 4242,"x":1}"#), Some(4242));
        assert_eq!(lock_pid("no json"), None);
        assert_eq!(lock_pid(r#"{"pid":"abc"}"#), None);
    }

    #[test]
    fn timeout_default_and_override() {
        assert_eq!(timeout_minutes(None), 45);
        assert_eq!(timeout_minutes(Some("90")), 90);
        assert_eq!(timeout_minutes(Some("x")), 45);
    }
}
