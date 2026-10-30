#!/usr/bin/env bash
# Traceability gate (ARCH/04, ARCH/00):
#   1. every R-ID in ARCH/00_requirements_traceability.md is cited by >=1 AC
#   2. every AC cites >=1 known R-ID
#   3. every AC is governed by >=2 distinct tests
# Rust tests: fn ac_<M>_<m>_<n>_<name>      (AC<M>.<m>.<n>)   src/ and tests/
set -eu
cd "$(dirname "$0")/.."
MIN="${AC_MIN_TESTS:-2}"
fail=0

reqs=$(grep -oE '^\| R-[A-Z]+[0-9]+ ' ARCH/00_requirements_traceability.md | tr -d '| ' | sort -u)
rows=$(grep -hE '^\| AC[0-9]+\.[0-9]+\.[0-9]+ ' RELEASE/v*.md || true)
# Releases whose Status starts with "planned" are checked for requirement coverage only;
# their tests are written when the release is built.
active_rows=$(for f in RELEASE/v*.md; do grep -qE '^\*\*Status:\*\* planned' "$f" && continue; grep -hE '^\| AC[0-9]+\.[0-9]+\.[0-9]+ ' "$f"; done || true)
[ -n "$rows" ] || { echo "no AC rows in RELEASE/"; exit 1; }

# 1 + 2: requirements <-> ACs
cited=$(echo "$rows" | grep -oE 'R-[A-Z]+[0-9]+' | sort -u)
for r in $reqs; do
  echo "$cited" | grep -qx "$r" || { echo "UNCOVERED requirement $r (no AC cites it)"; fail=1; }
done
for c in $cited; do
  echo "$reqs" | grep -qx "$c" || { echo "UNKNOWN requirement $c cited by an AC"; fail=1; }
done
while IFS= read -r row; do
  id=$(echo "$row" | grep -oE '^\| AC[0-9]+\.[0-9]+\.[0-9]+' | tr -d '| ')
  echo "$row" | grep -qE 'R-[A-Z]+[0-9]+' || { echo "AC $id cites no requirement"; fail=1; }
done <<< "$rows"

# 3: tests per AC
count_tests() { # $1 = AC id e.g. AC0.1.3
  local id="$1" pre
  pre="ac_$(echo "${id#AC}" | tr . _)_"
  grep -rhoE "fn ${pre}[a-z0-9_]+" src tests 2>/dev/null | sort -u | wc -l | tr -d ' '
}
total=0; bad=0
for id in $(echo "$active_rows" | grep -oE '^\| AC[0-9]+\.[0-9]+\.[0-9]+' | tr -d '| ' | sort -u); do
  n=$(count_tests "$id"); total=$((total+1))
  if [ "$n" -lt "$MIN" ]; then echo "AC $id has $n test(s), needs >= $MIN"; bad=$((bad+1)); fi
done
[ "$bad" -eq 0 ] || fail=1
echo "ac-map: $total ACs, $(echo "$reqs" | wc -l | tr -d ' ') requirements, $bad under-tested"
exit $fail
