#!/bin/bash
# up.sh <scenario>: start the lab's machines for one scenario.
# Makes the SSH keys once (lab/.keys), builds the machine image, and starts
# `hub` and `worker-1` .. `worker-N` (the plan's [world] machines) on a private
# Docker network with the vivi binary, the stand-in agent and the scenario
# mounted read-only. Tear down with down.sh.
set -euo pipefail
. "$(dirname "$0")/env.sh"
SCENARIO=${1:?usage: up.sh <scenario dir under lab/scenarios>}
SC="$LAB/scenarios/$SCENARIO"
[ -f "$SC/plan.toml" ] || { echo "up.sh: no $SC/plan.toml" >&2; exit 2; }
[ -x "$LAB/bin/vivi" ] || { echo "up.sh: build lab/bin/vivi first (build-vivi.sh)" >&2; exit 2; }

world() { python3 -c "import tomllib,sys;w=tomllib.load(open(sys.argv[1],'rb')).get('world',{});print(w.get(sys.argv[2],sys.argv[3]))" "$SC/plan.toml" "$1" "$2"; }
MACHINES=$(world machines 1)

K="$LAB/.keys"
if [ ! -f "$K/worker_host" ]; then
  mkdir -p "$K" && chmod 700 "$K"
  for k in dispatch worker hub_host worker_host; do
    ssh-keygen -q -t ed25519 -N '' -C "vivi-lab-$k" -f "$K/$k"
  done
fi
{ echo "hub $(cut -d' ' -f1-2 "$K/hub_host.pub")"
  for i in $(seq 1 "$MACHINES"); do echo "worker-$i $(cut -d' ' -f1-2 "$K/worker_host.pub")"; done
} > "$K/known_hosts"

docker build -q -t vivi-lab-machine "$LAB/image" > /dev/null
docker network inspect vivi-lab > /dev/null 2>&1 || docker network create vivi-lab > /dev/null
start() { # start <name> <role> [extra docker args...]
  local name=$1 role=$2; shift 2
  docker rm -f "vivi-lab-$name" > /dev/null 2>&1 || true
  docker run -d --name "vivi-lab-$name" --hostname "$name" \
    --network vivi-lab --network-alias "$name" -e ROLE="$role" \
    -v "$K":/lab/keys:ro -v "$LAB/bin/vivi":/usr/local/bin/vivi:ro \
    -v "$SC":/lab/scenario:ro "$@" vivi-lab-machine > /dev/null
}
docker ps -aq --filter name=vivi-lab-worker- | xargs -r docker rm -f > /dev/null
start hub hub -v "$LAB/hub":/lab/hub:ro
for i in $(seq 1 "$MACHINES"); do
  start "worker-$i" worker -v "$LAB/agent/sim-agent":/usr/local/bin/sim-agent:ro
done
echo "lab up: hub, $MACHINES worker(s) (scenario $SCENARIO)"
