#!/bin/bash
# run-regress.sh <vivi binary> <label> [scenario...]: run each regression or
# mechanics scenario (default: every scenario with an [expect] table) on the
# given Linux vivi binary, one after another, and write one verdict line per
# scenario to runs/regress-<label>.txt. A scenario that expects no land waits
# its whole max, so the per-scenario max stays short.
set -uo pipefail
. "$(dirname "$0")/env.sh"
BIN=${1:?usage: run-regress.sh <vivi binary> <label> [scenario...]}
LABEL=${2:?usage: run-regress.sh <vivi binary> <label> [scenario...]}
shift 2
if [ $# -eq 0 ]; then
  set -- $(for f in "$LAB"/scenarios/*/plan.toml; do
    python3 -c "import tomllib,sys;sys.exit(0 if 'expect' in tomllib.load(open(sys.argv[1],'rb')) else 1)" "$f" \
      && basename "$(dirname "$f")"; done)
fi
SUMMARY="$LAB/runs/regress-$LABEL.txt"
mkdir -p "$LAB/runs"
echo "# $(date -u +%FT%TZ) binary $BIN" > "$SUMMARY"
for s in "$@"; do
  max=$(python3 -c "import tomllib,sys;print(tomllib.load(open(sys.argv[1],'rb')).get('expect',{}).get('max_secs',300))" "$LAB/scenarios/$s/plan.toml")
  start=$(date +%s)
  out=$(VIVI_BIN="$BIN" timeout $((max + 240)) "$LAB/run-multi.sh" "$s" "$max" 2>&1)
  verdict=$(printf '%s\n' "$out" | grep '^VERDICT' | tail -1)
  fails=$(printf '%s\n' "$out" | grep '^FAIL' | sed 's/^FAIL //' | paste -sd ';' -)
  dir=$(printf '%s\n' "$out" | awk '/^artifacts:/{print $2}')
  printf '%-14s %-22s %4ss  %s  %s\n' "$s" "${verdict:-VERDICT NONE}" "$(( $(date +%s) - start ))" "${fails:--}" "${dir##*/}" | tee -a "$SUMMARY"
done
"$LAB/down.sh" > /dev/null
