kind: additive
---
O22: a launcher-started lane now (a) reclaims its role with `synapse claim --role` from the lane home when the cached epoch is behind the roster and the role is offline (logged; an online role is never claimed over), and (b) gets a NOTICE in its first prompt, in `start --json` (`notice`) and in the launcher log when the role is held, the reclaim failed, or online state was unread. Red-first; mutations (claim call off, notice dropped from plan) each fail the new tests by name. Not run on a real lane.
