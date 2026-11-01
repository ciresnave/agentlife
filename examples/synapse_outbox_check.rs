// SPDX-License-Identifier: MIT OR Apache-2.0
//! Scratch-daemon check of the notify-outbox -> Synapse path (docs/LAUNCHER-SERVICE.md).
//! `SYNAPSE_HOME` / `SYNAPSE_ADDR` must name a THROWAWAY home; argument 1 is a scratch state dir.
//! Queues one message to `pm` and one to a role nobody claimed, relays the queue twice, then
//! sends the first message again under its own `message_id`.

use agentlife::launcher_service::{relay_outbox, SynapseSource};
use std::path::Path;

fn queued(state: &Path) -> Vec<String> {
    let dir = state.join("notify-outbox");
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn main() {
    let state = std::path::PathBuf::from(std::env::args().nth(1).expect("scratch state dir"));
    let home = synapse::keystore::default_home().expect("SYNAPSE_HOME");
    let text = format!("g6 scratch check {}", chrono::Utc::now().timestamp());
    lane_restart::notify::enqueue(&state, "pm", &text).unwrap();
    lane_restart::notify::enqueue(&state, "unclaimed-role", "should stay queued").unwrap();
    println!("queued before: {:?}", queued(&state));
    let source = SynapseSource::connect(&home).expect("connect");
    println!("relay 1 (sent, kept): {:?}", relay_outbox(&state, &source));
    println!("queued after 1: {:?}", queued(&state));
    println!("relay 2 (sent, kept): {:?}", relay_outbox(&state, &source));
    for f in queued(&state) {
        println!(
            "{f}: {}",
            std::fs::read_to_string(state.join("notify-outbox").join(&f)).unwrap()
        );
    }
}
