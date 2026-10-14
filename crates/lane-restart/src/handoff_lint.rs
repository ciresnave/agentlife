// SPDX-License-Identifier: MIT OR Apache-2.0

//! A warning, never a refusal, when a lane's HANDOFF is over its size ceiling.
//!
//! The ceiling is `CIRESNAVE-EXPECTATIONS.md` section 2.3c step 5: "20KB for the PM file, 8KB for
//! a lane's (measure with `wc -c`). Over the ceiling means step 1 [clean first] was skipped."
//! `assert-idle` runs right after a HANDOFF is written, which is the moment a lane can still fix
//! it. It only warns: a restart is mandatory at every task boundary (section 2.3b), and refusing
//! here could block one.
//!
//! "KB" is read as 1024 bytes. The expectations file does not say, so a file between 8000 and 8192
//! bytes is not flagged: the warning errs toward silence by 192 bytes rather than crying wolf.

/// 8 KB: a lane's HANDOFF.
pub const LANE_CEILING_BYTES: u64 = 8 * 1024;
/// 20 KB: the PM's HANDOFF.
pub const PM_CEILING_BYTES: u64 = 20 * 1024;

/// The ceiling that applies to `role`'s HANDOFF.
pub fn ceiling(role: &str) -> u64 {
    if role == "pm" {
        PM_CEILING_BYTES
    } else {
        LANE_CEILING_BYTES
    }
}

/// The warning for a HANDOFF of `bytes` at `path`, or `None` when it is within its ceiling.
pub fn warning(role: &str, path: &str, bytes: u64) -> Option<String> {
    let max = ceiling(role);
    (bytes > max).then(|| {
        format!(
            "WARNING: {path} is {bytes} bytes, over the {max}-byte ceiling for {} \
             (CIRESNAVE-EXPECTATIONS.md 2.3c step 5): a HANDOFF is rewritten, not appended to - \
             delete what is finished or stale, move what is worth keeping to its home, then re-run \
             assert-idle. Not a refusal: the restart is still allowed.",
            if role == "pm" { "the PM" } else { "a lane" }
        )
    })
}

/// Reads the size of `role`'s HANDOFF under `cwd` and returns the warning, if any. A missing
/// HANDOFF is not this lint's business (`stop` refuses that).
pub fn check(cwd: &str, role: &str) -> Option<String> {
    let (path, _) = crate::spec::find_handoff(cwd, role)?;
    let bytes = std::fs::metadata(&path).ok()?.len();
    warning(role, &path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ceiling_is_exact_and_the_pm_has_the_larger_one() {
        assert_eq!(LANE_CEILING_BYTES, 8192);
        assert_eq!(PM_CEILING_BYTES, 20480);
        assert_eq!(warning("agentlife", "H.md", 8192), None);
        assert!(warning("agentlife", "H.md", 8193).is_some());
        assert_eq!(warning("pm", "H.md", 20480), None);
        assert!(warning("pm", "H.md", 20481).is_some());
        // 8193 bytes is over a lane's ceiling but fine for the PM's.
        assert_eq!(warning("pm", "H.md", 8193), None);
    }

    #[test]
    fn the_warning_names_the_file_the_size_the_ceiling_and_the_section() {
        let w = warning("auth-framework", "C:/x/HANDOFF.md", 9000).unwrap();
        for needle in [
            "C:/x/HANDOFF.md",
            "9000 bytes",
            "8192-byte",
            "2.3c",
            "Not a refusal",
        ] {
            assert!(w.contains(needle), "{needle} missing from {w}");
        }
    }

    #[test]
    fn check_reads_the_real_file_and_a_missing_one_is_silent() {
        let d = tempfile::tempdir().unwrap();
        let cwd = d.path().to_string_lossy().into_owned();
        assert_eq!(
            check(&cwd, "x"),
            None,
            "no HANDOFF: not this lint's business"
        );
        std::fs::write(d.path().join("HANDOFF.md"), "a".repeat(100)).unwrap();
        assert_eq!(check(&cwd, "x"), None);
        std::fs::write(d.path().join("HANDOFF.md"), "a".repeat(9000)).unwrap();
        let w = check(&cwd, "x").unwrap();
        assert!(w.contains("9000 bytes"), "{w}");
    }
}
