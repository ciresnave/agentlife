// SPDX-License-Identifier: MIT OR Apache-2.0

//! How the OS says a process started, as one line for a mismatch log (`docs/PORT-LINUX-PLAN.md`,
//! slice P3').
//!
//! Issue #14 ("Linux `start_time` may differ by 1 s between reads") has no failing log, and the
//! only credible cause is the boot time moving under a stepped clock. This does not change what
//! an identity is. It only records, when an identity check refuses, the three numbers that tell a
//! clock step from a recycled pid: the start tick `/proc/<pid>/stat` reports (boot-relative, so a
//! step cannot move it), the boot time `/proc/stat` reports now, and the epoch start they give
//! together (what `sysinfo` derives).

/// The `starttime` field (22nd) of a `/proc/<pid>/stat` line, in clock ticks since boot.
///
/// The `comm` field (2nd) is parenthesised and may itself hold spaces or `)`, so the rest of the
/// line is read from the LAST `)`.
pub fn parse_stat_start_ticks(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    // after the `)`: state (3rd field), then 4th..; starttime is the 22nd, i.e. index 19 here
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// The `btime` line of `/proc/stat`: boot time, seconds since the epoch.
pub fn parse_btime(proc_stat: &str) -> Option<u64> {
    proc_stat
        .lines()
        .find_map(|l| l.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()
}

/// What the three numbers say, from text already read.
pub fn describe(stat: Option<&str>, proc_stat: Option<&str>) -> String {
    let ticks = stat.and_then(parse_stat_start_ticks);
    let btime = proc_stat.and_then(parse_btime);
    // CLK_TCK is 100 on every Linux this tool targets; stated rather than looked up
    let derived = match (ticks, btime) {
        (Some(t), Some(b)) => Some(b + t / 100),
        _ => None,
    };
    let f = |v: Option<u64>| v.map_or_else(|| "?".to_string(), |v| v.to_string());
    format!(
        "start readings: proc_start_ticks={} btime={} derived_epoch_start={} (CLK_TCK 100 assumed)",
        f(ticks),
        f(btime),
        f(derived)
    )
}

/// The readings for `pid` on this machine. On a non-Linux OS there is no `/proc`, and this says
/// so rather than inventing numbers.
pub fn start_readings(pid: u32) -> String {
    if cfg!(target_os = "linux") {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok();
        let proc_stat = std::fs::read_to_string("/proc/stat").ok();
        describe(stat.as_deref(), proc_stat.as_deref())
    } else {
        "start readings: not available off Linux".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `/proc/<pid>/stat` shape: 52 fields, `starttime` (22nd) = 37235868.
    const STAT: &str = "1234 (sh) S 1 1234 1234 0 -1 4194560 100 0 0 0 0 0 0 0 20 0 1 0 37235868 \
                        1000000 100 18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0 0";

    #[test]
    fn the_start_tick_is_the_22nd_field() {
        assert_eq!(parse_stat_start_ticks(STAT), Some(37235868));
    }

    /// The control that makes the field count trustworthy: a `comm` with spaces and parentheses
    /// must not shift it.
    #[test]
    fn a_comm_with_spaces_and_parentheses_does_not_shift_the_field() {
        let odd = STAT.replace("(sh)", "(a b) c (d)");
        assert_eq!(parse_stat_start_ticks(&odd), Some(37235868));
    }

    #[test]
    fn a_truncated_or_empty_line_is_none_not_a_guess() {
        assert_eq!(parse_stat_start_ticks("1 (x) S 1 2"), None);
        assert_eq!(parse_stat_start_ticks(""), None);
        assert_eq!(parse_stat_start_ticks("no parenthesis at all"), None);
    }

    #[test]
    fn btime_is_read_from_proc_stat() {
        assert_eq!(
            parse_btime("cpu  1 2 3\nbtime 1791631069\nprocesses 5\n"),
            Some(1791631069)
        );
        assert_eq!(parse_btime("cpu  1 2 3\n"), None);
    }

    #[test]
    fn the_description_carries_all_three_numbers_and_marks_a_missing_one() {
        let d = describe(Some(STAT), Some("btime 1000\n"));
        assert!(d.contains("proc_start_ticks=37235868"), "{d}");
        assert!(d.contains("btime=1000"), "{d}");
        assert!(d.contains("derived_epoch_start=373358"), "{d}"); // 1000 + 372358
        let missing = describe(None, Some("btime 1000\n"));
        assert!(missing.contains("proc_start_ticks=?"), "{missing}");
        assert!(missing.contains("derived_epoch_start=?"), "{missing}");
    }

    /// Where `/proc` exists the live read must agree with itself: this process has a start tick.
    #[cfg(target_os = "linux")]
    #[test]
    fn this_process_has_a_start_tick_on_linux() {
        let d = start_readings(std::process::id());
        assert!(!d.contains("proc_start_ticks=?"), "{d}");
        assert!(!d.contains("btime=?"), "{d}");
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn off_linux_it_says_so() {
        assert!(start_readings(std::process::id()).contains("not available off Linux"));
    }
}
