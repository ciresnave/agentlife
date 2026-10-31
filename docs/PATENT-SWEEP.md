# Patent sweep: agentlife 0.29.60 @ ae50a09

Date: 2026-10-31; Run by: agentlife lane, Claude Sonnet 5.5; Method: AI-assisted web search of patents.google.com results and claim reading with `tools/patent-claims.py`. **A due-diligence read, NOT legal advice and NOT a freedom-to-operate opinion.** It is **thin**: one search per idea, the first page of results each, claims read for seven patents. Nothing here says the project is clear of any patent; it says what was looked at, and what was not.

Standing instruction: *"Ensure a patent sweep has been done of every one of our projects before release."* (CireSnave, 2026-10-25; process: `C:\Projects\PATENT-SWEEP-PROCESS.md`). The wording below follows the PM's instruction of 2026-10-31: per patent, "outside as read", never "clear" or "no infringement".

Version line covered: 0.29 (before 1.0 the second number is the major). `agentlife` is the lifecycle core plus the Claude Code adapter (`lane-restart`, `lane-state`, `launcher`, `runner-lint`, `pin-plan`); the ideas below are the mechanisms, not the features.

## Ideas swept

1. **Plan-hash-bound, one-use human approval for restoring a fleet of agent sessions.** A restore is planned, the plan is hashed, and a person approves exactly that hash; the approval is spent once, before the action it covers, and is only ever the one the approval channel issued (the type cannot be constructed by the restoring program, and is not copyable).
2. **Process liveness by identity, not file age.** A session is live only if the recorded process id is a live process of the right kind AND its start time equals the recorded start time; a second launch of the same role is refused by reading the process table for a host or an agent process carrying the role's name, whatever the state file says.
3. **Write-ahead restart specification.** Before an agent session is stopped, a complete launch specification is written to disk, read back and checked; the session may be stopped only if it can be started again from that file; the new session is launched from the file, not from the old session's state.
4. **Startup-dialog answering in a pseudo-terminal host.** A host owns the session's pseudo-terminal, relays every byte unchanged, and only during a startup window compares the screen text to provenance-checked handlers and injects the matching handler's keystrokes; an unanswered dialog is reported to a person.
5. **Agent-asserted quiescence before a restart.** A restart is authorised only when the agent itself has asserted (after writing its hand-off note, size-limited) that no background process it started is still running, together with the not-busy and no-sub-agent state from its own hooks.
6. **Soft and hard stop classification with a signed marker.** A stop records whether the agent ended in the course of its own work (a message may wake it) or was stopped on purpose (it stays stopped until a person or the project manager starts it); the soft marker is signed so it cannot be forged by editing the file.
7. **Wake-on-message launcher service with caps.** A service starts a stopped agent when a message arrives for it, subject to a per-role start lock, a rate and concurrency cap, a free-memory floor, and the stop classification of idea 6.
8. **Launch command rebuilt from an allowlist.** A restored session's command line is rebuilt from an allowlist of flags parsed from the recorded one (dropping and listing everything else, refusing a more permissive permission mode than the session last ran in), so a restore cannot widen what was granted.

## Queries (source, query, date)

All: Google Patents results through the web-search tool (`allowed_domains: patents.google.com`), 2026-10-31. One query per idea; the tool is US-oriented and returns about ten results.

1. `patent restoring autonomous AI agent sessions after reboot plan hash bound one-time human approval`
2. `patent process liveness start time pid reuse prevent duplicate launch of agent session process table`
3. `patent persist launch specification to disk and verify before terminating process so it can be restarted write-ahead restart`
4. `patent automatically answering terminal startup dialog by matching screen text pseudo terminal host injecting keystrokes allowlisted handler`
5. `patent software agent asserts quiescent no background tasks before restart authorized handoff document`
6. (idea 6 shares query 7's search; a separate query for the signed soft/hard marker was **not run**)
7. `patent wake stopped software agent on incoming message launcher service resource cap memory floor soft stop hard stop`
8. `patent relaunch command line rebuilt from allowlist of flags dropping unrecognized arguments prevent privilege escalation on restore`
9. (idea 1, second angle) `patent consent approval token cannot be constructed outside issuing channel unforgeable approval type linear single use`

## Hits

"Read in full" = every claim of the patent was read as printed by `tools/patent-claims.py`. "Claim 1, first ~900 characters" = the independent claim 1 was cut off by my own display limit and the remaining claims were **not read**: those are **UNREAD** beyond that. Status (in force, lapsed, expired) was **not checked for any patent**; "outside as read" says nothing about whether a patent is alive.

| # | patent | title | assignee | status | claim(s) read | overlap | our difference | verdict |
|---|---|---|---|---|---|---|---|---|
| 1 | US20160004560A1 | Method for singleton process control | not recorded | published application, status not checked | all 9 claims, read in full | a background process's id is used, not the program name, so only one instance starts | claim 1 needs a parent process storing the last spawned process's id in a first temporary file and a second, global temporary file; we use no id files and compare the process table by id AND start time | outside as read |
| 2 | US20020184295A1 | Method for mutual computer process monitoring and restart | not recorded | published application, status not checked | all 8 claims, read in full | restarts a process that ended abnormally | claim 1 is a monitoring policy in which one process monitors another and restarts it on abnormal termination; ours has no mutual or ring monitoring, and a restart is a person's or the launcher's act from a recorded spec | outside as read |
| 3 | US9565218B2 | Resource management for WebRTC | not recorded | granted, status not checked | all 7 claims, read in full | a sleeping client is woken when a message arrives for it | claim 1 is a gateway closing a connection, a push notification server, a JSON session id and a token for a WebRTC application; ours starts a local process under a start lock, caps and a memory floor | outside as read |
| 4 | US11755364B2 | Transferral of process state and/or components in computing environments | not recorded | granted, status not checked | claim 1, first ~900 characters; other claims UNREAD | a second instance replaces the first | claim 1 is an operating-system function that sends the first process an update notice so it captures its state and resources and hands them to the new instance; ours transfers no process state (a fresh session starts from a written specification) | outside claim 1 as read in part; remaining claims UNREAD |
| 5 | US7478434B1 | Authentication and authorization protocol for secure web-based access to a protected resource | not recorded | granted, status not checked | claim 1, first ~900 characters; other claims UNREAD | a one-time-use piece of data bound to a request | claim 1 is a client generating one-time data bound to an identity in a cookie set by an authentication server, validated by a web server; ours is a local approval bound to a plan hash, issued by a person at a prompt | outside claim 1 as read in part; remaining claims UNREAD |
| 6 | US11100278B2 | Systems and methods for presentation of a terminal application screen | not recorded | granted, status not checked | claim 1, first ~900 characters; other claims UNREAD | compares terminal screen text with identification text | claim 1 selects a transformation template and renders the screen as an HTML page on a client; ours sends keystrokes back to the host for a matched, allowlisted dialog | outside claim 1 as read in part; remaining claims UNREAD |
| 7 | US7761873B2 | User-space resource management | not recorded | granted, status not checked | claim 1, first ~1000 characters; other claims UNREAD | a limit on a resource and a reaction when it is exceeded | claim 1 is a user-space process paging memory pages of a workload out when a limit is exceeded; ours refuses to start a session when free memory is under a floor and pages nothing | outside claim 1 as read in part; remaining claims UNREAD |

## Seen in the results, UNREAD

Listed so that nothing in the results is silently dropped. None of these was opened beyond the title and the search snippet; **UNREAD**:

- Idea 1 and 9: EP3289505A1 (resumption of logon across reboots), US12450494B1 (validating autonomous AI agents), US12412138B1 (agentic orchestration), US7627895B2 (trust tokens), US20160232527A1 (token processing utilizing multiple authorizations), US12719671 ("Consent chain certification"; the closest to consent, and unread).
- Idea 2: US20100138189A1, WO2017001916A1, US6286025B1, US20020116151A1, WO2000022527A1.
- Idea 3: WO2005071522A1, USRE42936E1, US6205450B1, WO2015095427A1, US20050177832A1.
- Idea 4: US5627977 (an old "trainable user interface translator" that synchronises a pseudo-terminal host on a screen string; likely lapsed, **not checked**), US20030120838A1.
- Idea 5: US20120260118A1 and family (management of background tasks).
- Idea 7: US7761873 family and EP0723236B1 (agent processes sleeping and waking).
- Idea 8: US8239688B2 (securely recovering a computing device), US20210294909A1.

Non-patent results (arXiv papers on agent hand-offs, human-in-the-loop approval, session persistence) are prior art, not patents, and were not assessed here.

## Needs review (to the PM)

None *as read*. That is a statement about seven patents and about claim text, not about the project. The closest unread documents for the consent idea (US12719671) and for the screen-matching idea (US5627977, expiry unchecked) should be read before a first publish.

## Limitations

- **Thin.** One search per idea, first page only, seven patents read, four of them only in part. Idea 6 (signed soft/hard marker) had no search of its own.
- **Not a patent database search.** The web-search tool is US-oriented and returns about ten results; USPTO, EPO/Espacenet and WIPO were not queried directly, nor were non-English or non-US publications.
- **Status unchecked.** Whether any hit is granted, lapsed or expired was not checked.
- **AI-assisted reading.** Read by a model; a person who knows the claims should confirm any "outside as read".
- **Prior-art papers are not part of this**, and the evidence index CodeRipper's `patent-index-match` uses (a dependency-level check) is a different thing from this sweep.
- Before a first publish of any crate from this repository: repeat with the databases above, read the UNREAD list, and have the project manager check the version line this file names against the manifest.
