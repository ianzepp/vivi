#!/bin/bash
# Hosted-project smoke: the hub's project mailspace and the server wrapping it
# are the same mailspace.
#
# One host mailspace made with the vivi CLI (roles operator and mind,
# `[sync] send = "addressed"`, a `[sync] project` id), registered in a host
# registry with `vivi pump service add --no-pump --serve`, and one server that
# reads the same registry. Nothing is copied between them. The legs:
#   - the server serves the registered project, and a project API key minted
#     by the host's own `vivi sync api-key mint` (no identity) and one minted
#     over HTTP with the creator's identity token (with `identity: mind`) both
#     authenticate on /v1/projects/<id>/mcp;
#   - a need sent over MCP (default identity: operator) is in the host
#     mailspace at once, as `vivi need list --for mind` shows it;
#   - a mark set over MCP as mind is the mark the CLI shows;
#   - a need created by the CLI is listed, shown and on the board over MCP,
#     live, before any index pass;
#   - done and reopen over MCP are the CLI's statuses;
#   - the server never lays out a `.vivi` under DATA_DIR/projects/<id>;
#   - removing the registry entry stops the server serving the project.
#
# Local only: boots target/debug/vivi-server from this repo on $PORT.
#
# Environment, all optional:
#   SRV         this repo (default: the script's parent directory)
#   VIVI        the vivi binary (default: vivi on PATH, else ~/.local/bin/vivi)
#   PORT        server port (default: a free port)
#   KEEP        set to 1 to keep the temp directory
set -u
[ -d /opt/homebrew/bin ] && export PATH=/opt/homebrew/bin:$PATH
SRV=${SRV:-$(cd "$(dirname "$0")/.." && pwd)}
VIVI=${VIVI:-$(command -v vivi || echo "$HOME/.local/bin/vivi")}
PORT=${PORT:-$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')}
BASE=http://127.0.0.1:$PORT
W=$(mktemp -d "${TMPDIR:-/tmp}/hosted-smoke.XXXXXX")
HOST=$W/host
DATA=$W/data
PASS=0; FAIL=0
SPID=""
ok() { echo "PASS: $1"; PASS=$((PASS+1)); }
bad() { echo "FAIL: $1"; FAIL=$((FAIL+1)); }
finish() {
  [ -n "$SPID" ] && kill "$SPID" 2>/dev/null
  [ "${KEEP:-0}" = 1 ] || rm -rf "$W"
}
trap finish EXIT
done_() { echo "RESULT pass=$PASS fail=$FAIL"; [ "$FAIL" = 0 ]; exit $?; }
die() { bad "$1"; done_; }

[ -x "$VIVI" ] || { echo "vivi binary not found at $VIVI (set VIVI)"; exit 1; }
(cd "$SRV" && cargo build --bin vivi-server 2>&1 | tail -1)
[ -x "$SRV/target/debug/vivi-server" ] || { echo "vivi-server binary not found under $SRV"; exit 1; }
mkdir -p "$DATA" "$W/cfg"
export VIVI_CONFIG_DIR=$W/cfg
jget() { python3 -c "import json,sys; print(json.load(sys.stdin)$1)"; }

# --- the host mailspace, made with the CLI -----------------------------------
# A canonical project id: 25 Crockford digits in groups of five.
PID=$(python3 -c '
import random
a = "0123456789abcdefghjkmnpqrstvwxyz"
s = "".join(random.SystemRandom().choice(a) for _ in range(25))
print("-".join(s[i:i + 5] for i in range(0, 25, 5)))')
"$VIVI" mailspace init --project "$HOST" >/dev/null 2>&1
"$VIVI" role add mind --kind mind --project "$HOST" >/dev/null 2>&1
printf '\n[sync]\nserver = "%s"\nproject = "%s"\nname = "hub"\nsend = "addressed"\n' "$BASE" "$PID" >> "$HOST/.vivi/mailspace.toml"
"$VIVI" role list --project "$HOST" 2>/dev/null | grep -q '^mind ' \
  && grep -q '^send = "addressed"' "$HOST/.vivi/mailspace.toml" \
  && ok "host mailspace made with the CLI (operator, mind; project $PID)" || die "host mailspace setup"

"$VIVI" pump service add "$HOST" --no-pump --serve >"$W/registry.log" 2>&1 \
  && "$VIVI" pump service list 2>&1 | grep -q "serve" \
  && ok "host registered with --no-pump --serve" || { cat "$W/registry.log"; die "registry add"; }

# --- the server, on the same registry ----------------------------------------
BIND=127.0.0.1 PORT=$PORT DATA_DIR=$DATA STORE_ENCRYPTION_KEY=$(openssl rand -base64 32) \
  "$SRV/target/debug/vivi-server" >"$W/server.log" 2>&1 &
SPID=$!
UP=0
for _ in $(seq 1 40); do
  curl -s -m 2 "$BASE/healthz" 2>/dev/null | grep -q '"ok"' && { UP=1; break; }
  sleep 0.5
done
[ "$UP" = 1 ] && ok "server up on $BASE" || { cat "$W/server.log"; die "server up"; }

# --- API keys: one from the host's CLI, one over HTTP with an identity -------
# `vivi sync api-key mint` authenticates as the host mailspace's own sync key,
# which the server registered as the project's creator member; it needs the
# project to be served, so it also waits for that.
KEYFILE=$W/key-operator
MINTED=0
for _ in $(seq 1 30); do
  rm -f "$KEYFILE"
  "$VIVI" sync api-key mint --label smoke --out "$KEYFILE" --project "$HOST" >"$W/mint.log" 2>&1 \
    && [ -s "$KEYFILE" ] && { MINTED=1; break; }
  sleep 1
done
[ "$MINTED" = 1 ] && ok "the server serves the registered project; CLI minted a key (no identity)" \
  || { cat "$W/mint.log" "$W/server.log"; die "CLI key mint"; }
KEY_OP=$(tr -d '\n' < "$KEYFILE")

# The identity key: the creator signs a challenge with the mailspace's key
# (the 32-byte seed -> PKCS#8 -> PEM), takes an identity token, mints with
# `identity: mind`.
{ printf '\x30\x2e\x02\x01\x00\x30\x05\x06\x03\x2b\x65\x70\x04\x22\x04\x20'; cat "$HOST/.vivi/chat/identity.ed25519"; } > "$W/h.der"
openssl pkey -inform DER -in "$W/h.der" -out "$W/h.priv.pem" 2>/dev/null
openssl pkey -in "$W/h.priv.pem" -pubout -out "$W/h.pub.pem" 2>/dev/null
CH=$(python3 -c 'import json,sys; print(json.dumps({"public_key_pem": open(sys.argv[1]).read()}))' "$W/h.pub.pem" \
  | curl -s -m 15 -X POST "$BASE/v1/projects/$PID/identities/challenge" -H 'content-type: application/json' -d @-)
CID=$(echo "$CH" | jget '["challenge_id"]' 2>/dev/null)
echo "$CH" | jget '["challenge"]' 2>/dev/null | python3 -c 'import base64,sys; sys.stdout.buffer.write(base64.b64decode(sys.stdin.read().strip()))' > "$W/chal.bin"
openssl pkeyutl -sign -inkey "$W/h.priv.pem" -rawin -in "$W/chal.bin" -out "$W/sig.bin" 2>/dev/null
SIG=$(base64 < "$W/sig.bin" | tr -d '\n')
TOKEN=$(curl -s -m 15 -X POST "$BASE/v1/projects/$PID/auth/token" -H 'content-type: application/json' \
  -d "{\"name\":\"hub\",\"challenge_id\":\"$CID\",\"signature_base64\":\"$SIG\"}" | jget '["token"]' 2>/dev/null)
MINT=$(curl -s -m 15 -X POST "$BASE/v1/projects/$PID/keys" -H "authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' -d '{"label":"smoke-mind","identity":"mind"}')
KEY_MIND=$(echo "$MINT" | jget '["key"]' 2>/dev/null)
[ -n "$KEY_MIND" ] && [ "$(echo "$MINT" | jget '["identity"]')" = mind ] \
  && ok "creator token minted a key with identity mind (HTTP)" || die "identity key mint: $MINT"

# --- MCP over HTTP -----------------------------------------------------------
# mcp <key> <tool> <json args>: prints the tool's JSON result; status in $MCP_ERR.
MCP_ERR=0
mcp() {
  python3 - "$BASE/v1/projects/$PID/mcp" "$1" "$2" "$3" <<'PY'
import json, sys, urllib.request
url, key, name, args = sys.argv[1:5]
body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                   "params": {"name": name, "arguments": json.loads(args)}}).encode()
req = urllib.request.Request(url, body, {"authorization": "Bearer " + key, "content-type": "application/json"})
try:
    value = json.load(urllib.request.urlopen(req, timeout=20))
except Exception as exc:
    print(json.dumps({"error": "http: %s" % exc})); sys.exit(2)
if "result" not in value:
    print(json.dumps({"error": json.dumps(value)})); sys.exit(2)
text = value["result"]["content"][0]["text"]
print(text)
sys.exit(1 if value["result"].get("isError") else 0)
PY
}
status_of() { # http status of an MCP call with $1 as the key (project overview)
  curl -s -m 15 -o /dev/null -w '%{http_code}' -X POST "$BASE/v1/projects/$PID/mcp" \
    -H "authorization: Bearer $1" -H 'content-type: application/json' \
    -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"overview","arguments":{}}}'
}
[ "$(status_of "$KEY_OP")" = 200 ] && [ "$(status_of "$KEY_MIND")" = 200 ] \
  && ok "both keys authenticate on /v1/projects/<id>/mcp" || bad "key status: $(status_of "$KEY_OP") $(status_of "$KEY_MIND")"

# --- send over MCP -> the CLI sees it at once --------------------------------
SENT=$(mcp "$KEY_OP" send '{"kind":"need","to":"mind","subject":"hosted smoke need","body":"ship it\n\ndone_when: shipped"}')
HANDLE=$(echo "$SENT" | jget '["handle"]' 2>/dev/null)
[ "$(echo "$SENT" | jget '["from"]' 2>/dev/null)" = operator ] && echo "$HANDLE" | grep -qE '^[0-9a-f]{8}$' \
  && ok "MCP send as operator to mind ($HANDLE)" || die "MCP send: $SENT"
"$VIVI" need list --for mind --status open --project "$HOST" 2>&1 | grep -q "hosted smoke need" \
  && ok "vivi need list --for mind shows it immediately" || bad "CLI does not list the MCP-sent need"

# --- mark over MCP -> the CLI sees the mark ----------------------------------
"$VIVI" need list --for mind --mark pump --project "$HOST" 2>&1 | grep -q "hosted smoke need" \
  && die "the need carries the pump mark before it was marked"
mcp "$KEY_MIND" mark "{\"handle\":\"$HANDLE\",\"marks\":[\"pump\"]}" >"$W/mark.out" 2>&1 \
  && ok "MCP mark as mind (for=mind)" || bad "MCP mark: $(cat "$W/mark.out")"
"$VIVI" need list --for mind --mark pump --project "$HOST" 2>&1 | grep -q "hosted smoke need" \
  && ok "vivi need list --for mind --mark pump sees the mark" || bad "CLI does not see the pump mark"
mcp "$KEY_MIND" list '{"kind":"need","marks":["pump"]}' | grep -q "$HANDLE" \
  && ok "MCP list --marks pump agrees" || bad "MCP list by mark"

# --- CLI-created need -> MCP list/show/board ---------------------------------
CLI_OUT=$("$VIVI" need send --from operator --to mind --subject "cli smoke need" --body "from the cli" --project "$HOST" 2>&1)
CLI_HANDLE=$(echo "$CLI_OUT" | awk '/^created/ {c=$3} /^sent/ {s=$2} END {print (c != "" ? c : s)}')
echo "$CLI_HANDLE" | grep -qE '^[0-9a-f]{8}$' && ok "CLI need send ($CLI_HANDLE)" || die "CLI need send: $CLI_OUT"
mcp "$KEY_MIND" list '{"kind":"need"}' | grep -q "$CLI_HANDLE" \
  && ok "MCP list sees the CLI-created need" || bad "MCP list misses the CLI need"
SHOWN=$(mcp "$KEY_MIND" show "{\"handle\":\"$CLI_HANDLE\"}" 2>&1)
echo "$SHOWN" | grep -q "cli smoke need" && ok "MCP show reads the CLI-created need" || bad "MCP show: $SHOWN"
BOARD=$(mcp "$KEY_MIND" board '{}' 2>&1)
[ "$(echo "$BOARD" | jget '["totals"]["needs_open"]' 2>/dev/null)" = 2 ] \
  && ok "MCP board counts both needs open" || bad "MCP board: $BOARD"

# --- done / reopen over MCP -> CLI status ------------------------------------
mcp "$KEY_MIND" done "{\"handle\":\"$HANDLE\",\"note\":\"smoke done\"}" >"$W/done.out" 2>&1 \
  && ok "MCP done as mind" || bad "MCP done: $(cat "$W/done.out")"
"$VIVI" need list --for mind --status done --project "$HOST" 2>&1 | grep -q "hosted smoke need" \
  && ! "$VIVI" need list --for mind --status open --project "$HOST" 2>&1 | grep -q "hosted smoke need" \
  && ok "CLI status: done" || bad "CLI does not show the need done"
mcp "$KEY_MIND" reopen "{\"handle\":\"$HANDLE\",\"note\":\"smoke reopen\"}" >"$W/reopen.out" 2>&1 \
  && ok "MCP reopen as mind" || bad "MCP reopen: $(cat "$W/reopen.out")"
"$VIVI" need list --for mind --status open --project "$HOST" 2>&1 | grep -q "hosted smoke need" \
  && ok "CLI status: open again" || bad "CLI does not show the need reopened"

# --- the server's data dir holds no mailspace --------------------------------
if [ -d "$DATA/projects/$PID" ] && [ -z "$(find "$DATA/projects/$PID" -name .vivi -print -quit)" ]; then
  ok "no .vivi under DATA_DIR/projects/<id> ($(ls "$DATA/projects/$PID" | tr '\n' ' '))"
else
  bad ".vivi found (or no project dir) under $DATA/projects/$PID"
fi

# --- leaving the registry stops the serving ----------------------------------
"$VIVI" pump service remove "$HOST" >/dev/null 2>&1 && ok "registry entry removed" || bad "registry remove"
GONE=0
for _ in $(seq 1 30); do
  [ "$(status_of "$KEY_OP")" != 200 ] && { GONE=1; break; }
  sleep 1
done
[ "$GONE" = 1 ] && ok "the project is no longer served (MCP now $(status_of "$KEY_OP"))" || bad "project still served after removal"
[ -d "$HOST/.vivi" ] && "$VIVI" need list --for mind --project "$HOST" 2>&1 | grep -q "hosted smoke need" \
  && ok "the host mailspace is untouched by the removal" || bad "host mailspace damaged by removal"

done_
