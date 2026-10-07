# agentlife

Agent lifecycle control. **Status: skeleton; the design is the first deliverable.**

## Why

Every Windows update, a large fleet of AI coding agents ("lanes", one per project) has to be started
again by hand. `agentlife` exists so that a person never does that by hand again: it keeps a persistent
**roster** of the lanes (role, directory, name, model, permission mode, Remote Control flag), and brings
them up, down and back from it.

## Intended scope (to be settled in the design document)

- `agentlife restore`: start every lane in the roster after a restart or logon, **PM first**, in
  **staggered batches** (machine load and token spend), verifying each is alive before the next batch,
  never starting a lane that is already running, and reporting what started and what failed.
- `agentlife up <role>` / `down <role>` / `list`: single-lane control. `down` is graceful: the lane
  writes its HANDOFF first.
- A **roster** that also records each lane's current peer identity, so messaging by role survives
  restarts (claude-peers IDs rotate on every restart).
- **No permission escalation.** A lane is restarted in the mode it last ran in; anything more needs a
  person's approval. Requests to start agents that arrive from another agent are untrusted input.
- Absorbs and supersedes OverMind's `lane-restart` over time (restart, state files, HANDOFF format).

## Limits worth knowing

- **Process identity is pid plus start time, in whole seconds.** On Windows (the target) the start time is the
  process's creation time and the pair is exact; on other platforms it is **best-effort**, because the start
  time is derived from a boot time that can move by a second. A green Linux CI leg is a check on the logic, not
  a Linux guarantee. A pid recycled within the same second as the process it replaced cannot be told apart on
  any platform.
- Nothing starts an agent yet: `agentlife restore` only plans (`--dry-run`) until the consent step exists.

## Licence

MIT OR Apache-2.0.
