#!/bin/bash
# down.sh: stop and remove the lab's machines and network. Keys and runs stay.
. "$(dirname "$0")/env.sh"
docker ps -aq --filter name=vivi-lab- | xargs -r docker rm -f > /dev/null 2>&1 || true
docker network rm vivi-lab > /dev/null 2>&1 || true
echo "lab down"
