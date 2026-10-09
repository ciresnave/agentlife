// SPDX-License-Identifier: MIT OR Apache-2.0
//! lane-restart's old paths still work after the move to `lane-state`, and
//! they name the SAME types (not copies), so with-secret and the binary can
//! mix the two spellings.

#[test]
fn the_old_lane_restart_paths_are_the_lane_state_items() {
    fn same<T>(_: &T, _: &T) {}
    let a: Option<lane_restart::state::LaneState> = None;
    let b: Option<lane_state::state::LaneState> = None;
    same(&a, &b);
    let a: Option<lane_restart::facts::ProcEntry> = None;
    let b: Option<lane_state::facts::ProcEntry> = None;
    same(&a, &b);
    assert_eq!(
        lane_restart::paths::project_dir_name("C:/a b"),
        lane_state::paths::project_dir_name("C:/a b")
    );
    let a: Option<lane_restart::lane_state_writer::HookInput> = None;
    let b: Option<lane_state::claude_proc::HookInput> = None;
    same(&a, &b);
}
