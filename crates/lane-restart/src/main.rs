// SPDX-License-Identifier: MIT OR Apache-2.0

//! The `lane-restart` binary: a wrapper around its library. All of the command line lives in
//! `lane_restart::cli::run`, so everything it does can be tested in-process.

fn main() -> std::process::ExitCode {
    lane_restart::cli::run(std::env::args_os().collect())
}
