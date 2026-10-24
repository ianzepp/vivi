#!/bin/bash
# Two-checkout smoke: the release acceptance gate (see README). Two vivi
# checkouts (A, B), a third member (C), and one server, in three parts:
#
#   1. Rooms (1.3.0): a private, short-lived chat project reached over plain
#      HTTP. Create by name, keyless join, send, poll by cursor, long-poll,
#      close. There is no room client any more (`vivi chat` left vivi 11), so
#      the legs drive the HTTP API directly; the old board badge, `chat watch`
#      and `chat read` legs became cursor polls and a long-poll (marked below).
#   2. The durable project (1.0.0 and on): the same checkouts sync through
#      `vivi sync` as members of a project. This carries what the seat-bound
#      room used to: a need crossing under one handle and the same bytes,
#      closing and receipts, a task for another seat, an outage that queues and
#      then flushes, and prune and resync. The unreleased item events are
#      here too: a done event from the author is applied on the holder after a
#      pull, is never listed as work over MCP, and is served to every member.
#   3. A raw kinded-letter probe on the project wire.
#
# Local (default): boots target/debug/vivi-server from this repo on $PORT.
# Deployed: SERVER_URL=https://host scripts/two-checkout-smoke.sh runs the same
# legs against that server. Nothing is booted or restarted, so the outage leg
# (which has to stop the server) is skipped and reported.
#
# Environment, all optional:
#   SERVER_URL  run against a deployed server instead of booting one
#   SRV         this repo (default: the script's parent directory)
#   VIVI_REPO   the vivi checkout (default: $SRV/../vivi)
#   VIVI        the vivi binary (default: $VIVI_REPO/target/debug/vivi)
#   PORT        local server port (default: 18123)
#
# Each run against a deployed server leaves one closed room and one project
# behind.
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
C=/tmp/vc8-c
K=/tmp/vc8-keys
PASS=0; FAIL=0; SKIP=0
# per-run throwaway key (VC-5: server validates base64 and refuses to boot without it)
SRVKEY=$(openssl rand -base64 32)
ok() { echo "PASS: $1"; PASS=$((PASS+1)); }
bad() { echo "FAIL: $1"; FAIL=$((FAIL+1)); }
skip() { echo "SKIP: $1"; SKIP=$((SKIP+1)); }
jget() { python3 -c "import json,sys; print(json.load(sys.stdin)$1)"; }
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

rm -rf "$DATA" "$A" "$B" "$C" "$K" 2>/dev/null
mkdir -p "$DATA" "$A" "$B" "$C" "$K"

# boot server (local mode only)
if [ "$LOCAL" = 1 ]; then
  clear_port
  : >/tmp/vc8-server.log
  boot_server
fi
curl -s -m 15 "$BASE/healthz" | grep '"ok"' >/dev/null && ok "server up" || bad "server up"

# ---------------------------------------------------------------------------
# 1. Rooms (1.3.0): create by name, keyless join, send, poll by cursor
# ---------------------------------------------------------------------------
# A room is a private chat project with a TTL; its id is the capability. The
# creator is joined by `name` at create, others by `POST .../join {name}`.
CREATE=$(curl -s -m 15 -X POST "$BASE/v1/rooms" -H 'content-type: application/json' \
  -d '{"name": "smoke-creator", "ttl_seconds": 600}')
ROOM=$(echo "$CREATE" | jget '["room_id"]' 2>/dev/null)
CTOKEN=$(echo "$CREATE" | jget '["token"]' 2>/dev/null)
echo "room: $ROOM"
[ ${#ROOM} -ge 25 ] && [ -n "$CTOKEN" ] && [ "$(echo "$CREATE" | jget '["mode"]' 2>/dev/null)" = chat ] \
  && ok "room created (mode chat, creator joined by name)" || bad "room created: $CREATE"

room_join() { # name -> http code; body in /tmp/vc8-join.out
  python3 -c 'import json,sys; print(json.dumps({"name": sys.argv[1]}))' "$1" \
    | curl -s -m 15 -o /tmp/vc8-join.out -w '%{http_code}' -X POST "$BASE/v1/rooms/$ROOM/join" \
        -H 'content-type: application/json' -d @-
}
room_send() { # token subject body outfile -> http code (body in outfile)
  python3 -c 'import json,sys; print(json.dumps({"subject": sys.argv[1], "body": sys.argv[2]}))' "$2" "$3" \
    | curl -s -m 15 -o "$4" -w '%{http_code}' -X POST "$BASE/v1/rooms/$ROOM/messages" \
        -H 'content-type: application/json' -H "authorization: Bearer $1" -d @-
}
room_poll() { # token query -> page on stdout
  curl -s -m 30 "$BASE/v1/rooms/$ROOM/messages?$2" -H "authorization: Bearer $1"
}

CODE=$(room_join alpha.checkout-a); TOK_A=$(jget '["token"]' < /tmp/vc8-join.out 2>/dev/null)
[ "$CODE" = 201 ] && [ -n "$TOK_A" ] && ok "A joined the room by name" || { bad "A joined the room ($CODE)"; cat /tmp/vc8-join.out; }
CODE=$(room_join beta.checkout-b); TOK_B=$(jget '["token"]' < /tmp/vc8-join.out 2>/dev/null)
[ "$CODE" = 201 ] && [ -n "$TOK_B" ] && [ "$(jget '["members"]' < /tmp/vc8-join.out 2>/dev/null)" = 3 ] \
  && ok "B joined the room by name (3 members)" || { bad "B joined the room ($CODE)"; cat /tmp/vc8-join.out; }
CODE=$(room_join alpha.checkout-a)
[ "$CODE" = 409 ] && grep -q name_taken /tmp/vc8-join.out && ok "a taken name is refused (409 name_taken)" \
  || { bad "duplicate name returned $CODE"; cat /tmp/vc8-join.out; }

# send + read round-trip
CODE=$(room_send "$TOK_A" "greeting" "hello from A" /tmp/vc8-send1.json)
[ "$CODE" = 201 ] && python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
sys.exit(0 if len(d["handle"]) == 8 and d["kind"] == "chat" and d["duplicate"] is False else 1)' /tmp/vc8-send1.json \
  && ok "A send (201, chat letter, handle $(jget '["handle"]' < /tmp/vc8-send1.json))" || { bad "A send ($CODE)"; cat /tmp/vc8-send1.json; }
# Was the board's unread badge (VC-10), checked before `chat read --mark-read`
# cleared it. The cursor is the client's now: B's unread count is whatever lies
# past its cursor, and a poll consumes nothing server-side, so asking twice
# from the same cursor shows the same pending message.
P1=$(room_poll "$TOK_B" "since=0"); P1b=$(room_poll "$TOK_B" "since=0")
echo "$P1" | python3 -c '
import json, sys
m = json.load(sys.stdin)["messages"]
sys.exit(0 if len(m) == 1 and m[0]["body"] == "hello from A" else 1)' && [ "$P1" = "$P1b" ] \
  && ok "B has 1 unread past cursor 0, and polling does not consume it" || { bad "B unread past cursor 0"; echo "$P1"; }
echo "$P1" | python3 -c '
import json, sys
m = json.load(sys.stdin)["messages"]
sys.exit(0 if m and m[0]["from"] == "alpha.checkout-a" else 1)' \
  && ok "origin label rendered (from alpha.checkout-a)" || { bad "origin label rendered"; echo "$P1"; }
# Was `chat watch --once`, fail-fast on one pending message. Now the long poll:
# B parks on its cursor, A sends a second later, and the poll returns that
# message before the wait runs out.
CUR=$(echo "$P1" | jget '["next_cursor"]')
( sleep 2; room_send "$TOK_A" "again" "hello again from A" /tmp/vc8-send2.json >/dev/null ) &
SENDER=$!
T0=$(date +%s)
WATCH=$(room_poll "$TOK_B" "since=$CUR&wait=20")
T1=$(date +%s)
wait "$SENDER"
echo "$WATCH" | grep -q 'hello again from A' && [ $((T1 - T0)) -lt 20 ] \
  && ok "B long poll returns the pending message ($((T1 - T0))s into a 20s wait)" || { bad "B long poll"; echo "$WATCH"; }
# Was `chat read --mark-read`: a message is read once when the cursor advances.
P2=$(room_poll "$TOK_B" "since=$CUR")
echo "$P2" | grep -q 'hello again from A' && ok "B read sees message" || { bad "B read"; echo "$P2"; }
[ "$(echo "$P2" | grep -o 'hello again from A' | wc -l)" -eq 1 ] && ok "message read exactly once" || bad "message read more than once"
CUR2=$(echo "$P2" | jget '["next_cursor"]')
room_poll "$TOK_B" "since=$CUR2" | python3 -c '
import json, sys
sys.exit(0 if json.load(sys.stdin)["messages"] == [] else 1)' \
  && ok "advancing the cursor leaves nothing unread" || bad "messages left past the advanced cursor"
# A closed room takes no more writes (1.3.0: close, then 410).
CODE=$(curl -s -m 15 -o /tmp/vc8-close.out -w '%{http_code}' -X POST "$BASE/v1/rooms/$ROOM/close" -H "authorization: Bearer $CTOKEN")
CODE2=$(room_send "$TOK_A" "late" "after close" /tmp/vc8-late.json)
[ "$CODE" = 200 ] && [ "$(jget '["closed"]' < /tmp/vc8-close.out 2>/dev/null)" = True ] && [ "$CODE2" = 410 ] \
  && ok "close ends the room; a later send is refused (410)" || bad "close: $CODE, then send: $CODE2"

# ---------------------------------------------------------------------------
# 2. The durable project: two checkouts and a third member, syncing by letter
# ---------------------------------------------------------------------------
# What `chat join` and `chat pull` did for a seat is `sync join` and `sync` for a
# member of a project. A creates it with its own key; B and C are vouched for by
# A (an admitted member) and then join. Address domains are the first 8 hex
# characters of each mailspace id.
for D in "$A" "$B" "$C"; do
  "$VIVI" mailspace init --project "$D" >/dev/null 2>&1
  "$VIVI" role add mind --kind mind --project "$D" >/dev/null 2>&1
  "$VIVI" role add hand --kind hand --project "$D" >/dev/null 2>&1
done
id_of() { sed -n 's/^id *= *"\(.*\)"/\1/p' "$1/.vivi/mailspace.toml" | head -1; }
PA=$(id_of "$A"); PA=${PA:0:8}; PB=$(id_of "$B"); PB=${PB:0:8}; PC=$(id_of "$C"); PC=${PC:0:8}
[ -n "$PA" ] && [ "$PA" != "$PB" ] && [ "$PB" != "$PC" ] && [ "$PA" != "$PC" ] \
  && ok "each checkout minted its own mailspace id (prefixes $PA, $PB, $PC)" || { bad "mailspace prefixes: $PA $PB $PC"; exit 1; }

printf '\n[sync]\nserver = "%s"\nname = "alpha"\n' "$BASE" >> "$A/.vivi/mailspace.toml"
"$VIVI" sync create --project "$A" >/tmp/vc8-create.log 2>&1
PID=$(awk '/^project / {print $2}' /tmp/vc8-create.log)
[ -n "$PID" ] && ok "A created the project ($PID)" || { bad "A created the project"; cat /tmp/vc8-create.log; exit 1; }
printf 'project = "%s"\n' "$PID" >> "$A/.vivi/mailspace.toml"
for pair in "$B:beta" "$C:gamma"; do
  printf '\n[sync]\nserver = "%s"\nproject = "%s"\nname = "%s"\n' "$BASE" "$PID" "${pair#*:}" >> "${pair%%:*}/.vivi/mailspace.toml"
done

"$VIVI" sync join --project "$A" >/tmp/vc8-join-a.log 2>&1 && ok "A (creator) joined the project" || { bad "A joined the project"; cat /tmp/vc8-join-a.log; }
for pair in "$B:beta" "$C:gamma"; do
  D=${pair%%:*}; N=${pair#*:}
  "$VIVI" sync key --project "$D" > "$K/$N.pub.pem" 2>/dev/null
  "$VIVI" sync vouch "$N" --key-file "$K/$N.pub.pem" --project "$A" >/tmp/vc8-vouch-$N.log 2>&1 \
    && "$VIVI" sync join --project "$D" >/tmp/vc8-join-$N.log 2>&1 \
    && ok "$N was vouched for by A and joined the project" || { bad "$N joined the project"; cat /tmp/vc8-vouch-$N.log /tmp/vc8-join-$N.log; }
done

# A project API key is a bearer for the letter wire and for MCP: it is how the
# legs below look at what the server itself says (the old script held a room
# token for this).
mint_key() { # dir tag -> raw key on stdout
  "$VIVI" sync api-key mint --label "smoke-$2" --out "$K/$2.key" --project "$1" >/dev/null 2>&1
  cat "$K/$2.key" 2>/dev/null
}
KEY_B=$(mint_key "$B" b); KEY_C=$(mint_key "$C" c)
[ -n "$KEY_B" ] && [ -n "$KEY_C" ] && ok "B and C minted project API keys" || { bad "minted project API keys"; exit 1; }
items_as() { # key query -> body on stdout
  curl -s -m 15 "$BASE/v1/projects/$PID/items?$2" -H "authorization: Bearer $1"
}
mcp_tool() { # key tool args-json -> the tool's text result on stdout
  python3 -c '
import json, sys
args = json.loads(sys.argv[2])
if isinstance(args, dict) and "project" not in args:
    args["project"] = sys.argv[3]
print(json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                  "params": {"name": sys.argv[1], "arguments": args}}))' "$2" "$3" "$PID" \
    | curl -s -m 15 -X POST "$BASE/v1/mcp" -H 'content-type: application/json' \
        -H "authorization: Bearer $1" -d @- \
    | python3 -c 'import json,sys; r = json.load(sys.stdin); print(r.get("result", {}).get("content", [{}])[0].get("text", ""))'
}

# Phase 02 traveling identity (P2-1/2/5/6): a need minted on A crosses to B and
# files there under the same handle and the same bytes. Counts in sync summaries
# are not asserted (earlier legs left traffic); outcomes are, through the
# mailspace itself.
# grep reads to the end (no -q): an early exit would close the pipe on vivi, which
# fails on a broken pipe and, under pipefail, turns a match into a failure.
open_has() { "$VIVI" "$1" list --for "${4:-mind}" --status open --project "$2" 2>&1 | grep "$3" >/dev/null; }
# the Content line of the need's own message: the thread lists every letter that
# threads to it (a done event, a receipt), and by date they can come first
content_of() { "$VIVI" need show "$1" --project "$2" 2>&1 | awk -v h="$1" '$0 ~ "^## .* - " h " \\[" {f=1; next} f && /^Content:/ {print; f=0}'; }
# a letter sent to another mailspace prints `sent <handle>`; one addressed to a
# role of the sender's own prints `created <role> <handle>` (the traveling
# letter) and then the sent copy
handle_of() { awk '/^created/ {c=$3} /^sent/ {s=$2} END {print (c != "" ? c : s)}'; }
PRIOR=$("$VIVI" need send --from mind --to mind --subject "prior local need" --body "an earlier local item" --done-when "an earlier local item" --project "$A" 2>/dev/null | handle_of)
[ -n "$PRIOR" ] && ok "A filed a prior local need ($PRIOR)" || bad "A prior local need"
# `chat send --ref` is `need send --depends-on`: the cited handle travels in
# the letter's front matter (`depends_on`).
NEED=$("$VIVI" need send --from mind --to "mind@$PB.local" --subject "cross-checkout need" --body "supersedes $PRIOR; please do the thing" --done-when "please do the thing" --depends-on "$PRIOR" --project "$A" 2>/tmp/vc8-need-send.err | handle_of)
echo "$NEED" | grep -qE '^[0-9a-f]{8}$' && ok "A sends a need citing $PRIOR (handle $NEED)" || { bad "A need send"; cat /tmp/vc8-need-send.err; }
"$VIVI" sync --project "$A" >/tmp/vc8-pull-a1.log 2>&1
"$VIVI" sync --project "$B" >/tmp/vc8-pull-b1.log 2>&1
open_has need "$B" "$NEED" && ok "the need files on B under the same handle ($NEED)" || { bad "need did not reach B under handle $NEED"; cat /tmp/vc8-pull-b1.log; }
HA=$(content_of "$NEED" "$A"); HB=$(content_of "$NEED" "$B")
[ -n "$HA" ] && [ "$HA" = "$HB" ] && ok "same content hash on both stores" || bad "content hash differs: A=[$HA] B=[$HB]"
"$VIVI" sync pull --project "$B" >/tmp/vc8-pull-b2.log 2>&1
grep -q ' 0 new' /tmp/vc8-pull-b2.log && ok "a replayed pull files nothing new" || { bad "replayed pull filed new items"; cat /tmp/vc8-pull-b2.log; }
"$VIVI" need done "$NEED" --for mind --project "$B" >/tmp/vc8-need-done.log 2>&1
"$VIVI" sync --project "$B" >/dev/null 2>&1
grep -q "^done $NEED" /tmp/vc8-need-done.log && ! open_has need "$B" "$NEED" && ok "B closes the need and it stays closed after a sync" || { bad "closed need reopened or did not close"; cat /tmp/vc8-need-done.log; }
# B's close travels as an event letter, so A's copy closes with it; the receipt
# (vivi open item 2: not automatic) is a reply sent by hand, which threads.
"$VIVI" mail reply "$NEED" --from mind --body "closed $NEED" --project "$B" >/tmp/vc8-receipt.log 2>&1
"$VIVI" sync --project "$B" >/dev/null 2>&1
"$VIVI" sync --project "$A" >/tmp/vc8-pull-a2.log 2>&1
SHOW=$("$VIVI" need show "$NEED" --project "$A" 2>&1)
echo "$SHOW" | grep -qE '^Thread \(([2-9]|[0-9]{2,}) message' && echo "$SHOW" | grep -q "closed $NEED" \
  && ok "the receipt lands on A and threads to the original" || { bad "receipt did not thread to the original"; cat /tmp/vc8-pull-a2.log; }
# The server shows every member every work item; the client files only what is
# for it. The task is for C's hand: B never files it under its own roles, C does.
"$VIVI" task send --from mind --to "hand@$PC.local" --subject "not for B" --body "directed elsewhere" --project "$A" >/tmp/vc8-dtask.log 2>&1
DTASK=$(handle_of < /tmp/vc8-dtask.log)
"$VIVI" sync --project "$A" >/dev/null 2>&1
"$VIVI" sync --project "$B" >/tmp/vc8-pull-b3.log 2>&1
"$VIVI" sync --project "$C" >/tmp/vc8-pull-c3.log 2>&1
SERVED=$(items_as "$KEY_B" "since=0&limit=1000" | python3 -c '
import json, sys
print(any(r["message_id"].startswith("msg_" + sys.argv[1]) for r in json.load(sys.stdin)["records"]))' "$DTASK")
[ -n "$DTASK" ] && ! open_has task "$B" "$DTASK" mind && ! open_has task "$B" "$DTASK" hand && open_has task "$C" "$DTASK" hand \
  && ok "a task addressed to another seat is not filed on B, and is on C's hand" || { bad "a task for another seat was filed on B (or not on C)"; cat /tmp/vc8-pull-b3.log; }
[ "$SERVED" = True ] && ok "the server still serves that task to B (work is visible to every member)" || bad "the server hid the task from B"

# Item events (1.4.0, unreleased): the author's done event is applied on the
# holder after a pull, is never work, and is served to every member.
EVNEED=$("$VIVI" need send --from mind --to "mind@$PB.local" --subject "event need" --body "A will close this one" --done-when "A will close this one" --project "$A" 2>/dev/null | handle_of)
"$VIVI" sync --project "$A" >/dev/null 2>&1
"$VIVI" sync --project "$B" >/dev/null 2>&1
open_has need "$B" "$EVNEED" && ok "B holds A's second need open ($EVNEED)" || bad "second need did not reach B ($EVNEED)"
"$VIVI" need done "$EVNEED" --for mind --project "$A" >/tmp/vc8-ev-done.log 2>&1
EVH=$(sed -n 's/^done [0-9a-f]*; event \([0-9a-f]\{8\}\).*/\1/p' /tmp/vc8-ev-done.log)
echo "$EVH" | grep -qE '^[0-9a-f]{8}$' && ok "A closes it; the close is an event letter ($EVH)" || { bad "A done printed no event handle"; cat /tmp/vc8-ev-done.log; }
"$VIVI" sync --project "$A" >/tmp/vc8-ev-push.log 2>&1
"$VIVI" sync --project "$B" >/tmp/vc8-ev-pull.log 2>&1
! open_has need "$B" "$EVNEED" && "$VIVI" need list --for mind --status all --project "$B" 2>&1 | grep "$EVNEED" | grep -q ' done ' \
  && grep -qE '[1-9][0-9]* event\(s\) applied' /tmp/vc8-ev-pull.log \
  && ok "A's done event is applied on B after a pull" || { bad "done event not applied on B"; cat /tmp/vc8-ev-push.log /tmp/vc8-ev-pull.log; }

# MCP reads the same fold. PRIOR is still open: it keeps the open list from being
# trivially empty. The event letter is mail on the server and never work.
OPEN=$(mcp_tool "$KEY_B" list '{"kind":"need","status":"open"}')
echo "$OPEN" | python3 -c '
import json, sys
h = {i["handle"] for i in json.load(sys.stdin)["items"]}
need, prior, evneed, evh = sys.argv[1:5]
sys.exit(0 if prior in h and not ({need, evneed, evh} & h) else 1)' "$NEED" "$PRIOR" "$EVNEED" "$EVH" \
  && ok "MCP: the open need list holds PRIOR and neither the closed needs nor the event letter" || { bad "MCP open need list"; echo "$OPEN"; }
ALL=$(mcp_tool "$KEY_B" list '{"kind":"need","status":"all"}')
echo "$ALL" | python3 -c '
import json, sys
items = {i["handle"]: i["status"] for i in json.load(sys.stdin)["items"]}
need, evneed, evh = sys.argv[1:4]
sys.exit(0 if items.get(need) == "done" and items.get(evneed) == "done" and evh not in items else 1)' "$NEED" "$EVNEED" "$EVH" \
  && ok "MCP: both closed needs fold to done, and no event letter is a need" || { bad "MCP all need list"; echo "$ALL"; }
mcp_tool "$KEY_B" show "{\"handle\":\"$EVH\"}" | python3 -c '
import json, sys
i = json.load(sys.stdin)["item"]
sys.exit(0 if i["kind"] == "event" and i["status"] == "event" else 1)' \
  && ok "MCP: show reports the event letter as kind event" || bad "MCP show of the event letter"
# C is neither the sender nor an addressee of that letter; an event is served by
# its header to every member.
curl -s -m 15 -X POST "$BASE/v1/projects/$PID/items/lookup" -H "authorization: Bearer $KEY_C" \
  -H 'content-type: application/json' -d "{\"handles\": [\"$EVH\"]}" | python3 -c '
import base64, json, sys
d = json.load(sys.stdin)
ok = len(d["records"]) == 1 and d["missing"] == [] \
    and b"X-Vivi-Event:" in base64.b64decode(d["records"][0]["raw_eml"])
sys.exit(0 if ok else 1)' \
  && ok "the event letter is served to C, a member it was not addressed to" || bad "the event letter is not visible to C"

# offline queue -> restart -> flush (needs to stop the server: local mode only).
# The seat client's outbox is gone with `vivi chat`; the letters a member could
# not push stay unsent in its mailspace, and the next push sends each once.
if [ "$LOCAL" = 1 ]; then
  kill "$(cat /tmp/vc8-server.pid)" 2>/dev/null; clear_port
  OFFNEED=$("$VIVI" need send --from mind --to "mind@$PB.local" --subject "offline need" --body "written while the server was down" --done-when "written while the server was down" --project "$A" 2>/dev/null | handle_of)
  "$VIVI" sync push --project "$A" >/tmp/vc8-send2.log 2>&1
  "$VIVI" sync status --project "$A" 2>/dev/null | grep -q 'unsent: 1 letter' && ! open_has need "$B" "$OFFNEED" \
    && ok "offline push leaves the letter unsent" || { bad "offline push left the letter unsent"; cat /tmp/vc8-send2.log; }
  boot_server
  "$VIVI" sync push --project "$A" >/tmp/vc8-flush.log 2>&1
  grep -qE '1 sent' /tmp/vc8-flush.log && ok "push after the restart delivers the queued letter" || { bad "flush"; cat /tmp/vc8-flush.log; }
  "$VIVI" sync status --project "$A" 2>/dev/null | grep -q 'unsent: 0 letter' && ok "nothing unsent after the flush" || bad "unsent letters remain after the flush"
  "$VIVI" sync --project "$B" >/tmp/vc8-read2.log 2>&1
  open_has need "$B" "$OFFNEED" && ok "B sees the flushed need" || { bad "B did not file the flushed need"; cat /tmp/vc8-read2.log; }
  COUNT=$("$VIVI" need list --for mind --status all --project "$B" 2>&1 | grep -c "$OFFNEED")
  [ "$COUNT" -eq 1 ] && ok "delivered exactly once" || bad "duplicate delivery: $COUNT"
else
  skip "offline queue/flush (needs a server restart; local mode only)"
fi

# Phase 02 cache lifecycle (P2-7): the project is the durable log, the mailspace
# a replica. Prune evicts the closed need and drops its blob; resync restores it
# byte-for-byte without reopening it. Prune's smallest cutoff is 1m and it compares
# the item's own date, so poll the dry run until the need is old enough instead of
# sleeping a fixed time.
blob_set() { find "$B/.vivi/blobs" -type f -exec sha256sum {} + 2>/dev/null | sort; }
"$VIVI" sync --project "$B" >/dev/null 2>&1   # B must be caught up before the snapshot
echo "waiting for the need to age past the 1m prune cutoff (polling the dry run)"
for _ in $(seq 1 18); do
  "$VIVI" sync prune --older-than 1m --dry-run --project "$B" 2>&1 | grep "$NEED" >/dev/null && break
  sleep 10
done
BEFORE=$(blob_set); NBEFORE=$(echo "$BEFORE" | wc -l)
"$VIVI" sync prune --older-than 1m --project "$B" >/tmp/vc8-prune.log 2>&1
AFTER=$(blob_set); NAFTER=$(echo "$AFTER" | wc -l)
grep -q 'evicted' /tmp/vc8-prune.log && grep -q "$NEED" /tmp/vc8-prune.log && [ "$NAFTER" -lt "$NBEFORE" ] \
  && ok "prune evicts the closed need and drops its blob ($NBEFORE -> $NAFTER blobs)" || { bad "prune did not evict the closed need"; cat /tmp/vc8-prune.log; }
"$VIVI" sync resync --project "$B" >/tmp/vc8-resync1.log 2>&1
grep -q 'restored' /tmp/vc8-resync1.log && [ "$(blob_set)" = "$BEFORE" ] \
  && ok "resync restores the evicted items; the blob set is byte-identical" || { bad "resync did not restore the blob set"; cat /tmp/vc8-resync1.log; }
HB2=$(content_of "$NEED" "$B")
! open_has need "$B" "$NEED" && [ "$HB2" = "$HB" ] && ok "the closed need stays closed and unchanged after resync" || bad "need reopened or changed after resync: before=[$HB] after=[$HB2]"
"$VIVI" sync resync --project "$B" >/tmp/vc8-resync2.log 2>&1
grep -q ' 0 new' /tmp/vc8-resync2.log && ! grep -q 'restored' /tmp/vc8-resync2.log && [ "$(blob_set)" = "$BEFORE" ] \
  && ok "a second resync is a no-op" || { bad "second resync changed something"; cat /tmp/vc8-resync2.log; }

# ---------------------------------------------------------------------------
# 3. Phase 02 wire (R7/R8): a kinded letter on the project wire
# ---------------------------------------------------------------------------
# A push is `PUT .../items/{id}`: the letter is the body, the envelope rides in
# `x-vivi-to` / `x-vivi-kind`. The list echoes the kind and the recipient, and
# serves the letter back byte for byte, so the In-Reply-To and References
# headers (the room send's `in_reply_to` / `refs` fields, now inside the letter)
# survive. Runs last: it is a need addressed to B, and nothing syncs after it.
PROBEID="msg_$(openssl rand -hex 12)"
printf 'From: mind@%s.local\nTo: mind@%s.local\nSubject: smoke need\nMessage-ID: <probe@smoke.local>\nIn-Reply-To: <msg_aaaaaaaaaaaaaaaaaaaaaaaa@smoke.local>\nReferences: <msg_bbbbbbbbbbbbbbbbbbbbbbbb@smoke.local>\nDate: Sun, 11 Oct 2026 12:00:00 +0000\nX-Vivi-Kind: mail\nX-Vivi-Format: 1\n\nkinded smoke\n' "$PB" "$PB" > /tmp/vc8-probe.eml
probe_put() { # id -> http code
  curl -s -m 15 -o /tmp/vc8-probe.out -w '%{http_code}' -X PUT "$BASE/v1/projects/$PID/items/$1" \
    -H "authorization: Bearer $KEY_B" -H 'content-type: message/rfc822' \
    -H "x-vivi-to: mind@$PB.local" -H 'x-vivi-kind: need' --data-binary @/tmp/vc8-probe.eml
}
CODE=$(probe_put "$PROBEID")
WANT=$(sha256sum < /tmp/vc8-probe.eml | awk '{print $1}')
items_as "$KEY_B" "since=0&limit=1000" | python3 -c '
import base64, hashlib, json, sys
pid, want, pb = sys.argv[1:4]
m = [x for x in json.load(sys.stdin)["records"] if x["message_id"] == pid]
raw = base64.b64decode(m[0]["raw_eml"]) if m else b""
ok = m and m[0]["kind"] == "need" and m[0]["to"] == "mind@" + pb + ".local" \
    and hashlib.sha256(raw).hexdigest() == want and m[0]["content_sha256"] == want \
    and b"In-Reply-To: <msg_aaaaaaaaaaaaaaaaaaaaaaaa@smoke.local>" in raw
sys.exit(0 if ok else 1)' "$PROBEID" "$WANT" "$PB" \
  && [ "$CODE" = 201 ] && ok "kinded send echoes kind and recipient, and the letter comes back byte for byte" \
  || { bad "kinded send not echoed ($CODE; server is not the phase 02 build?)"; cat /tmp/vc8-probe.out; }
CODE=$(probe_put "not-a-traveling-id")
[ "$CODE" = "400" ] && ok "kinded send without a traveling Message-ID is rejected" || bad "kinded send with a bad Message-ID returned $CODE (expected 400)"

if [ "$LOCAL" = 1 ]; then kill "$(cat /tmp/vc8-server.pid)" 2>/dev/null; fi
echo "RESULT pass=$PASS fail=$FAIL skip=$SKIP"
[ "$FAIL" -eq 0 ]
