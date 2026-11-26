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

## Built (PM ruling toupv5-jds-jw, 2026-11-26)
1. Guard: a polled console size under 10 rows or 40 columns is logged (`PTY SIZE ignored a degenerate ...`) and
   NOT forwarded; the pty keeps its size. Test: `a_degenerate_polled_console_size_is_never_forwarded_to_the_pty`
   (pure decision function, 2x46 and ~10-wide inputs, with real-change and unchanged controls).
2. REDRAW, not an answer: after 2 repaints with the screen still footer-only, the pty is resized ONCE per start to
   120x30 and the existing handlers match the full dialog when it repaints. Logged as `REDRAW`. Still footer-only
   after that: nothing further, the lane needs a person.
3. A dialog the handlers did not match is NEVER answered: the footer "Enter to confirm . Esc to cancel" is shared by
   every confirmation dialog, so answering on footer text alone would be a blind answer. The earlier Enter
   fallback proposal is withdrawn.
