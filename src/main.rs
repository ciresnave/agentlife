// SPDX-License-Identifier: MIT OR Apache-2.0
//! `agentlife`: agent lifecycle control. Skeleton only; see README.md for the intended scope.

/// The crate version, as one string.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn main() {
    println!(
        "agentlife {}: skeleton, no commands implemented yet (see README.md)",
        version()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_the_manifest_version() {
        assert_eq!(version(), "0.1.0");
        assert!(!version().is_empty());
    }
}
