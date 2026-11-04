#!/bin/bash
# run-multi.sh <scenario> [max seconds]: one lab run of a multi-need scenario
# (gen-multi.py), from fresh machines. Like run.sh, but files every `[[need]]`
# and its pump record, then waits until each need has landed or stopped
# (locked or parked), or time runs out. Everything the run leaves behind is
# copied to lab/runs/<stamp>-<scenario>/, with needs.txt: one line per need,
# `<plan id> <handle> landed|stuck|open`.
set -euo pipefail
. "$(dirname "$0")/env.sh"
SCENARIO=${1:?usage: run-multi.sh <scenario> [max seconds]}
MAX=${2:-1800}
P=/srv/vivi/projects/lab
OUT="$LAB/runs/$(date -u +%Y%m%dT%H%M%SZ)-$SCENARIO"
mkdir -p "$OUT"
hub_vivi() { docker exec vivi-lab-hub runuser -u vivi -- env HOME=/home/vivi bash -c "cd / && $*"; }

"$LAB/up.sh" "$SCENARIO" 2>&1 | grep -v -e DEPRECATED -e buildx -e '^\s*$' || true
sleep 2
docker exec vivi-lab-hub /lab/hub/setup.sh > "$OUT/setup.log" 2>&1 || { tail -20 "$OUT/setup.log"; exit 1; }
docker exec -d vivi-lab-hub /lab/hub/start-service.sh

# One line per need: plan id, then the shell-quoted send arguments.
python3 - "$LAB/scenarios/$SCENARIO/plan.toml" > "$OUT/sends.txt" <<'PY'
import sys, tomllib, shlex
for n in tomllib.load(open(sys.argv[1], "rb"))["need"]:
    # One line per need: a body's newlines become spaces (the stand-in reads
    # the LAB-NEED marker as one token either way).
    args = ["--subject", n["subject"], "--body", " ".join(n["body"].split("\n"))]
    for d in n["done_when"]:
        args += ["--done-when", d]
    print(n["id"], n.get("after", "-"), " ".join(shlex.quote(a) for a in args))
PY
: > "$OUT/needs.txt"
handle_of() { awk -v id="$1" '$1 == id {print $2}' "$OUT/needs.txt"; }
landed_need() { # landed_need <handle>: 0 when its merge to main is done
  local n
  n=$(hub_vivi "sqlite3 $P/.vivi/pump/pump.db \"select count(*) from flow_steps where need='$1' and node like 'merge%' and state='done' and verdict='merged'\"" 2>/dev/null || echo 0)
  [ "${n:-0}" -gt 0 ]
}
while read -r id after args; do
  # `after = "<need id>"`: send this need only once that one has landed, so
  # its read-only roles must see landed work on the target's main.
  if [ "$after" != "-" ]; then
    prior=$(handle_of "$after")
    waited=0
    until [ -n "$prior" ] && landed_need "$prior"; do
      sleep 5; waited=$((waited + 5))
      [ "$waited" -ge "$MAX" ] && break
    done
  fi
  H=$(hub_vivi "vivi need send --project $P --from operator --to hand --target app $args" | awk '/^created/{print $3}')
  hub_vivi "vivi pump send --project $P --need $H" > /dev/null
  echo "$id $H" >> "$OUT/needs.txt"
done < "$OUT/sends.txt"
total=$(wc -l < "$OUT/needs.txt" | tr -d ' ')
echo "$total needs sent at $(date -u +%T); waiting up to ${MAX}s"

state_of() { # state_of <handle>: landed | stuck | open
  local merged
  merged=$(hub_vivi "sqlite3 $P/.vivi/pump/pump.db \"select count(*) from flow_steps where need='$1' and node like 'merge%' and state='done' and verdict='merged'\"" 2>/dev/null || echo 0)
  if [ "${merged:-0}" -gt 0 ]; then echo landed; return; fi
  if hub_vivi "grep -E 'flow-need-(locked|parked)' $P/.vivi/pump/exceptions.txt | grep -q 'need $1 '" 2>/dev/null; then echo stuck; return; fi
  echo open
}

start=$(date +%s)
while :; do
  sleep 5
  landed=0 stuck=0
  while read -r id H; do
    case $(state_of "$H") in landed) landed=$((landed + 1)) ;; stuck) stuck=$((stuck + 1)) ;; esac
  done < "$OUT/needs.txt"
  [ $((landed + stuck)) -ge "$total" ] && break
  [ $(( $(date +%s) - start )) -ge "$MAX" ] && break
done
elapsed=$(( $(date +%s) - start ))
while read -r id H; do echo "$id $H $(state_of "$H")"; done < "$OUT/needs.txt" > "$OUT/needs.state" && mv "$OUT/needs.state" "$OUT/needs.txt"
echo "result: $landed landed, $stuck stuck of $total after ${elapsed}s"
echo "$landed $stuck $total $elapsed" > "$OUT/result.txt"

# Artifacts: pump home, the lane, every step row with its need, the agent logs.
docker exec vivi-lab-hub tar -C "$P/.vivi" -cf - pump mailspace.toml blobs 2>/dev/null | tar -C "$OUT" -xf - || true
docker exec vivi-lab-hub cat /home/vivi/service.log > "$OUT/service.log" 2>/dev/null || true
hub_vivi "git -C /srv/vivi/lanes/lab/app.git log --oneline --graph --all -200" > "$OUT/lane.txt" 2>&1 || true
hub_vivi "sqlite3 -header $P/.vivi/pump/pump.db 'select need, node, state, verdict from flow_steps order by rowid'" > "$OUT/steps.txt" 2>&1 || true
for w in $(docker ps --format '{{.Names}}' --filter name=vivi-lab-worker-); do
  mkdir -p "$OUT/agents/$w"
  docker exec "$w" tar -C /home/worker -cf - sim-agent 2>/dev/null | tar -C "$OUT/agents/$w" -xf - || true
done
echo "artifacts: $OUT"
if python3 -c "import tomllib,sys;sys.exit(0 if 'expect' in tomllib.load(open(sys.argv[1],'rb')) else 1)" "$LAB/scenarios/$SCENARIO/plan.toml"; then
  python3 "$LAB/expect.py" "$LAB/scenarios/$SCENARIO/plan.toml" "$OUT" | tee "$OUT/expect.txt"
fi
