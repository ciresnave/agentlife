# Mail before the first turn: measurement and design (DESIGN ONLY, no code)

Written 2026-11-25 for the PM TASKs `mail-before-first-turn` (00:45Z) and `mail-summary-design` (00:30Z) via pm-platform. CireSnave, 2026-11-25, verbatim: *"The ideal solution would be to have mail delivered as soon as the lane connects to Synapse rather than several seconds later after it has read its HANDOFF and thinks it has nothing to do."*

## 1. Measurement

**Predicate:** for every lane session whose first user entry is the mail-start prompt ("You were started because mail arrived"), the seconds from that first entry to the first later user entry containing `<channel source="synapse"` (a channel event reaching the session), and to the first assistant entry. Read from `~/.claude/projects/C--Projects-*/*.jsonl`, 312 sessions, newest 2026-11-25 01:18Z (`tools/mail-first-latency.py`, rerunnable; one machine, one run, `python -I`).

| what | result |
|---|---|
| first assistant turn after the prompt | median 3.1 s: the lane has begun its first turn almost at once |
| sessions that ever saw a channel event | 237 of 312 (the other 75 read the mail some other way or got none) |
| first channel event 110-130 s after the prompt | **151 of 237 (64%)**; 230-250 s: 16; 30-110 s: 26; 130-230 s: 2; over 250 s: 41; **under 30 s: 1** (min 22 s) |

**Reading.** The delay is not connect latency or first-turn timing. The first event a session sees lands at **about 120 s**, the synapse ack-timeout redelivery ("sent again after about 2, 4, 8 and 16 minutes", the 4 min bucket is the second redelivery). So the first delivery, made when the channel connects, does **not reach the session as an event**, and the message sits leased to it until the retry. That also explains the "empty inbox" fetch (fc7b000): the message is leased, so `fetch` returns nothing. **[inferred, not yet measured: why the first delivery is lost.]** Leading candidates: (a) the adapter pushes it while claude is still on the development-channels / trust dialogs, which the host answers a few seconds in, so the notification has no listener; (b) the adapter pushes before the MCP session is initialised. Both are on synapse's or claude's side of the line; this lane cannot tell them apart from transcripts.

**Probe that decides it (needs the launchtest role and the PM, a start with one mail queued):** record three timestamps for one start: the daemon's lease/deliver of the message, the host log's moment the dev-channels dialog is answered, and the first channel entry in the transcript. Delivery before the dialog answer = (a). Not run yet.

## 2. Design: which mechanism

- **A (host withholds the first prompt until the channel is up)**: does not fix the cause. The prompt is the lane's *first turn*, not the delivery; if the first delivery is dropped, a lane that starts later still sees nothing until the 120 s retry. A only helps if the drop is purely "lane got busy first", which the 3 s first turn and the 1-in-237 under-30-s count argue against.
- **B (adapter flushes the queue on connect, after claude can listen; host waits for that signal)**: fixes the cause if the probe shows (a) or (b): deliver (or redeliver) when the session is ready, not when the TCP connection opens. **Recommended**, and it is synapse's code: a request to the synapse lane to (1) hold the first delivery until claude has accepted the channel, or (2) expose an on-demand `redeliver` the host can call right after the dialog handler answers. Option (2) is the smaller change and keeps the host's role to "tell the adapter the session is ready".
- **C (fc7b000, wait ~2 min)**: shipped in 0.30.86 as the backstop. It converts "empty inbox, stop" into "wait for the retry", so it matches the measured 120 s: not a fix, a floor under the failure.
- **Failure mode for B:** a channel that never connects (the alarmant epoch case: cached epoch behind the claim counter, 409 superseded on every call, adapter never re-claims) must not hold the prompt. The host waits at most ~20 s for the ready signal, then submits the prompt and prints one visible line, `channel not ready after 20 s: mail will arrive on retry or by fetch`. The prompt text keeps C's instruction.

## 3. Peek (mail-summary-design)

PM facts: synapse's CLI `inbox` leases; no read-only peek; reading another role's inbox means acting as that role; the launcher knows only `pending` counts and the oldest enqueue time. **Answer: no, not today.** The launcher cannot read a stopped role's waiting text without leasing it (which is what produces the lost-first-delivery state above), and acting as the role claims it. **Blocker, for synapse:** a read-only `peek` (list without lease or delivery-count bump), granted to the launcher's identity only. If synapse builds it, the text goes in a bounded summary labelled UNTRUSTED in the start prompt (sender role, message id, the L1 header line, at most N note characters), or in a `MAIL-PENDING.md` that no prompt tells the lane to follow and that is deleted after the ack; **never in HANDOFF** (PM position, agreed: HANDOFF is a trusted, lane-authored instruction file). With B working, a summary adds little; build it only if the probe shows B cannot be had.

## 4. Questions for the PM

1. Approve asking the synapse lane for (B)(2) `redeliver` / hold-until-ready, and for a launcher-only `peek`.
2. Run the probe on a `launchtest` start (who schedules it).
3. O22(a), policy: may the launcher claim automatically from the lane home when its cached epoch is behind the claim counter? A claim supersedes any live session of the same role.
