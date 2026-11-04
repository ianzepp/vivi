#!/bin/bash
# The lab repository's check: every unit file parses, and every unit's function
# runs and prints its own name. Fast, real, and red when a unit is broken.
set -eo pipefail
shopt -s nullglob
units=(src/*.sh)
for f in "${units[@]}"; do
  bash -n "$f"
  name=$(basename "$f" .sh)
  out=$(bash -c ". '$f' && $name")
  [ "$out" = "$name" ] || { echo "check: $f printed '$out', expected '$name'" >&2; exit 1; }
done
echo "check: ${#units[@]} unit(s) ok"
