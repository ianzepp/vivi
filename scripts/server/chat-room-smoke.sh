#!/bin/bash
# Lightweight chat room smoke (item 17). Creates one throwaway chat project.
#
#   scripts/chat-room-smoke.sh
#   SERVER_URL=https://vivi-server-main-production.up.railway.app scripts/chat-room-smoke.sh
set -u
SRV=${SRV:-$(cd "$(dirname "$0")/../.." && pwd)}
PORT=${PORT:-18126}
BASE=${SERVER_URL:-}
BASE=${BASE%/}
LOCAL=0
if [ -z "$BASE" ]; then LOCAL=1; BASE="http://127.0.0.1:$PORT"; fi
W=/tmp/chat-room-smoke
PASS=0; FAIL=0
ok() { echo "PASS: $1"; PASS=$((PASS+1)); }
bad() { echo "FAIL: $1"; FAIL=$((FAIL+1)); }
jget() { python3 -c "import json,sys; print(json.load(sys.stdin)$1)"; }

rm -rf "$W"; mkdir -p "$W/data"
if [ "$LOCAL" = 1 ]; then
  (cd "$SRV" && cargo build --bin vivi 2>&1 | tail -1)
  [ -x "$SRV/target/debug/vivi" ] || { echo "vivi-server binary not found under $SRV"; exit 1; }
  PORT=$PORT DATA_DIR="$W/data" "$SRV/target/debug/vivi" server run >"$W/server.log" 2>&1 &
  SPID=$!
  trap 'kill $SPID 2>/dev/null' EXIT
fi
UP=0
for _ in $(seq 1 40); do
  curl -s -m 2 "$BASE/healthz" 2>/dev/null | grep -q '"ok"' && { UP=1; break; }
  sleep 0.5
done
[ "$UP" = 1 ] && ok "server up" || { bad "server up"; echo "RESULT pass=$PASS fail=$FAIL"; exit 1; }

CREATE=$(curl -sS -X POST "$BASE/v1/rooms" -H "content-type: application/json" -d '{"name":"alice"}')
RID=$(echo "$CREATE" | jget '["room_id"]')
ALICE=$(echo "$CREATE" | jget '["token"]')
[ -n "$RID" ] && [ -n "$ALICE" ] && ok "create room" || { bad "create room: $CREATE"; echo "RESULT pass=$PASS fail=$FAIL"; exit 1; }

JOIN=$(curl -sS -X POST "$BASE/v1/rooms/$RID/join" -H "content-type: application/json" -d '{"name":"bob"}')
BOB=$(echo "$JOIN" | jget '["token"]')
[ -n "$BOB" ] && ok "join" || { bad "join: $JOIN"; echo "RESULT pass=$PASS fail=$FAIL"; exit 1; }

SENT=$(curl -sS -X POST "$BASE/v1/rooms/$RID/messages" \
  -H "content-type: application/json" -H "authorization: Bearer $ALICE" \
  -d '{"body":"hello from the smoke"}')
echo "$SENT" | grep -q '"handle"' && ok "send" || bad "send: $SENT"

PAGE=$(curl -sS "$BASE/v1/rooms/$RID/messages?since=0" -H "authorization: Bearer $BOB")
echo "$PAGE" | grep -q 'hello from the smoke' && ok "poll" || bad "poll: $PAGE"

MCP=$(curl -sS -X POST "$BASE/v1/rooms/$RID/mcp" \
  -H "content-type: application/json" -H "authorization: Bearer $BOB" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"send","arguments":{"kind":"chat","subject":"mcp","body":"from mcp"}}}')
echo "$MCP" | grep -q 'handle' && ok "mcp send chat" || bad "mcp: $MCP"

echo "RESULT pass=$PASS fail=$FAIL room=$RID"
[ "$FAIL" = 0 ] || exit 1
