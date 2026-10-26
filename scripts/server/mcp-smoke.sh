#!/bin/bash
# MCP + API-key smoke against a running server. Creates one throwaway project.
#
#   scripts/mcp-smoke.sh
#   SERVER_URL=https://vivi-server-main-production.up.railway.app scripts/mcp-smoke.sh
set -u
SRV=${SRV:-$(cd "$(dirname "$0")/../.." && pwd)}
PORT=${PORT:-18125}
BASE=${SERVER_URL:-}
BASE=${BASE%/}
LOCAL=0
if [ -z "$BASE" ]; then LOCAL=1; BASE="http://127.0.0.1:$PORT"; fi
W=/tmp/mcp-smoke
PASS=0; FAIL=0
ok() { echo "PASS: $1"; PASS=$((PASS+1)); }
bad() { echo "FAIL: $1"; FAIL=$((FAIL+1)); }
jget() { python3 -c "import json,sys; print(json.load(sys.stdin)$1)"; }

rm -rf "$W"; mkdir -p "$W/data"
if [ "$LOCAL" = 1 ]; then
  (cd "$SRV" && cargo build --bin vivi 2>&1 | tail -1)
  [ -x "$SRV/target/debug/vivi" ] || { echo "vivi-server binary not found under $SRV"; exit 1; }
  SRVKEY=$(openssl rand -base64 32)
  PORT=$PORT DATA_DIR="$W/data" STORE_ENCRYPTION_KEY="$SRVKEY" "$SRV/target/debug/vivi" server run >"$W/server.log" 2>&1 &
  SPID=$!
  trap 'kill $SPID 2>/dev/null' EXIT
fi
UP=0
for _ in $(seq 1 40); do
  curl -s -m 2 "$BASE/healthz" 2>/dev/null | grep -q '"ok"' && { UP=1; break; }
  sleep 0.5
done
[ "$UP" = 1 ] && ok "server up" || { bad "server up"; echo "RESULT pass=$PASS fail=$FAIL"; exit 1; }

openssl genpkey -algorithm ed25519 -out "$W/key.pem" 2>/dev/null
openssl pkey -in "$W/key.pem" -pubout -out "$W/pub.pem" 2>/dev/null
CREATE=$(python3 -c 'import json,sys; print(json.dumps({"public_key_pem": open(sys.argv[1]).read(), "creator_name": "creator"}))' "$W/pub.pem")
PROJ=$(curl -sS -X POST "$BASE/v1/projects" -H "content-type: application/json" -d "$CREATE")
PID=$(echo "$PROJ" | jget '["project_id"]')
[ -n "$PID" ] && ok "create project" || { bad "create project: $PROJ"; echo "RESULT pass=$PASS fail=$FAIL"; exit 1; }

CHAL_REQ=$(python3 -c 'import json,sys; print(json.dumps({"public_key_pem": open(sys.argv[1]).read()}))' "$W/pub.pem")
CHAL=$(curl -sS -X POST "$BASE/v1/projects/$PID/identities/challenge" -H "content-type: application/json" -d "$CHAL_REQ")
CID=$(echo "$CHAL" | jget '["challenge_id"]')
echo "$CHAL" | jget '["challenge"]' | python3 -c 'import sys,base64; open(sys.argv[1],"wb").write(base64.b64decode(sys.stdin.read().strip()))' "$W/chal.bin"
openssl pkeyutl -sign -inkey "$W/key.pem" -rawin -in "$W/chal.bin" -out "$W/sig.bin" 2>/dev/null
SIG=$(python3 -c 'import base64,sys; print(base64.b64encode(open(sys.argv[1],"rb").read()).decode())' "$W/sig.bin")
MSID=$(python3 -c 'import hashlib,sys; h=hashlib.sha256(b"creator").hexdigest(); print(f"{h[0:8]}-{h[8:12]}-{h[12:16]}-{h[16:20]}-{h[20:32]}")')
REG=$(python3 -c 'import json,sys; print(json.dumps({"name":"creator","public_key_pem":open(sys.argv[1]).read(),"challenge_id":sys.argv[2],"signature_base64":sys.argv[3],"mailspace_id":sys.argv[4]}))' "$W/pub.pem" "$CID" "$SIG" "$MSID")
IDENT=$(curl -sS -X POST "$BASE/v1/projects/$PID/identities" -H "content-type: application/json" -d "$REG")
TOKEN=$(echo "$IDENT" | jget '["token"]')
[ -n "$TOKEN" ] && ok "register" || { bad "register: $IDENT"; echo "RESULT pass=$PASS fail=$FAIL"; exit 1; }

KEYS=$(curl -sS -X POST "$BASE/v1/projects/$PID/keys" -H "content-type: application/json" -H "authorization: Bearer $TOKEN" -d '{"label":"mcp-smoke"}')
APIKEY=$(echo "$KEYS" | jget '["key"]')
[ -n "$APIKEY" ] && ok "mint api key" || { bad "mint api key: $KEYS"; echo "RESULT pass=$PASS fail=$FAIL"; exit 1; }

rpc() {
  curl -sS -X POST "$BASE/v1/mcp" \
    -H "content-type: application/json" \
    -H "authorization: Bearer $APIKEY" \
    -d "$1"
}

INIT=$(rpc '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"mcp-smoke","version":"0"}}}')
echo "$INIT" | grep -q '"vivi-server"' && ok "initialize" || bad "initialize: $INIT"

LIST=$(rpc '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}')
echo "$LIST" | grep -q '"overview"' && ok "tools/list" || bad "tools/list: $LIST"

rpc_text() {
  echo "$1" | python3 -c 'import json,sys; r=json.load(sys.stdin); print(r.get("result",{}).get("content",[{}])[0].get("text",""))'
}

OVER=$(rpc "$(python3 -c 'import json,sys; print(json.dumps({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"overview","arguments":{"project":sys.argv[1]}}}))' "$PID")")
echo "$(rpc_text "$OVER")" | grep -q '"creator"' && ok "overview via api key" || bad "overview: $OVER"

PREFIX=$(python3 -c 'import hashlib; print(hashlib.sha256(b"creator").hexdigest()[:8])')
SEND=$(rpc "$(python3 -c 'import json,sys; print(json.dumps({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"send","arguments":{"project":sys.argv[2],"kind":"need","to":"creator@%s.local"%sys.argv[1],"subject":"mcp-smoke","body":"from the smoke","done_when":"from the smoke"}}}))' "$PREFIX" "$PID")")
echo "$(rpc_text "$SEND")" | grep -q '"handle"' && ok "send need" || bad "send: $SEND"

# E2E: the real vivi client against this server. The client posts
# {server}/v1/mcp, takes the key from VIVI_MCP_API_KEY, and sends
# --project-id as the tools' `project` argument.
VIVI=${VIVI:-}
if [ -z "$VIVI" ]; then
  echo "SKIP: VIVI is not set; the vivi-client leg did not run"
elif [ ! -x "$VIVI" ]; then
  echo "SKIP: VIVI=$VIVI is not executable; the vivi-client leg did not run"
else
  if VIVI_MCP_SERVER="$BASE" VIVI_MCP_API_KEY="$APIKEY" "$VIVI" mcp --project-id "$PID" tools >"$W/vivi-tools.txt" 2>&1 \
    && grep -q 'overview' "$W/vivi-tools.txt"; then
    ok "vivi mcp tools"
  else
    bad "vivi mcp tools: $(cat "$W/vivi-tools.txt")"
  fi
  if VIVI_MCP_SERVER="$BASE" VIVI_MCP_API_KEY="$APIKEY" "$VIVI" mcp --project-id "$PID" call overview >"$W/vivi-overview.txt" 2>&1 \
    && grep -q "$PID" "$W/vivi-overview.txt"; then
    ok "vivi mcp call overview names the project"
  else
    bad "vivi mcp call overview: $(cat "$W/vivi-overview.txt")"
  fi
fi

echo "RESULT pass=$PASS fail=$FAIL project=$PID"
[ "$FAIL" = 0 ] || exit 1
