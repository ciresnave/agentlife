# agentlife-launcher

The decision engine of [agentlife](https://github.com/ciresnave/agentlife)'s launch-on-message
service: the policy (rules 1-7, the mechanism mapping, the replay and rate store, the
launch-stalled alarm) behind traits, with no network and no process code.

This crate was named `launcher` inside the agentlife workspace. That name on crates.io belongs to
another author's crate, so it is published as `agentlife-launcher`.

Licensed under MIT OR Apache-2.0.
