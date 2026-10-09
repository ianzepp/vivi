#!/bin/bash
# Two-checkout smoke: two vivi checkouts against a live server — create, join,
# send, board badge, watch, read, offline queue/flush — plus a phase-02 kinded
# send probe. This is the end-to-end acceptance gate (see README).
#
# Local (default): boots target/debug/vivi-server from this repo on $PORT.
# Deployed: SERVER_URL=https://host scripts/two-checkout-smoke.sh runs the same
# legs against that server. Nothing is booted or restarted, so the offline
# queue/flush leg (which has to stop the server) is skipped and reported.
#
# Environment, all optional:
#   SERVER_URL  run against a deployed server instead of booting one
#   SRV         this repo (default: the script's parent directory)
#   VIVI_REPO   the vivi checkout (default: $SRV/../vivi)
#   VIVI        the vivi binary (default: $VIVI_REPO/target/debug/vivi)
#   PORT        local server port (default: 18123)
#
# Each run against a deployed server leaves one throwaway room behind.
set -uo pipefail
[ -d /opt/homebrew/bin ] && export PATH=/opt/homebrew/bin:$PATH
SRV=${SRV:-$(cd "$(dirname "$0")/.." && pwd)}
VIVI_REPO=${VIVI_REPO:-$SRV/../vivi}
VIVI=${VIVI:-$VIVI_REPO/target/debug/vivi}
PORT=${PORT:-18123}
BASE=${SERVER_URL:-}
BASE=${BASE%/}
LOCAL=0
if [ -z "$BASE" ]; then LOCAL=1; BASE="http://127.0.0.1:$PORT"; fi
DATA=/tmp/vc8-data
A=/tmp/vc8-a
B=/tmp/vc8-b
PASS=0; FAIL=0; SKIP=0
# per-run throwaway key (VC-5: server validates base64 and refuses to boot without it)
SRVKEY=$(openssl rand -base64 32)
ok() { echo "PASS: $1"; PASS=$((PASS+1)); }
bad() { echo "FAIL: $1"; FAIL=$((FAIL+1)); }
skip() { echo "SKIP: $1"; SKIP=$((SKIP+1)); }
# server drains up to 10s on SIGTERM; wait for the port to actually free
port_free() {
  for _ in $(seq 1 24); do
    curl -s -m 1 "http://127.0.0.1:$PORT/healthz" >/dev/null 2>&1 || return 0
    sleep 0.5
  done
  return 1
}
clear_port() {
  lsof -ti tcp:$PORT 2>/dev/null | xargs kill 2>/dev/null
  port_free || { echo "FAIL: port $PORT still busy"; exit 1; }
}
boot_server() {
  PORT=$PORT DATA_DIR="$DATA" STORE_ENCRYPTION_KEY="$SRVKEY" "$SRV/target/debug/vivi-server" >>/tmp/vc8-server.log 2>&1 &
  echo $! > /tmp/vc8-server.pid
  sleep 1
}

if [ -d "$VIVI_REPO" ]; then
  (cd "$VIVI_REPO" && cargo build --bin vivi 2>&1 | tail -1)
fi
[ -x "$VIVI" ] || { echo "vivi binary not found at $VIVI (set VIVI or VIVI_REPO)"; exit 1; }
if [ "$LOCAL" = 1 ]; then
  (cd "$SRV" && cargo build --bin vivi-server 2>&1 | tail -1)
  [ -x "$SRV/target/debug/vivi-server" ] || { echo "vivi-server binary not found under $SRV"; exit 1; }
fi

rm -rf "$DATA" "$A" "$B" 2>/dev/null
mkdir -p "$DATA" "$A" "$B"

# boot server (local mode only)
if [ "$LOCAL" = 1 ]; then
  clear_port
  : >/tmp/vc8-server.log
  boot_server
fi
curl -s -m 15 "$BASE/healthz" | grep -q '"ok"' && ok "server up" || bad "server up"

# creator key + room (server wants the SPKI PUBLIC KEY pem, not PKCS#8 private)
openssl genpkey -algorithm ed25519 -out /tmp/vc8-creator.pem 2>/dev/null
openssl pkey -in /tmp/vc8-creator.pem -pubout -out /tmp/vc8-creator-pub.pem 2>/dev/null
PEM=$(python3 - <<'PY'
import json
pem = open('/tmp/vc8-creator-pub.pem').read()
print(json.dumps({"seats": 4, "label": "smoke-creator", "public_key_pem": pem}))
PY
)
CREATE=$(curl -s -m 15 -X POST "$BASE/v1/rooms" -H 'content-type: application/json' -d "$PEM")
ROOM=$(echo "$CREATE" | python3 -c 'import json,sys; print(json.load(sys.stdin)["room_id"])')
echo "room: $ROOM"
[ ${#ROOM} -ge 25 ] && ok "room created" || bad "room created: $CREATE"

# two checkouts
for D in "$A" "$B"; do
  "$VIVI" mailspace init --project "$D" >/dev/null 2>&1
  printf '\n[chat]\nserver = "%s"\n' "$BASE" >> "$D/.vivi/mailspace.toml"
done

"$VIVI" chat join "$ROOM" --label "alpha.checkout-a" --project "$A" >/tmp/vc8-join-a.log 2>&1 && ok "A joined" || { bad "A joined"; cat /tmp/vc8-join-a.log; }
"$VIVI" chat join "$ROOM" --label "beta.checkout-b" --project "$B" >/tmp/vc8-join-b.log 2>&1 && ok "B joined" || { bad "B joined"; cat /tmp/vc8-join-b.log; }

# send + read round-trip
"$VIVI" chat send "$ROOM" --body "hello from A" --project "$A" >/tmp/vc8-send1.log 2>&1
grep -q 'sent' /tmp/vc8-send1.log && ! grep -q 'queued' /tmp/vc8-send1.log && ok "A send (sent)" || { bad "A send"; cat /tmp/vc8-send1.log; }
# unread badge (VC-10) — must be checked before B's --mark-read read clears it
"$VIVI" board --project "$B" >/tmp/vc8-board1.log 2>&1
grep -q '^chat: .* unread in' /tmp/vc8-board1.log && ok "B board shows chat unread badge" || { bad "B board chat badge"; cat /tmp/vc8-board1.log; }
# fail-fast single-cycle watch renders the pending message
"$VIVI" chat watch "$ROOM" --once --project "$B" >/tmp/vc8-watch1.log 2>&1
grep -q 'hello from A' /tmp/vc8-watch1.log && ok "B watch --once renders pending message" || { bad "B watch --once"; cat /tmp/vc8-watch1.log; }
# watch consumes (cursors advance every cycle); send a fresh message for the read assertions
"$VIVI" chat send "$ROOM" --body "hello again from A" --project "$A" >/tmp/vc8-send1b.log 2>&1
"$VIVI" chat read "$ROOM" --mark-read --project "$B" >/tmp/vc8-read1.log 2>&1
grep -q 'hello again from A' /tmp/vc8-read1.log && ok "B read sees message" || { bad "B read"; cat /tmp/vc8-read1.log; }
grep -q 'alpha.checkout-a' /tmp/vc8-read1.log && ok "origin label rendered" || bad "origin label rendered"
[ "$(grep -c 'hello again from A' /tmp/vc8-read1.log)" -eq 1 ] && ok "message read exactly once" || bad "message read more than once"

# offline queue -> restart -> flush (needs to stop the server: local mode only)
if [ "$LOCAL" = 1 ]; then
  kill "$(cat /tmp/vc8-server.pid)" 2>/dev/null; clear_port
  "$VIVI" chat send "$ROOM" --body "offline message" --project "$A" >/tmp/vc8-send2.log 2>&1
  grep -q 'queued' /tmp/vc8-send2.log && ok "offline send queued" || { bad "offline send queued"; cat /tmp/vc8-send2.log; }
  boot_server
  "$VIVI" chat flush --project "$A" >/tmp/vc8-flush.log 2>&1
  grep -qE '1 sent' /tmp/vc8-flush.log && ok "flush delivers queued message" || { bad "flush"; cat /tmp/vc8-flush.log; }
  DEPTH=$("$VIVI" chat status --project "$A" 2>/dev/null | grep -o 'outbox depth: [0-9]*' | grep -o '[0-9]*$')
  [ "$DEPTH" = "0" ] && ok "outbox empty after flush" || bad "outbox depth after flush: $DEPTH"
  "$VIVI" chat read "$ROOM" --mark-read --project "$B" >/tmp/vc8-read2.log 2>&1
  grep -q 'offline message' /tmp/vc8-read2.log && ok "B sees flushed message" || { bad "B read2"; cat /tmp/vc8-read2.log; }
  COUNT=$(grep -c 'offline message' /tmp/vc8-read2.log)
  [ "$COUNT" -eq 1 ] && ok "delivered exactly once" || bad "duplicate delivery: $COUNT"
else
  skip "offline queue/flush (needs a server restart; local mode only)"
fi

# Phase 02 wire (R7/R8): a kinded send round-trips with kind, in_reply_to, and
# refs echoed. A pre-phase-02 server silently drops unknown fields, so a missing
# `kind` here means the server under test is not the phase 02 build. Runs last:
# it broadcasts to the joined seats, and nothing reads after it.
read -r TOKEN LABEL FP < <(echo "$CREATE" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["token"], d["label"], d["fingerprint"][:16])')
NEEDID="msg_$(openssl rand -hex 12)"
H=(-H "authorization: Bearer $TOKEN" -H "x-vivi-chat-from: $LABEL" -H "x-vivi-chat-fingerprint: $FP")
curl -s -m 15 -X POST "$BASE/v1/rooms/$ROOM/messages" "${H[@]}" -H "message-id: $NEEDID" -H 'content-type: application/json' \
  -d '{"kind":"need","subject":"smoke need","body":"kinded smoke","in_reply_to":"msg_aaaaaaaaaaaaaaaaaaaaaaaa","refs":["msg_bbbbbbbbbbbbbbbbbbbbbbbb"]}' >/tmp/vc8-kinded-send.json
POLL=$(curl -s -m 15 "$BASE/v1/rooms/$ROOM/messages?since=0" "${H[@]}")
echo "$POLL" | NEEDID="$NEEDID" python3 -c '
import json, os, sys
m = [x for x in json.load(sys.stdin)["messages"] if x["message_id"] == os.environ["NEEDID"]]
ok = m and m[0].get("kind") == "need" \
    and m[0].get("in_reply_to") == "msg_aaaaaaaaaaaaaaaaaaaaaaaa" \
    and m[0].get("refs") == ["msg_bbbbbbbbbbbbbbbbbbbbbbbb"]
sys.exit(0 if ok else 1)' \
  && ok "kinded send echoes kind/in_reply_to/refs" || { bad "kinded send not echoed (server is not the phase 02 build?)"; echo "$POLL"; }
KINDED_BAD=$(curl -s -m 15 -o /dev/null -w '%{http_code}' -X POST "$BASE/v1/rooms/$ROOM/messages" "${H[@]}" \
  -H "message-id: not-a-traveling-id" -H 'content-type: application/json' -d '{"kind":"need","body":"x"}')
[ "$KINDED_BAD" = "400" ] && ok "kinded send without a traveling Message-ID is rejected" || bad "kinded send with a bad Message-ID returned $KINDED_BAD (expected 400)"

if [ "$LOCAL" = 1 ]; then kill "$(cat /tmp/vc8-server.pid)" 2>/dev/null; fi
echo "RESULT pass=$PASS fail=$FAIL skip=$SKIP"
[ "$FAIL" -eq 0 ]
