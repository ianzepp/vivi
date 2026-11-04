#!/bin/bash
# setup.sh (inside the hub, as root): build the host base, create the scenario's
# project the way the real hub does (`vivi project create`), configure it, and
# enable its pump. The supervisor is started separately (start-service.sh).
set -euo pipefail
V=/usr/local/bin/vivi
PROJECTS=/srv/vivi/projects
LANES=/srv/vivi/lanes
PROJECT=lab
TARGET=app
P=$PROJECTS/$PROJECT
as_vivi() { (cd / && runuser -u vivi -- env HOME=/home/vivi USER=vivi "$@"); }

# Host base: one pool per worker machine (w1 .. wN, from the plan's [world]),
# each falling back to the next, and the pump roles every project here
# inherits, all routed to w1. Written as vivi.
install -d -o vivi -g vivi -m 750 "$PROJECTS/.vivi"
python3 - /lab/scenario/plan.toml <<'PY' | as_vivi tee "$PROJECTS/.vivi/mailspace.toml" > /dev/null
import sys, tomllib
world = tomllib.load(open(sys.argv[1], "rb")).get("world", {})
machines, slots = int(world.get("machines", 1)), int(world.get("slots", 4))
out = ["# Lab host base: shared pools and roles every project under this root inherits.",
       "base = true", ""]
for i in range(1, machines + 1):
    p = f"w{i}"
    out += [f"[pools.{p}]", 'provider = "worker"', f"slots = {slots}",
            'facts = ["uname -srm"]', "restore = []"]
    if i < machines:
        out.append(f'fallback = "w{i + 1}"')
    out += ["", f"[pools.{p}.worker]", f'host = "worker@worker-{i}"', 'kind = "dir"', "",
            f"[pools.{p}.harness]", 'name = "sim"',
            'run = "sim-agent --model {model} --rules {rules} @{brief}"', 'models = ["lab/sim"]', "",
            f"[pools.{p}.bands]"]
    out += [f'P{b} = [{{ model = "lab/sim", reasoning = "low" }}]' for b in range(4)]
    out += ["", f"[pools.{p}.committer]", 'name = "Lab"', 'email = "lab@example.com"', "",
            f"[pools.{p}.bridge]", "enabled = false", ""]
roles = [("hand", "hand", "P2"), ("mind", "mind", "P1"), ("auditor", "auditor", "P1"),
         ("merge", None, "P2"), ("pump-planner", None, "P1"), ("researcher", None, "P1"),
         ("admitter", None, "P1"), ("slicer", None, "P0"), ("head-cto", None, "P0"),
         ("repairer", None, "P2")]
for name, kind, band in roles:
    out += ["[[identities]]", f'name = "{name}"']
    if kind:
        out.append(f'kind = "{kind}"')
    if name != "mind":
        out.append('pool = "w1"')
    out += [f'model = "{band}"', ""]
print("\n".join(out))
PY
MACHINES=$(python3 -c "import tomllib;print(tomllib.load(open('/lab/scenario/plan.toml','rb')).get('world',{}).get('machines',1))")

# The scenario's repository, as a Git source the project is filled from.
as_vivi bash -c 'rm -rf /tmp/lab-src && cp -r /lab/scenario/repo /tmp/lab-src && cd /tmp/lab-src \
  && git init -q -b main && git add -A \
  && git -c user.name=Lab -c user.email=lab@example.com commit -q -m "lab: initial repository"'

as_vivi $V project create "$PROJECT" --projects-root "$PROJECTS" --lanes-root "$LANES" \
  --git-url-prefix git@hub: --target "$TARGET" --source file:///tmp/lab-src \
  --pool w1 --branch main

as_vivi $V config set --project "$P" pump.tick 2
as_vivi $V config set --project "$P" pump.committer_name Lab
as_vivi $V config set --project "$P" pump.committer_email lab@example.com
pools=$(python3 -c "import sys;print('[' + ', '.join(f'\"w{i}\"' for i in range(1, int(sys.argv[1]) + 1)) + ']')" "$MACHINES")
as_vivi $V config set --project "$P" pump.rungs.pools "$pools"
url=$(as_vivi $V config show --project "$P" --json | python3 -c "import json,sys;print(json.load(sys.stdin)['pools']['w1']['remote_url'])" 2>/dev/null || echo git@hub:lab/app.git)
for i in $(seq 2 "$MACHINES"); do
  as_vivi $V config set --project "$P" "pools.w$i.remote_url" "$url"
done
as_vivi $V config set --project "$P" pump.rungs.timeout 120
budget=$(python3 -c "import tomllib;print(tomllib.load(open('/lab/scenario/plan.toml','rb')).get('world',{}).get('budget',''))")
if [ -n "$budget" ]; then
  as_vivi $V config set --project "$P" pump.dispatch.budget "$budget"
fi
# Each rung runs the repository's check; the plan's [world] may give a rung a
# random fail rate (task_fail_pct, need_fail_pct, main_fail_pct, in percent).
for rung in task need main; do
  pct=$(python3 -c "import tomllib,sys;print(tomllib.load(open('/lab/scenario/plan.toml','rb')).get('world',{}).get(sys.argv[1]+'_fail_pct',0))" "$rung")
  as_vivi $V pump rungs set --project "$P" "$rung" "./check.sh --label $rung --fail-pct $pct"
done
# Stage switches: the plan's [world] stages table (audit, judge, admit,
# need_rung = true|false) becomes [pump.flow.stages].
python3 -c "import tomllib;s=tomllib.load(open('/lab/scenario/plan.toml','rb')).get('world',{}).get('stages',{});print('\n'.join(f'{k} {str(v).lower()}' for k,v in s.items()))" \
  | while read -r stage value; do
      if [ -n "$stage" ]; then
        as_vivi $V config set --project "$P" "pump.flow.stages.$stage" "$value"
      fi
    done
as_vivi $V config validate --project "$P"
as_vivi $V pump start --project "$P"
echo "hub setup: project $P ready"
