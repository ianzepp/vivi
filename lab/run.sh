#!/bin/bash
# run.sh <scenario> [max seconds]: one full lab run, from fresh machines.
# Starts hub and worker, sets the hub up, starts the pump supervisor, files the
# scenario's need and its pump record, then waits until the need closes, the
# run locks or parks, or time runs out. Everything the run leaves behind is
# copied to lab/runs/<stamp>-<scenario>/.
set -euo pipefail
. "$(dirname "$0")/env.sh"
SCENARIO=${1:?usage: run.sh <scenario> [max seconds]}
MAX=${2:-600}
P=/srv/vivi/projects/lab
OUT="$LAB/runs/$(date -u +%Y%m%dT%H%M%SZ)-$SCENARIO"
mkdir -p "$OUT"
hub_vivi() { docker exec vivi-lab-hub runuser -u vivi -- env HOME=/home/vivi bash -c "cd / && $*"; }

"$LAB/up.sh" "$SCENARIO" 2>&1 | grep -v -e DEPRECATED -e buildx -e '^\s*$' || true
sleep 2
docker exec vivi-lab-hub /lab/hub/setup.sh > "$OUT/setup.log" 2>&1 || { tail -20 "$OUT/setup.log"; exit 1; }
docker exec -d vivi-lab-hub /lab/hub/start-service.sh

need=$(python3 - "$LAB/scenarios/$SCENARIO/plan.toml" <<'PY'
import sys, tomllib, shlex
n = tomllib.load(open(sys.argv[1], "rb"))["need"]
args = ["--subject", n["subject"], "--body", n["body"]]
for d in n["done_when"]:
    args += ["--done-when", d]
print(" ".join(shlex.quote(a) for a in args))
PY
)
H=$(hub_vivi "vivi need send --project $P --from operator --to hand --target app $need" | awk '/^created/{print $3}')
hub_vivi "vivi pump send --project $P --need $H" > /dev/null
echo "need $H sent at $(date -u +%T); waiting up to ${MAX}s"
start=$(date +%s)
state=timeout
while [ $(( $(date +%s) - start )) -lt "$MAX" ]; do
  sleep 5
  merged=$(hub_vivi "sqlite3 $P/.vivi/pump/pump.db \"select count(*) from flow_steps where need='$H' and node like 'merge%' and state='done' and verdict='merged'\"" 2>/dev/null || echo 0)
  if [ "${merged:-0}" -gt 0 ]; then state=landed; break; fi
  if hub_vivi "grep -qE 'flow-need-locked|flow-need-parked' $P/.vivi/pump/exceptions.txt" 2>/dev/null; then state=stuck; break; fi
done
elapsed=$(( $(date +%s) - start ))
echo "result: $state after ${elapsed}s"
echo "$state $elapsed" > "$OUT/result.txt"

# Artifacts: pump home, the flow view, the lane's main, the agent's call log.
docker exec vivi-lab-hub tar -C "$P/.vivi" -cf - pump mailspace.toml 2>/dev/null | tar -C "$OUT" -xf - || true
docker exec vivi-lab-hub cat /home/vivi/service.log > "$OUT/service.log" 2>/dev/null || true
hub_vivi "vivi flow show build --project $P --need $H" > "$OUT/flow.txt" 2>&1 || true
hub_vivi "git -C /srv/vivi/lanes/lab/app.git log --oneline --graph --all -60" > "$OUT/lane.txt" 2>&1 || true
hub_vivi "sqlite3 -header $P/.vivi/pump/pump.db 'select node, state, verdict from flow_steps order by rowid'" > "$OUT/steps.txt" 2>&1 || true
docker exec vivi-lab-worker tar -C /home/worker -cf - sim-agent 2>/dev/null | tar -C "$OUT" -xf - || true
echo "artifacts: $OUT"
