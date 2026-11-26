# Footer-only dialog stalls: pty size (design note, 2026-11-26)

PM FINDING toupv5-jds-jv / -hx / -i8. Launcher starts stall on the dev-channels dialog showing only the
footer; hand starts work. Captures: the child redraws after `ESC[8;2;46t` (2 rows x 46 cols) or in a
~10-column pty.

## Measured now (this PR)
The host log gets `PTY SIZE at creation: CxR, source: ...` (console query vs the 80x25 fallback) and one
`PTY SIZE ...` line for every resize the host sets: nudge wider, nudge back, and "follows the host's own
console (was AxB)". A failed resize is logged. Compare a stalled start's lines with a successful one.

## Hypothesis (not proven)
The resize thread polls `current_console_size()` every 500 ms and forwards any change to the pty. In a
launcher start (scheduled task, hidden/new console) that query can report a degenerate window, which the
thread then applies to the pty: the first nudge falls in the same loop pass, which matches the 2x46
arriving ~0.3 s after the first REPAINT in the 02:57 capture and the size varying between starts.
The new "follows the host's own console" lines confirm or refute this.

## Proposed (needs PM read before code)
1. Guard: a polled size under 10 rows or 40 columns is logged and NOT forwarded; the pty keeps its size.
2. Fallback: if the screen is footer-only for N s (proposal 20 s) after the guard, the host answers the
   warning with option 1 (Enter) exactly as the handler would, logged as FALLBACK-ANSWER, once per start.
   Rationale: the footer is the dev-channels dialog's own; option 1 is the default. Risk: answering a dialog
   the model never matched; limit to footer text that is exactly "Enter to confirm . Esc to cancel".
3. Test: a fake pty whose console query returns 2x46 must leave the pty size unchanged.
