#!/bin/bash
# The lab repository's check: every unit file parses, and every unit's function
# runs and prints its own name. Fast, real, and red when a unit is broken.
#
#   check.sh [--label L] [--fail-pct P]
#
# With --fail-pct, the check also fails at random in P percent of runs, each
# run drawing on its own. That stands in for a real test that catches bad work:
# the rung goes red and the pump does what it does with any red. The label
# (task, need, ...) names the rung in the failure line the pump records.
set -eo pipefail
label=check
fail_pct=0
while [ $# -gt 0 ]; do
  case "$1" in
    --label) label=$2; shift 2 ;;
    --fail-pct) fail_pct=$2; shift 2 ;;
    *) echo "check: unknown argument $1" >&2; exit 2 ;;
  esac
done
shopt -s nullglob
units=(src/*.sh)
for f in "${units[@]}"; do
  bash -n "$f"
  name=$(basename "$f" .sh)
  out=$(bash -c ". '$f' && $name")
  [ "$out" = "$name" ] || { echo "check: $f printed '$out', expected '$name'" >&2; exit 1; }
done
# One draw in 0..9999 against the rate in basis points, so 12.5 works.
draw=$(( $(od -An -N4 -tu4 /dev/urandom) % 10000 ))
limit=$(awk -v p="$fail_pct" 'BEGIN { printf "%d", p * 100 }')
if [ "$draw" -lt "$limit" ]; then
  echo "check: injected $label failure (draw $draw < $limit of 10000)" >&2
  exit 1
fi
echo "check: ${#units[@]} unit(s) ok ($label)"
