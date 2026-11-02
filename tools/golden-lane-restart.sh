#!/bin/bash
# thin-main golden-output check (PM ruling 2026-11-01: each PR of the series keeps behaviour identical).
# Usage: tools/golden-lane-restart.sh <lane-restart built from main> <lane-restart built from the branch>
# Runs hermetic invocations (own temp dir each, no lane state) against both and diffs stdout, stderr
# and exit code. Add a case here with every command a PR moves. Positive control: pass a wrapper
# that changes one output as <after> and it reports DIFF.
BEFORE="$1"; AFTER="$2"
fail=0; n=0
tmp=$(mktemp -d)
mk() { d="$tmp/$1"; mkdir -p "$d"; [ -n "$2" ] && printf '%b' "$2" > "$d/HANDOFF.md"; echo "$d"; }
GOOD='WAIT: a merge | check: gh pr view 1 --json state,mergedAt | wake: 2099-01-01T00:00:00Z\n'
BAD='WAIT: a merge | check: gh pr view 1 --json state,mergedAt\n'
run_case() { # name dir env-assignments... -- args...
  name="$1"; dir="$2"; shift 2
  envs=(); while [ "$1" != "--" ]; do envs+=("$1"); shift; done; shift
  for which in before after; do
    exe="$BEFORE"; [ $which = after ] && exe="$AFTER"
    ( cd "$dir" && env -u LANE_ROLE "${envs[@]}" "$exe" "$@" > "$tmp/$name.$which.out" 2> "$tmp/$name.$which.err"; echo $? > "$tmp/$name.$which.rc" )
  done
  n=$((n+1))
  for f in out err rc; do
    # Temp dir paths differ between runs of the same case only by the case name, so they match.
    if ! cmp -s "$tmp/$name.before.$f" "$tmp/$name.after.$f"; then
      echo "DIFF $name ($f)"; diff "$tmp/$name.before.$f" "$tmp/$name.after.$f" | head -5; fail=1
    fi
  done
}
E=$(mk empty "")
N=$(mk nowaits "# nothing\n")
G=$(mk good "$GOOD")
B=$(mk bad "$BAD")
run_case help "$E" -- --help
run_case h "$E" -- -h
run_case version "$E" -- --version
run_case V "$E" -- -V
run_case badflag "$E" -- --no-such-flag
run_case waits_nohandoff "$E" -- waits --role r
run_case waits_none "$N" -- waits --role r
run_case waits_good "$G" -- waits --role r
run_case waits_bad "$B" -- waits --role r
run_case waits_envrole "$G" LANE_ROLE=viaenv -- waits
run_case waits_leaf "$G" -- waits
run_case waits_flag_beats_env "$G" LANE_ROLE=viaenv -- waits --role flagged
run_case waits_badarg "$G" -- waits --bogus
run_case waits_missing_value "$G" -- waits --role
run_case idle_refused "$B" -- assert-idle --role goldenrole-$$
run_case idle_nostate "$G" -- assert-idle --role goldenrole-$$
run_case idle_nohandoff "$E" -- assert-idle --role goldenrole-$$
run_case idle_badarg "$B" -- assert-idle --bogus
# waits-poll: only its argument errors and help are hermetic (a real pass reads the machine's
# lane state), so those are the cases; the pass itself is tested in-crate with a fixture ctx.
run_case poll_badflag "$E" -- waits-poll --bogus
run_case poll_positional "$E" -- waits-poll extra
run_case poll_help "$E" -- waits-poll --help
run_case poll_dry_run_help "$E" -- waits-poll --dry-run -h
# status: same rule (a real `status` reads the machine's lane state); the argument errors and help.
run_case status_badflag "$E" -- status --bogus
run_case status_quiet_missing "$E" -- status --quiet-over
run_case status_quiet_text "$E" -- status --quiet-over x
run_case status_quiet_zero "$E" -- status --quiet-over 0
run_case status_positional "$E" -- status extra
run_case status_help "$E" -- status --json -h
# ledger: a real `ledger` reads the machine's restart.log (it only grows, so the real-path
# comparison is made back to back outside this harness); here, the argument errors, the
# --since parse error (raised before the log is read) and help.
run_case ledger_badflag "$E" -- ledger --bogus
run_case ledger_last_text "$E" -- ledger --last x
run_case ledger_since_bad "$E" -- ledger --since soon
run_case ledger_since_missing "$E" -- ledger --since
run_case ledger_positional "$E" -- ledger extra
run_case ledger_help "$E" -- ledger --by-lane -h
# approvals: a real run fetches the approvals repo over the network, so the hermetic cases are the
# ones that stop before any fetch: no home, no config file, a broken or invalid config, and the
# argument errors. (The home is `USERPROFILE`, else `HOME`.)
H0="$tmp/home-empty"; mkdir -p "$H0"
H1="$tmp/home-brokenjson"; mkdir -p "$H1/.overmind"; printf '{nope' > "$H1/.overmind/lane-restart.json"
H2="$tmp/home-badrepo"; mkdir -p "$H2/.overmind"
printf '{"approvals": {"repo": "not a repo", "path": ".lane/approvals"}}' > "$H2/.overmind/lane-restart.json"
run_case approvals_nohome "$E" -u USERPROFILE -u HOME -- approvals
run_case approvals_noconfig "$E" USERPROFILE="$H0" -- approvals
run_case approvals_noconfig_role "$E" USERPROFILE="$H0" -- approvals --role overmind
run_case approvals_brokenjson "$E" USERPROFILE="$H1" -- approvals
run_case approvals_badrepo "$E" USERPROFILE="$H2" -- approvals
run_case approvals_badflag "$E" USERPROFILE="$H0" -- approvals --bogus
run_case approvals_role_missing "$E" USERPROFILE="$H0" -- approvals --role
run_case approvals_positional "$E" USERPROFILE="$H0" -- approvals extra
run_case approvals_help "$E" -- approvals --role r -h
echo "golden cases: $n; differing: $fail (0 = every stdout, stderr and exit code identical)"
rm -rf "$tmp"
exit $fail
