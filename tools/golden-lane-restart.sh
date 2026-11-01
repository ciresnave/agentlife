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
echo "golden cases: $n; differing: $fail (0 = every stdout, stderr and exit code identical)"
rm -rf "$tmp"
exit $fail
