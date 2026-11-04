# Sourced by the lab scripts. Docker runs with a minimal client config of its
# own (lab/.docker), so a credential helper named in ~/.docker/config.json that
# is not installed does not block pulling public images. The user's contexts
# and CLI plugins (compose) are linked in, not copied.
LAB=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# Homebrew first: the scripts read TOML with Python 3.11+ (system python3 is 3.9).
export PATH="/opt/homebrew/bin:$PATH"
export DOCKER_CONFIG="$LAB/.docker"
if [ ! -f "$DOCKER_CONFIG/config.json" ]; then
  mkdir -p "$DOCKER_CONFIG"
  context=$(python3 -c "import json,os;print(json.load(open(os.path.expanduser('~/.docker/config.json'))).get('currentContext',''))" 2>/dev/null || true)
  printf '{"currentContext": "%s"}\n' "$context" > "$DOCKER_CONFIG/config.json"
  for d in contexts cli-plugins; do
    [ -e "$HOME/.docker/$d" ] && ln -sfn "$HOME/.docker/$d" "$DOCKER_CONFIG/$d"
  done
fi
