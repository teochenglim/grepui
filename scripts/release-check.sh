#!/usr/bin/env bash
# Verify one release's ACs: every AC row has >=2 tests and they pass.
#   scripts/release-check.sh 0.1.0
set -uo pipefail
cd "$(dirname "$0")/.."
REL="${1:?usage: release-check.sh X.Y.Z}"
FILE="RELEASE/v$REL.md"
[ -f "$FILE" ] || { echo "no $FILE"; exit 2; }
echo "== v$REL: $(grep -cE '^\| AC[0-9]' "$FILE") ACs"
fail=0
for id in $(grep -oE '^\| AC[0-9]+\.[0-9]+\.[0-9]+' "$FILE" | tr -d '| '); do
  pre="ac_$(echo "${id#AC}" | tr . _)_"
  n=$(grep -rhoE "fn ${pre}[a-z0-9_]+" src tests 2>/dev/null | sort -u | wc -l)
  printf '  %-10s %2d tests\n' "$id" "$n"; [ "$n" -ge 2 ] || fail=1
done
IFS=. read -r MA MI _ <<<"$REL"
cargo test --locked -q "ac_${MA}_${MI}_" 2>&1 | tail -5 || fail=1
exit $fail
