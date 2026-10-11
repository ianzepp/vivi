#!/bin/bash
# Project sync smoke: the .eml sync core, end to end.
#
# Two vivi checkouts and one server. A need composed on A travels as the exact
# .eml file: it is stored on the server byte for byte and filed on B with an
# identical hash. A reply composed on B travels back and threads on A. Replays
# change nothing. Raw HTTP legs check the server's idempotency and that what it
# serves is exactly what was appended. A push is `PUT .../items/{id}` with the
# letter as the request body.
#
# Local (default): boots target/debug/vivi-server from this repo on $PORT.
# Deployed: SERVER_URL=https://host scripts/project-sync-smoke.sh runs the same
# legs against that server; each run leaves one throwaway project behind.
#
# Environment, all optional:
#   SERVER_URL  run against a deployed server instead of booting one
#   SRV         this repo (default: the script's parent directory)
#   VIVI_REPO   the vivi checkout (default: $SRV/../vivi)
#   VIVI        the vivi binary (default: $VIVI_REPO/target/debug/vivi)
#   PORT        local server port (default: 18124)
set -u
[ -d /opt/homebrew/bin ] && export PATH=/opt/homebrew/bin:$PATH
SRV=${SRV:-$(cd "$(dirname "$0")/.." && pwd)}
VIVI_REPO=${VIVI_REPO:-$SRV/../vivi}
VIVI=${VIVI:-$VIVI_REPO/target/debug/vivi}
PORT=${PORT:-18124}
BASE=${SERVER_URL:-}
BASE=${BASE%/}
LOCAL=0
if [ -z "$BASE" ]; then LOCAL=1; BASE="http://127.0.0.1:$PORT"; fi
W=/tmp/ps-smoke
A=$W/a
B=$W/b
PASS=0; FAIL=0
SRVKEY=$(openssl rand -base64 32)
ok() { echo "PASS: $1"; PASS=$((PASS+1)); }
bad() { echo "FAIL: $1"; FAIL=$((FAIL+1)); }

jget() { python3 -c "import json,sys; print(json.load(sys.stdin)$1)"; }

# --- build and boot ---------------------------------------------------------
[ -d "$VIVI_REPO" ] && (cd "$VIVI_REPO" && cargo build --bin vivi 2>&1 | tail -1)
[ -x "$VIVI" ] || { echo "vivi binary not found at $VIVI (set VIVI or VIVI_REPO)"; exit 1; }
rm -rf "$W"; mkdir -p "$W/data" "$A" "$B"
if [ "$LOCAL" = 1 ]; then
  (cd "$SRV" && cargo build --bin vivi-server 2>&1 | tail -1)
  [ -x "$SRV/target/debug/vivi-server" ] || { echo "vivi-server binary not found under $SRV"; exit 1; }
  for _ in $(seq 1 30); do curl -s -m 1 "$BASE/healthz" >/dev/null 2>&1 || break; sleep 1; done
  PORT=$PORT DATA_DIR="$W/data" STORE_ENCRYPTION_KEY="$SRVKEY" "$SRV/target/debug/vivi-server" >"$W/server.log" 2>&1 &
  SPID=$!
  trap 'kill $SPID 2>/dev/null' EXIT
fi
UP=0
for _ in $(seq 1 40); do
  curl -s -m 2 "$BASE/healthz" 2>/dev/null | grep -q '"ok"' && { UP=1; break; }
  sleep 0.5
done
[ "$UP" = 1 ] && ok "server up" || { bad "server up"; echo "RESULT pass=$PASS fail=$FAIL"; exit 1; }

# --- two checkouts, each with a key ----------------------------------------
# A is "alice" with a local stand-in role "bob"; B is "bob" with "alice".
setup_checkout() { # dir peer
  "$VIVI" mailspace init --project "$1" >/dev/null 2>&1
  "$VIVI" role add mind --kind mind --project "$1" >/dev/null 2>&1
  "$VIVI" role add "$2" --kind hand --project "$1" >/dev/null 2>&1
  "$VIVI" chat status --project "$1" >/dev/null 2>&1   # mints the checkout key
}
setup_checkout "$A" bob
setup_checkout "$B" alice
# key files: the 32-byte seed -> PKCS#8 -> private and public PEM
make_pems() { # dir tag
  { printf '\x30\x2e\x02\x01\x00\x30\x05\x06\x03\x2b\x65\x70\x04\x22\x04\x20'; cat "$1/.vivi/chat/identity.ed25519"; } > "$W/$2.der"
  openssl pkey -inform DER -in "$W/$2.der" -out "$W/$2.priv.pem" 2>/dev/null
  openssl pkey -in "$W/$2.priv.pem" -pubout -out "$W/$2.pub.pem" 2>/dev/null
}
make_pems "$A" a
make_pems "$B" b
[ -s "$W/a.pub.pem" ] && [ -s "$W/b.pub.pem" ] && ok "checkout keys derived" || { bad "checkout keys derived"; exit 1; }

# --- raw HTTP helpers ------------------------------------------------------
bearer() { # name tag  -> token on stdout (project must exist: $PID)
  local pem ch cid sig
  pem=$(python3 -c 'import json,sys; print(json.dumps({"public_key_pem": open(sys.argv[1]).read()}))' "$W/$2.pub.pem")
  ch=$(curl -s -m 15 -X POST "$BASE/v1/projects/$PID/identities/challenge" -H 'content-type: application/json' -d "$pem")
  cid=$(echo "$ch" | jget '["challenge_id"]')
  echo "$ch" | jget '["challenge"]' | python3 -c 'import base64,sys; sys.stdout.buffer.write(base64.b64decode(sys.stdin.read().strip()))' > "$W/chal.bin"
  openssl pkeyutl -sign -inkey "$W/$2.priv.pem" -rawin -in "$W/chal.bin" -out "$W/sig.bin" 2>/dev/null
  sig=$(base64 < "$W/sig.bin" | tr -d '\n')
  curl -s -m 15 -X POST "$BASE/v1/projects/$PID/auth/token" -H 'content-type: application/json' \
    -d "{\"name\":\"$1\",\"challenge_id\":\"$cid\",\"signature_base64\":\"$sig\"}" | jget '["token"]'
}
append() { # token id emlfile to kind -> http code (body in $W/append.out)
  # The letter is the request body, byte for byte; the envelope rides in headers.
  local extra=()
  [ -n "$4" ] && extra+=(-H "x-vivi-to: $4")
  [ -n "$5" ] && extra+=(-H "x-vivi-kind: $5")
  curl -s -m 15 -o "$W/append.out" -w '%{http_code}' -X PUT "$BASE/v1/projects/$PID/items/$2" \
    -H "authorization: Bearer $1" -H 'content-type: message/rfc822' ${extra[@]+"${extra[@]}"} --data-binary @"$3"
}
read_items() { # token -> body on stdout
  curl -s -m 15 "$BASE/v1/projects/$PID/items?since=0" -H "authorization: Bearer $1"
}

# --- project, config, join -------------------------------------------------
CREATE=$(python3 -c 'import json,sys; print(json.dumps({"public_key_pem": open(sys.argv[1]).read(), "creator_name": "alice"}))' "$W/a.pub.pem" \
  | curl -s -m 15 -X POST "$BASE/v1/projects" -H 'content-type: application/json' -d @-)
PID=$(echo "$CREATE" | jget '["project_id"]' 2>/dev/null)
[ -n "$PID" ] && ok "project created ($PID)" || { bad "project created: $CREATE"; exit 1; }

write_sync_config() { # dir name peer
  printf '\n[sync]\nserver = "%s"\nproject = "%s"\nname = "%s"\nrecipient = "mind"\n\n[sync.names]\n%s = "%s"\n' \
    "$BASE" "$PID" "$2" "$3" "$3" >> "$1/.vivi/mailspace.toml"
}
write_sync_config "$A" alice bob
write_sync_config "$B" bob alice

"$VIVI" sync join --project "$B" >"$W/join-b0.log" 2>&1 && bad "B joins before it is vouched for" || ok "B cannot join before it is vouched for"
"$VIVI" sync join --project "$A" >"$W/join-a.log" 2>&1 && ok "A (creator) joins" || { bad "A joins"; cat "$W/join-a.log"; }

# A vouches for B: a letter to postmaster carrying the vouch (governance mail)
TOKEN_A=$(bearer alice a)
python3 - "$W/b.pub.pem" > "$W/vouch.eml" <<'PY'
import json, sys
pem = open(sys.argv[1]).read()
body = json.dumps({"vouch": {"name": "bob", "public_key_pem": pem}})
print("From: alice@smoke.local\nTo: postmaster@smoke.local\nSubject: vouch bob\n"
      "Message-ID: <vouch-bob@smoke.local>\nDate: Sun, 11 Oct 2026 12:00:00 +0000\n"
      "MIME-Version: 1.0\nContent-Type: text/plain; charset=utf-8\n\n" + body)
PY
CODE=$(append "$TOKEN_A" "msg_$(openssl rand -hex 12)" "$W/vouch.eml" postmaster mail)
[ "$CODE" = 201 ] && ok "A vouches for B (letter to postmaster)" || { bad "vouch append returned $CODE"; cat "$W/append.out"; }
"$VIVI" sync join --project "$B" >"$W/join-b.log" 2>&1 && ok "B joins after the vouch" || { bad "B joins"; cat "$W/join-b.log"; }

# --- the core: a need travels as its exact file -----------------------------
blob_of() { # dir handle -> path of the .eml blob
  local ch
  ch=$("$VIVI" need show "$2" --project "$1" 2>&1 | grep -m1 '^Content:' | awk '{print $2}')
  echo "$1/.vivi/blobs/$(echo "$ch" | cut -c1-2)/$(echo "$ch" | cut -c3-4)/$ch.eml"
}
open_has() { "$VIVI" "$1" list --for mind --status open --project "$2" 2>&1 | grep -q "$3"; }
rows_of() { sqlite3 "$1/.vivi/mail.sqlite" "select count(*) from messages" 2>/dev/null; }

NEED=$("$VIVI" need send --from mind --to mind --subject "ship the sync" --body "letters travel byte for byte" --project "$A" 2>/dev/null | awk '/^created/ {print $3}')
echo "$NEED" | grep -qE '^[0-9a-f]{8}$' && ok "A composes a need locally ($NEED)" || { bad "A need send"; exit 1; }

"$VIVI" sync --project "$A" >"$W/sync-a1.log" 2>&1 && ok "A syncs (push then pull)" || { bad "A sync"; cat "$W/sync-a1.log"; }
"$VIVI" sync --project "$B" >"$W/sync-b1.log" 2>&1 && ok "B syncs" || { bad "B sync"; cat "$W/sync-b1.log"; }

open_has need "$B" "$NEED" && ok "the need files on B under the same handle ($NEED)" || { bad "need did not reach B"; cat "$W/sync-b1.log"; }
FA=$(blob_of "$A" "$NEED"); FB=$(blob_of "$B" "$NEED")
HA=$(shasum -a 256 "$FA" 2>/dev/null | awk '{print $1}'); HB=$(shasum -a 256 "$FB" 2>/dev/null | awk '{print $1}')
[ -n "$HA" ] && [ "$HA" = "$HB" ] && ok "the file hash is identical on A and B" || bad "hash differs: A=[$HA] B=[$HB]"

# the server holds exactly that file
ITEMS=$(read_items "$(bearer bob b)")
SERVED=$(echo "$ITEMS" | python3 -c '
import base64, hashlib, json, sys
recs = json.load(sys.stdin)["records"]
print(",".join(sorted(r["content_sha256"] + ("=" if hashlib.sha256(base64.b64decode(r["raw_eml"])).hexdigest() == r["content_sha256"] else "!") for r in recs)))')
echo "$SERVED" | grep -q "$HA=" && ok "the server serves that exact file (hash verifies)" || bad "server did not serve the file: $SERVED"
echo "$SERVED" | grep -q '!' && bad "a served record's bytes do not match its hash"

# --- replays change nothing -------------------------------------------------
RA=$(rows_of "$A"); RB=$(rows_of "$B"); N1=$(echo "$ITEMS" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["records"]))')
"$VIVI" sync --project "$A" >"$W/sync-a2.log" 2>&1; "$VIVI" sync --project "$B" >"$W/sync-b2.log" 2>&1
N2=$(read_items "$(bearer bob b)" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["records"]))')
[ "$(rows_of "$A")" = "$RA" ] && [ "$(rows_of "$B")" = "$RB" ] && [ "$N1" = "$N2" ] \
  && ok "a second sync changes nothing (A rows $RA, B rows $RB, server items $N2)" \
  || bad "replay changed state: A $RA->$(rows_of "$A") B $RB->$(rows_of "$B") server $N1->$N2"

# --- a reply travels back and threads --------------------------------------
"$VIVI" mail reply "$NEED" --from mind --to alice --body "closed, thanks" --project "$B" >"$W/reply.log" 2>&1 \
  && ok "B composes a reply to the need" || { bad "B reply"; cat "$W/reply.log"; }
"$VIVI" sync --project "$B" >"$W/sync-b3.log" 2>&1 && ok "B pushes the reply" || { bad "B sync 3"; cat "$W/sync-b3.log"; }
"$VIVI" sync --project "$A" >"$W/sync-a3.log" 2>&1 && ok "A pulls the reply" || { bad "A sync 3"; cat "$W/sync-a3.log"; }
SHOW=$("$VIVI" need show "$NEED" --project "$A" 2>&1)
case "$SHOW" in
  *"Thread (2 message"*) ok "the reply threads to the original on A" ;;
  *) bad "the reply did not thread on A"; echo "$SHOW" | head -20 ;;
esac

# --- server idempotency, raw ------------------------------------------------
TOKEN_B=$(bearer bob b)
printf 'From: raw@smoke.local\nTo: bob@smoke.local\nSubject: raw one\nMessage-ID: <raw1@smoke.local>\n\nraw body\r\nwith CRLF and no trailing newline' > "$W/raw1.eml"
ID1="msg_$(openssl rand -hex 12)"; ID2="msg_$(openssl rand -hex 12)"
C1=$(append "$TOKEN_B" "$ID1" "$W/raw1.eml" alice mail)
SEQ1=$(jget '["sequence"]' < "$W/append.out")
C2=$(append "$TOKEN_B" "$ID1" "$W/raw1.eml" alice mail)
C3=$(append "$TOKEN_B" "$ID2" "$W/raw1.eml" alice mail)
DUPID=$(jget '["message_id"]' < "$W/append.out")
printf 'From: raw@smoke.local\nSubject: different\n\nother' > "$W/raw2.eml"
C4=$(append "$TOKEN_B" "$ID1" "$W/raw2.eml" alice mail)
[ "$C1" = 201 ] && [ "$C2" = 200 ] && [ "$C3" = 200 ] && [ "$DUPID" = "$ID1" ] && [ "$C4" = 409 ] \
  && ok "idempotency: replay 200, same bytes new id 200 (original id), same id new bytes 409" \
  || bad "idempotency codes: $C1 $C2 $C3 (dup id $DUPID) $C4"
BACK=$(read_items "$TOKEN_B" | python3 -c '
import base64, json, sys
r = [x for x in json.load(sys.stdin)["records"] if x["message_id"] == sys.argv[1]]
sys.stdout.buffer.write(base64.b64decode(r[0]["raw_eml"]))' "$ID1" | shasum -a 256 | awk '{print $1}')
WANT=$(shasum -a 256 < "$W/raw1.eml" | awk '{print $1}')
[ "$BACK" = "$WANT" ] && ok "raw bytes (CRLF, no trailing newline) come back identical" || bad "raw round trip differs"
# the push is a raw body: empty is refused, over 64 KiB is refused, exactly 64 KiB is taken
: > "$W/empty.eml"
CODE=$(append "$TOKEN_B" "msg_$(openssl rand -hex 12)" "$W/empty.eml" alice mail)
[ "$CODE" = 400 ] && ok "an empty letter is refused (400)" || bad "empty letter returned $CODE"
python3 -c 'import sys; sys.stdout.buffer.write(b"X" * 65536)' > "$W/at-cap.eml"
python3 -c 'import sys; sys.stdout.buffer.write(b"X" * 65537)' > "$W/over-cap.eml"
C_AT=$(append "$TOKEN_B" "msg_$(openssl rand -hex 12)" "$W/at-cap.eml" alice mail)
C_OVER=$(append "$TOKEN_B" "msg_$(openssl rand -hex 12)" "$W/over-cap.eml" alice mail)
[ "$C_AT" = 201 ] && [ "$C_OVER" = 413 ] && ok "64 KiB is accepted, 64 KiB + 1 is refused (413)" || bad "size cap: at=$C_AT over=$C_OVER"
CODE=$(append "$TOKEN_B" "not-a-traveling-id" "$W/raw1.eml" alice mail)
[ "$CODE" = 400 ] && ok "a non-traveling id is refused (400)" || bad "bad id returned $CODE"

echo "RESULT pass=$PASS fail=$FAIL"
[ "$FAIL" -eq 0 ]
