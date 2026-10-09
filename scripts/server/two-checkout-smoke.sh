#!/bin/bash
# VC-8 integration smoke: two checkouts, live server, send/read + offline queue/flush.
set -uo pipefail
export PATH=/opt/homebrew/bin:$PATH
VIVI=/Users/ianzepp/work/ianzepp/vivi/target/debug/vivi
SRV=/Users/ianzepp/work/ianzepp/vivi-server
PORT=18123
DATA=/tmp/vc8-data
A=/tmp/vc8-a
B=/tmp/vc8-b
PASS=0; FAIL=0
ok() { echo "PASS: $1"; PASS=$((PASS+1)); }
bad() { echo "FAIL: $1"; FAIL=$((FAIL+1)); }

cd /Users/ianzepp/work/ianzepp/vivi
cargo build --bin vivi 2>&1 | tail -1 || { echo "build failed"; exit 1; }

rm -rf "$DATA" "$A" "$B" 2>/dev/null
mkdir -p "$DATA" "$A" "$B"

# boot server
PORT=$PORT DATA_DIR="$DATA" "$SRV/target/debug/vivi-server" >/tmp/vc8-server.log 2>&1 &
echo $! > /tmp/vc8-server.pid
sleep 1
curl -s -m 3 "http://127.0.0.1:$PORT/healthz" | grep -q '"ok"' && ok "server up" || bad "server up"

# creator key + room (server wants the SPKI PUBLIC KEY pem, not PKCS#8 private)
openssl genpkey -algorithm ed25519 -out /tmp/vc8-creator.pem 2>/dev/null
openssl pkey -in /tmp/vc8-creator.pem -pubout -out /tmp/vc8-creator-pub.pem 2>/dev/null
PEM=$(python3 - <<'PY'
import json
pem = open('/tmp/vc8-creator-pub.pem').read()
print(json.dumps({"seats": 4, "label": "smoke-creator", "public_key_pem": pem}))
PY
)
CREATE=$(curl -s -m 5 -X POST "http://127.0.0.1:$PORT/v1/rooms" -H 'content-type: application/json' -d "$PEM")
ROOM=$(echo "$CREATE" | python3 -c 'import json,sys; print(json.load(sys.stdin)["room_id"])')
echo "room: $ROOM"
[ ${#ROOM} -ge 25 ] && ok "room created" || bad "room created: $CREATE"

# two checkouts
for D in "$A" "$B"; do
  "$VIVI" mailspace init --project "$D" >/dev/null 2>&1
  printf '\n[chat]\nserver = "http://127.0.0.1:%s"\n' "$PORT" >> "$D/.vivi/mailspace.toml"
done

"$VIVI" chat join "$ROOM" --label "alpha.checkout-a" --project "$A" >/tmp/vc8-join-a.log 2>&1 && ok "A joined" || { bad "A joined"; cat /tmp/vc8-join-a.log; }
"$VIVI" chat join "$ROOM" --label "beta.checkout-b" --project "$B" >/tmp/vc8-join-b.log 2>&1 && ok "B joined" || { bad "B joined"; cat /tmp/vc8-join-b.log; }

# send + read round-trip
"$VIVI" chat send "$ROOM" --body "hello from A" --project "$A" >/tmp/vc8-send1.log 2>&1
grep -q 'sent' /tmp/vc8-send1.log && ! grep -q 'queued' /tmp/vc8-send1.log && ok "A send (sent)" || { bad "A send"; cat /tmp/vc8-send1.log; }
"$VIVI" chat read "$ROOM" --mark-read --project "$B" >/tmp/vc8-read1.log 2>&1
grep -q 'hello from A' /tmp/vc8-read1.log && ok "B read sees message" || { bad "B read"; cat /tmp/vc8-read1.log; }
grep -q 'alpha.checkout-a' /tmp/vc8-read1.log && ok "origin label rendered" || bad "origin label rendered"

# offline queue -> restart -> flush
kill $(cat /tmp/vc8-server.pid) 2>/dev/null; sleep 1
curl -s -m 1 "http://127.0.0.1:$PORT/healthz" >/dev/null 2>&1 && { echo "FAIL: server still up after kill"; exit 1; }
"$VIVI" chat send "$ROOM" --body "offline message" --project "$A" >/tmp/vc8-send2.log 2>&1
grep -q 'queued' /tmp/vc8-send2.log && ok "offline send queued" || { bad "offline send queued"; cat /tmp/vc8-send2.log; }
PORT=$PORT DATA_DIR="$DATA" "$SRV/target/debug/vivi-server" >>/tmp/vc8-server.log 2>&1 &
echo $! > /tmp/vc8-server.pid
sleep 1
"$VIVI" chat flush --project "$A" >/tmp/vc8-flush.log 2>&1
grep -qE '1 sent' /tmp/vc8-flush.log && ok "flush delivers queued message" || { bad "flush"; cat /tmp/vc8-flush.log; }
DEPTH=$("$VIVI" chat status --project "$A" 2>/dev/null | grep -o 'outbox depth: [0-9]*' | grep -o '[0-9]*$')
[ "$DEPTH" = "0" ] && ok "outbox empty after flush" || bad "outbox depth after flush: $DEPTH"
"$VIVI" chat read "$ROOM" --mark-read --project "$B" >/tmp/vc8-read2.log 2>&1
grep -q 'offline message' /tmp/vc8-read2.log && ok "B sees flushed message" || { bad "B read2"; cat /tmp/vc8-read2.log; }
COUNT=$(grep -c 'offline message' /tmp/vc8-read2.log)
[ "$COUNT" -eq 1 ] && ok "delivered exactly once" || bad "duplicate delivery: $COUNT"

kill $(cat /tmp/vc8-server.pid) 2>/dev/null
echo "RESULT pass=$PASS fail=$FAIL"
