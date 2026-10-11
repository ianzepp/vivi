#!/bin/bash
# Project sync smoke: the .eml sync core, end to end.
#
# Two vivi checkouts and one server. Every mailspace has an id; the first 8
# characters are the domain of its addresses, so A writes to B's role by
# address (`hand@<B prefix>.local`) with no local stand-in. A need composed on A
# travels as the exact .eml file: it is stored on the server byte for byte and
# filed on B under the addressed role with an identical hash. A reply composed
# on B routes back to the sender's address and threads on A. Replays change
# nothing. Raw HTTP legs check the server's idempotency, that what it serves is
# exactly what was appended, that a prefix cannot be claimed twice, that mail
# is visible only to its sender and the addressed mailspace, and that a pulled
# letter whose envelope disagrees with its own headers is refused. A push is
# `PUT .../items/{id}` with the letter as the request body.
# The last legs cover history windows: paging and filters on the list, lookup by
# handle, `pull --since` with its floor, `fetch`, `pull --refs`, `prune`, and
# `resync` restoring evicted letters byte for byte.
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
C=$W/c
D=$W/d
PASS=0; FAIL=0
SRVKEY=$(openssl rand -base64 32)
ok() { echo "PASS: $1"; PASS=$((PASS+1)); }
bad() { echo "FAIL: $1"; FAIL=$((FAIL+1)); }

jget() { python3 -c "import json,sys; print(json.load(sys.stdin)$1)"; }
uuid4() { python3 -c 'import uuid; print(uuid.uuid4())'; }

# --- build and boot ---------------------------------------------------------
[ -d "$VIVI_REPO" ] && (cd "$VIVI_REPO" && cargo build --bin vivi 2>&1 | tail -1)
[ -x "$VIVI" ] || { echo "vivi binary not found at $VIVI (set VIVI or VIVI_REPO)"; exit 1; }
rm -rf "$W"; mkdir -p "$W/data" "$A" "$B" "$C" "$D"
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

# --- four checkouts, each with a key ----------------------------------------
# A is "alice" and B is "bob": full mailspaces that sync. C ("carol") and D
# ("dave") are bare keys used for the raw registration and visibility legs.
setup_checkout() { # dir
  "$VIVI" mailspace init --project "$1" >/dev/null 2>&1
  "$VIVI" role add mind --kind mind --project "$1" >/dev/null 2>&1
  "$VIVI" role add hand --kind hand --project "$1" >/dev/null 2>&1
  "$VIVI" chat status --project "$1" >/dev/null 2>&1   # mints the checkout key
}
for d in "$A" "$B" "$C" "$D"; do setup_checkout "$d"; done
# key files: the 32-byte seed -> PKCS#8 -> private and public PEM
make_pems() { # dir tag
  { printf '\x30\x2e\x02\x01\x00\x30\x05\x06\x03\x2b\x65\x70\x04\x22\x04\x20'; cat "$1/.vivi/chat/identity.ed25519"; } > "$W/$2.der"
  openssl pkey -inform DER -in "$W/$2.der" -out "$W/$2.priv.pem" 2>/dev/null
  openssl pkey -in "$W/$2.priv.pem" -pubout -out "$W/$2.pub.pem" 2>/dev/null
}
make_pems "$A" a
make_pems "$B" b
make_pems "$C" c
make_pems "$D" d
[ -s "$W/a.pub.pem" ] && [ -s "$W/b.pub.pem" ] && [ -s "$W/c.pub.pem" ] && [ -s "$W/d.pub.pem" ] \
  && ok "checkout keys derived" || { bad "checkout keys derived"; exit 1; }

# the mailspace ids vivi minted: the prefix is the address domain
id_of() { sed -n 's/^id *= *"\(.*\)"/\1/p' "$1/.vivi/mailspace.toml" | head -1; }
ID_A=$(id_of "$A"); ID_B=$(id_of "$B")
PA=${ID_A:0:8}; PB=${ID_B:0:8}
echo "$ID_A" | grep -qE '^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$' \
  && echo "$ID_B" | grep -qE '^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$' \
  && [ "$PA" != "$PB" ] \
  && ok "each mailspace minted a UUID id (prefixes $PA and $PB)" || { bad "mailspace ids: A=[$ID_A] B=[$ID_B]"; exit 1; }

# --- raw HTTP helpers ------------------------------------------------------
sign_challenge() { # tag -> sets CID and SIG (project must exist: $PID)
  local pem ch
  pem=$(python3 -c 'import json,sys; print(json.dumps({"public_key_pem": open(sys.argv[1]).read()}))' "$W/$1.pub.pem")
  ch=$(curl -s -m 15 -X POST "$BASE/v1/projects/$PID/identities/challenge" -H 'content-type: application/json' -d "$pem")
  CID=$(echo "$ch" | jget '["challenge_id"]')
  echo "$ch" | jget '["challenge"]' | python3 -c 'import base64,sys; sys.stdout.buffer.write(base64.b64decode(sys.stdin.read().strip()))' > "$W/chal.bin"
  openssl pkeyutl -sign -inkey "$W/$1.priv.pem" -rawin -in "$W/chal.bin" -out "$W/sig.bin" 2>/dev/null
  SIG=$(base64 < "$W/sig.bin" | tr -d '\n')
}
bearer() { # name tag  -> token on stdout
  sign_challenge "$2"
  curl -s -m 15 -X POST "$BASE/v1/projects/$PID/auth/token" -H 'content-type: application/json' \
    -d "{\"name\":\"$1\",\"challenge_id\":\"$CID\",\"signature_base64\":\"$SIG\"}" | jget '["token"]'
}
register() { # name tag mailspace_id -> http code (body in $W/register.out)
  sign_challenge "$2"
  python3 -c 'import json,sys; print(json.dumps({"name": sys.argv[1], "public_key_pem": open(sys.argv[2]).read(), "challenge_id": sys.argv[3], "signature_base64": sys.argv[4], "mailspace_id": sys.argv[5]}))' \
    "$1" "$W/$2.pub.pem" "$CID" "$SIG" "$3" \
    | curl -s -m 15 -o "$W/register.out" -w '%{http_code}' -X POST "$BASE/v1/projects/$PID/identities" -H 'content-type: application/json' -d @-
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
get_letter() { # token id -> http code
  curl -s -m 15 -o /dev/null -w '%{http_code}' "$BASE/v1/projects/$PID/items/$2" -H "authorization: Bearer $1"
}
vouch() { # name tag -> http code, as alice (governance mail to postmaster)
  python3 - "$W/$2.pub.pem" "$1" > "$W/vouch-$1.eml" <<'PY'
import json, sys
pem = open(sys.argv[1]).read()
body = json.dumps({"vouch": {"name": sys.argv[2], "public_key_pem": pem}})
print("From: alice@smoke.local\nTo: postmaster@smoke.local\nSubject: vouch " + sys.argv[2] + "\n"
      "Message-ID: <vouch-" + sys.argv[2] + "@smoke.local>\nDate: Sun, 11 Oct 2026 12:00:00 +0000\n"
      "MIME-Version: 1.0\nContent-Type: text/plain; charset=utf-8\n\n" + body)
PY
  append "$TOKEN_A" "msg_$(openssl rand -hex 12)" "$W/vouch-$1.eml" postmaster mail
}

# --- project, config, join -------------------------------------------------
CREATE=$(python3 -c 'import json,sys; print(json.dumps({"public_key_pem": open(sys.argv[1]).read(), "creator_name": "alice"}))' "$W/a.pub.pem" \
  | curl -s -m 15 -X POST "$BASE/v1/projects" -H 'content-type: application/json' -d @-)
PID=$(echo "$CREATE" | jget '["project_id"]' 2>/dev/null)
[ -n "$PID" ] && ok "project created ($PID)" || { bad "project created: $CREATE"; exit 1; }

write_sync_config() { # dir name
  printf '\n[sync]\nserver = "%s"\nproject = "%s"\nname = "%s"\n' "$BASE" "$PID" "$2" >> "$1/.vivi/mailspace.toml"
}
write_sync_config "$A" alice
write_sync_config "$B" bob

"$VIVI" sync join --project "$B" >"$W/join-b0.log" 2>&1 && bad "B joins before it is vouched for" || ok "B cannot join before it is vouched for"
"$VIVI" sync join --project "$A" >"$W/join-a.log" 2>&1 && ok "A (creator) joins" || { bad "A joins"; cat "$W/join-a.log"; }

TOKEN_A=$(bearer alice a)
CODE=$(vouch bob b)
[ "$CODE" = 201 ] && ok "A vouches for B (letter to postmaster)" || { bad "vouch append returned $CODE"; cat "$W/append.out"; }
"$VIVI" sync join --project "$B" >"$W/join-b.log" 2>&1 && ok "B joins after the vouch" || { bad "B joins"; cat "$W/join-b.log"; }

# the directory publishes each member's mailspace id, and vivi can list it
DIR=$(curl -s -m 15 "$BASE/v1/projects/$PID/directory" -H "authorization: Bearer $TOKEN_A")
DIRIDS=$(echo "$DIR" | python3 -c 'import json,sys; print(",".join(sorted(m["name"] + "=" + str(m.get("mailspace_id")) for m in json.load(sys.stdin)["members"])))')
[ "$DIRIDS" = "alice=$ID_A,bob=$ID_B" ] && ok "the directory lists each member's mailspace id" || bad "directory ids: $DIRIDS"
"$VIVI" sync directory --project "$A" 2>&1 | grep -q "$PB.local" \
  && ok "vivi sync directory shows B's address domain ($PB.local)" || bad "sync directory does not show $PB.local"

# --- a prefix is claimed once, and a refusal burns nothing -----------------
CODE=$(vouch carol c); [ "$CODE" = 201 ] || bad "vouch carol returned $CODE"
CODE=$(vouch dave d); [ "$CODE" = 201 ] || bad "vouch dave returned $CODE"
CODE=$(register dave d "not-a-uuid")
[ "$CODE" = 400 ] && ok "a malformed mailspace id is refused (400)" || { bad "malformed id returned $CODE"; cat "$W/register.out"; }
CODE=$(register dave d "$PA-1111-4111-8111-111111111111")
[ "$CODE" = 409 ] && grep -q 'mailspace_prefix_taken' "$W/register.out" \
  && ok "a taken prefix is refused at registration (409 mailspace_prefix_taken)" || { bad "prefix collision returned $CODE"; cat "$W/register.out"; }
CODE=$(register dave d "$(uuid4)")
[ "$CODE" = 201 ] && ok "refusals burn nothing: the admission registers with a fresh id" || { bad "dave registration returned $CODE"; cat "$W/register.out"; }
CODE=$(register carol c "$(uuid4)")
[ "$CODE" = 201 ] && ok "a third member registers with a unique prefix" || { bad "carol registration returned $CODE"; cat "$W/register.out"; }

# --- the core: a need travels as its exact file, addressed by prefix -------
blob_of() { # dir handle -> path of the .eml blob
  local ch
  ch=$("$VIVI" need show "$2" --project "$1" 2>&1 | grep -m1 '^Content:' | awk '{print $2}')
  echo "$1/.vivi/blobs/$(echo "$ch" | cut -c1-2)/$(echo "$ch" | cut -c3-4)/$ch.eml"
}
open_has() { "$VIVI" "$1" list --for "$4" --status open --project "$2" 2>&1 | grep -q "$3"; }
rows_of() { sqlite3 "$1/.vivi/mail.sqlite" "select count(*) from messages" 2>/dev/null; }

NEED=$("$VIVI" need send --from mind --to "hand@$PB.local" --subject "ship the sync" --body "letters travel byte for byte" --project "$A" 2>"$W/need-send.err" | awk '/^created/ {c=$3} /^sent/ {s=$2} END {print (c != "" ? c : s)}')
echo "$NEED" | grep -qE '^[0-9a-f]{8}$' && ok "A composes a need to hand@$PB.local ($NEED)" || { bad "A need send"; cat "$W/need-send.err"; exit 1; }

"$VIVI" sync --project "$A" >"$W/sync-a1.log" 2>&1 && ok "A syncs (push then pull)" || { bad "A sync"; cat "$W/sync-a1.log"; }
"$VIVI" sync --project "$B" >"$W/sync-b1.log" 2>&1 && ok "B syncs" || { bad "B sync"; cat "$W/sync-b1.log"; }

open_has need "$B" "$NEED" hand && ok "the need files on B under role hand, same handle ($NEED)" || { bad "need did not reach B's hand"; cat "$W/sync-b1.log"; }
FA=$(blob_of "$A" "$NEED"); FB=$(blob_of "$B" "$NEED")
HA=$(shasum -a 256 "$FA" 2>/dev/null | awk '{print $1}'); HB=$(shasum -a 256 "$FB" 2>/dev/null | awk '{print $1}')
[ -n "$HA" ] && [ "$HA" = "$HB" ] && ok "the file hash is identical on A and B" || bad "hash differs: A=[$HA] B=[$HB]"
grep -qi "^To:.*hand@$PB.local" "$FA" && grep -qi "^From:.*mind@$PA.local" "$FA" \
  && ok "the letter's own From and To carry the prefixes (inside the hashed bytes)" \
  || bad "letter addresses are not prefix-based: $(grep -iE '^(From|To):' "$FA" | tr '\n' ' ')"

# the server holds exactly that file, with the addresses as its envelope
ITEMS=$(read_items "$(bearer bob b)")
SERVED=$(echo "$ITEMS" | python3 -c '
import base64, hashlib, json, sys
recs = json.load(sys.stdin)["records"]
print(",".join(sorted(r["content_sha256"] + ("=" if hashlib.sha256(base64.b64decode(r["raw_eml"])).hexdigest() == r["content_sha256"] else "!") for r in recs)))')
echo "$SERVED" | grep -q "$HA=" && ok "the server serves that exact file (hash verifies)" || bad "server did not serve the file: $SERVED"
echo "$SERVED" | grep -q '!' && bad "a served record's bytes do not match its hash"
echo "$ITEMS" | python3 -c '
import json, sys
recs = json.load(sys.stdin)["records"]
sys.exit(0 if any(r.get("to") == "hand@" + sys.argv[1] + ".local" and r.get("from") == "alice" for r in recs) else 1)' "$PB" \
  && ok "the envelope recipient is the letter's address" || bad "no record addressed hand@$PB.local from alice"

# --- replays change nothing -------------------------------------------------
RA=$(rows_of "$A"); RB=$(rows_of "$B"); N1=$(echo "$ITEMS" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["records"]))')
"$VIVI" sync --project "$A" >"$W/sync-a2.log" 2>&1; "$VIVI" sync --project "$B" >"$W/sync-b2.log" 2>&1
N2=$(read_items "$(bearer bob b)" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["records"]))')
[ "$(rows_of "$A")" = "$RA" ] && [ "$(rows_of "$B")" = "$RB" ] && [ "$N1" = "$N2" ] \
  && ok "a second sync changes nothing (A rows $RA, B rows $RB, server items $N2)" \
  || bad "replay changed state: A $RA->$(rows_of "$A") B $RB->$(rows_of "$B") server $N1->$N2"
[ "$(id_of "$A")" = "$ID_A" ] && [ "$(id_of "$B")" = "$ID_B" ] && ok "the mailspace ids are stable across runs" || bad "a mailspace id changed"

# --- a reply routes to the sender's address and threads --------------------
"$VIVI" mail reply "$NEED" --from hand --body "closed, thanks" --project "$B" >"$W/reply.log" 2>&1 \
  && ok "B replies with no --to (addressed to the original From)" || { bad "B reply"; cat "$W/reply.log"; }
"$VIVI" sync --project "$B" >"$W/sync-b3.log" 2>&1 && ok "B pushes the reply" || { bad "B sync 3"; cat "$W/sync-b3.log"; }
"$VIVI" sync --project "$A" >"$W/sync-a3.log" 2>&1 && ok "A pulls the reply" || { bad "A sync 3"; cat "$W/sync-a3.log"; }
SHOW=$("$VIVI" need show "$NEED" --project "$A" 2>&1)
case "$SHOW" in
  *"Thread (2 message"*) ok "the reply threads to the original on A" ;;
  *) bad "the reply did not thread on A"; echo "$SHOW" | head -20 ;;
esac
read_items "$TOKEN_A" | python3 -c '
import json, sys
recs = json.load(sys.stdin)["records"]
sys.exit(0 if any(r["from"] == "bob" and r.get("to") == "mind@" + sys.argv[1] + ".local" for r in recs) else 1)' "$PA" \
  && ok "the reply's envelope recipient is mind@$PA.local" || bad "no reply addressed mind@$PA.local from bob"

# --- a second place acting as A files by address (a restore, or a pool slot) ---
# Same mailspace id and the same key as A, an empty store. Filing is by address,
# not by author: the reply lands in mind's inbox, and the need A sent is held as
# a sent copy, exactly what a local send would have written.
A2=$W/a2
mkdir -p "$A2/.vivi/chat"
"$VIVI" mailspace init --project "$A2" >/dev/null 2>&1
"$VIVI" role add mind --kind mind --project "$A2" >/dev/null 2>&1
"$VIVI" role add hand --kind hand --project "$A2" >/dev/null 2>&1
python3 - "$A2/.vivi/mailspace.toml" "$ID_A" <<'PYEOF'
import re, sys
path, ident = sys.argv[1], sys.argv[2]
text = open(path).read()
open(path, "w").write(re.sub(r'(?m)^id *= *".*"$', 'id = "%s"' % ident, text, count=1))
PYEOF
# adopting an id on purpose: the sidecar guard holds the id init minted, so the
# provisioner drops it and the next command rewrites it from the config
rm -f "$A2/.vivi/mailspace.id"
cp "$A/.vivi/chat/identity.ed25519" "$A2/.vivi/chat/identity.ed25519"
write_sync_config "$A2" alice
[ "$(id_of "$A2")" = "$ID_A" ] && ok "the second place carries A's mailspace id" || bad "second place id: $(id_of "$A2")"
"$VIVI" sync --project "$A2" >"$W/sync-a2x.log" 2>&1 && ok "the second place syncs from the server" || { bad "second place sync"; cat "$W/sync-a2x.log"; }
"$VIVI" mail list --for mind --project "$A2" 2>&1 | grep -q "Re: ship the sync" \
  && ok "the reply (From hand@B, To mind@A) is delivered to mind's inbox there" || bad "reply not in mind's inbox on the second place"
"$VIVI" mail list --for mind --folder sent --project "$A2" 2>&1 | grep -q "ship the sync" \
  && ok "the need A sent is held there as a sent copy" || bad "need not held as a sent copy on the second place"
case "$("$VIVI" need show "$NEED" --project "$A2" 2>&1)" in
  *"Thread (2 message"*) ok "the reply threads to the need on the second place" ;;
  *) bad "the reply did not thread on the second place" ;;
esac
"$VIVI" sync --project "$A2" >"$W/sync-a2y.log" 2>&1 && ok "a second sync there is clean" || { bad "second place re-sync"; cat "$W/sync-a2y.log"; }

# --- an envelope that disagrees with the letter is refused at the edge -----
printf 'From: mind@%s.local\nTo: nobody@elsewhere.local\nSubject: lying envelope\nMessage-ID: <lie@smoke.local>\n\nthe envelope says hand@%s.local\n' "$PA" "$PB" > "$W/lie.eml"
CODE=$(append "$TOKEN_A" "msg_$(openssl rand -hex 12)" "$W/lie.eml" "hand@$PB.local" mail)
[ "$CODE" = 201 ] || bad "lying-envelope append returned $CODE"
"$VIVI" sync --project "$B" >"$W/sync-b4.log" 2>&1 && bad "B accepted a letter whose envelope disagrees with its To" \
  || { grep -qi 'envelope' "$W/sync-b4.log" && ok "B refuses a letter whose envelope disagrees with its To (non-zero exit)" || { bad "refusal text missing"; cat "$W/sync-b4.log"; }; }

# --- a From address is an authenticated claim: no member can write another's -
# bob appends a letter whose From header names A's mailspace. The server stamps
# bob's own prefix beside it, so A refuses it instead of filing it as mind's.
TOKEN_BF=$(bearer bob b)
printf 'From: mind@%s.local\nTo: mind@%s.local\nSubject: forged sender\nMessage-ID: <forged@smoke.local>\nDate: Sun, 11 Oct 2026 12:00:00 +0000\n\nI am not mind.\n' "$PA" "$PA" > "$W/forged.eml"
CODE=$(append "$TOKEN_BF" "msg_$(openssl rand -hex 12)" "$W/forged.eml" "mind@$PA.local" mail)
[ "$CODE" = 201 ] && ok "the server accepts a letter it cannot judge (it never reads headers)" || bad "forged-From append returned $CODE"
read_items "$TOKEN_A" | python3 -c '
import json, sys
recs = [r for r in json.load(sys.stdin)["records"] if r["from"] == "bob" and r.get("to") == "mind@" + sys.argv[2] + ".local" and r.get("from_prefix")]
sys.exit(0 if any(r["from_prefix"] == sys.argv[1] for r in recs) else 1)' "$PB" "$PA" \
  && ok "the record carries the sender's registered prefix ($PB), not the one the letter claims" || bad "from_prefix is not the sender's"
"$VIVI" sync --project "$A" >"$W/sync-a-forged.log" 2>&1 && bad "A filed a letter whose From claims A's mailspace but was sent by bob" \
  || { grep -qi 'claims' "$W/sync-a-forged.log" && ok "A refuses the forged From (non-zero exit)" || { bad "forged-From refusal text missing"; cat "$W/sync-a-forged.log"; }; }
"$VIVI" mail list --for mind --project "$A" 2>&1 | grep -q "forged sender" \
  && bad "the forged letter reached mind's inbox" || ok "the forged letter is not in mind's inbox"

# --- server idempotency and visibility, raw ---------------------------------
TOKEN_B=$(bearer bob b)
TOKEN_C=$(bearer carol c)
printf 'From: hand@%s.local\nTo: mind@%s.local\nSubject: raw one\nMessage-ID: <raw1@smoke.local>\n\nraw body\r\nwith CRLF and no trailing newline' "$PB" "$PA" > "$W/raw1.eml"
ID1="msg_$(openssl rand -hex 12)"; ID2="msg_$(openssl rand -hex 12)"
C1=$(append "$TOKEN_B" "$ID1" "$W/raw1.eml" "mind@$PA.local" mail)
SEQ1=$(jget '["sequence"]' < "$W/append.out")
C2=$(append "$TOKEN_B" "$ID1" "$W/raw1.eml" "mind@$PA.local" mail)
C3=$(append "$TOKEN_B" "$ID2" "$W/raw1.eml" "mind@$PA.local" mail)
DUPID=$(jget '["message_id"]' < "$W/append.out")
printf 'From: raw@smoke.local\nSubject: different\n\nother' > "$W/raw2.eml"
C4=$(append "$TOKEN_B" "$ID1" "$W/raw2.eml" "mind@$PA.local" mail)
[ "$C1" = 201 ] && [ "$C2" = 200 ] && [ "$C3" = 200 ] && [ "$DUPID" = "$ID1" ] && [ "$C4" = 409 ] \
  && ok "idempotency: replay 200, same bytes new id 200 (original id), same id new bytes 409" \
  || bad "idempotency codes: $C1 $C2 $C3 (dup id $DUPID) $C4"
BACK=$(read_items "$TOKEN_B" | python3 -c '
import base64, json, sys
r = [x for x in json.load(sys.stdin)["records"] if x["message_id"] == sys.argv[1]]
sys.stdout.buffer.write(base64.b64decode(r[0]["raw_eml"]))' "$ID1" | shasum -a 256 | awk '{print $1}')
WANT=$(shasum -a 256 < "$W/raw1.eml" | awk '{print $1}')
[ "$BACK" = "$WANT" ] && ok "raw bytes (CRLF, no trailing newline) come back identical" || bad "raw round trip differs"

# visibility: that mail is for A's mailspace; carol is a member and sees none of it
CA=$(get_letter "$TOKEN_A" "$ID1"); CB=$(get_letter "$TOKEN_B" "$ID1"); CC=$(get_letter "$TOKEN_C" "$ID1"); CU=$(get_letter "$TOKEN_C" "msg_$(openssl rand -hex 12)")
[ "$CA" = 200 ] && [ "$CB" = 200 ] && [ "$CC" = 404 ] && [ "$CU" = 404 ] \
  && ok "mail is visible to its sender and the addressed prefix only (carol: 404, same as unknown)" \
  || bad "visibility: A=$CA B=$CB C=$CC unknown=$CU"
read_items "$TOKEN_C" | python3 -c '
import json, sys
ids = {r["message_id"] for r in json.load(sys.stdin)["records"]}
sys.exit(1 if sys.argv[1] in ids else 0)' "$ID1" \
  && ok "the list omits mail the member may not see" || bad "carol's list shows mail addressed elsewhere"
read_items "$TOKEN_C" | python3 -c '
import json, sys
kinds = [r["kind"] for r in json.load(sys.stdin)["records"]]
sys.exit(0 if "need" in kinds else 1)' \
  && ok "work items stay visible to every member (carol sees the need)" || bad "carol cannot see the need"

# the push is a raw body: empty is refused, over 64 KiB is refused, exactly 64 KiB is taken
: > "$W/empty.eml"
CODE=$(append "$TOKEN_B" "msg_$(openssl rand -hex 12)" "$W/empty.eml" "" mail)
[ "$CODE" = 400 ] && ok "an empty letter is refused (400)" || bad "empty letter returned $CODE"
python3 -c 'import sys; sys.stdout.buffer.write(b"X" * 65536)' > "$W/at-cap.eml"
python3 -c 'import sys; sys.stdout.buffer.write(b"X" * 65537)' > "$W/over-cap.eml"
C_AT=$(append "$TOKEN_B" "msg_$(openssl rand -hex 12)" "$W/at-cap.eml" "" mail)
C_OVER=$(append "$TOKEN_B" "msg_$(openssl rand -hex 12)" "$W/over-cap.eml" "" mail)
[ "$C_AT" = 201 ] && [ "$C_OVER" = 413 ] && ok "64 KiB is accepted, 64 KiB + 1 is refused (413)" || bad "size cap: at=$C_AT over=$C_OVER"
CODE=$(append "$TOKEN_B" "not-a-traveling-id" "$W/raw1.eml" "" mail)
[ "$CODE" = 400 ] && ok "a non-traveling id is refused (400)" || bad "bad id returned $CODE"
CODE=$(append "$TOKEN_B" "msg_$(openssl rand -hex 12)" "$W/raw1.eml" "alice" mail)
[ "$CODE" = 400 ] && ok "a bare name is no longer a recipient (400)" || bad "bare-name recipient returned $CODE"

# --- the list is paged, filtered, and stamped; a handle is looked up --------
# The window legs: limit/has_more paging, the `after`, `kind` and `to` filters,
# the append time on every record, lookup by handle, and gzip.
items_q() { # token query -> body on stdout
  curl -s -m 15 "$BASE/v1/projects/$PID/items?$2" -H "authorization: Bearer $1"
}
items_code() { # token query -> http code
  curl -s -m 15 -o /dev/null -w '%{http_code}' "$BASE/v1/projects/$PID/items?$2" -H "authorization: Bearer $1"
}
lookup_q() { # token json -> body on stdout, code in $W/lookup.code
  curl -s -m 15 -o "$W/lookup.out" -w '%{http_code}' -X POST "$BASE/v1/projects/$PID/items/lookup" \
    -H "authorization: Bearer $1" -H 'content-type: application/json' -d "$2" > "$W/lookup.code"
  cat "$W/lookup.out"
}

ALL=$(items_q "$TOKEN_A" "since=0&limit=1000")
TOTAL=$(echo "$ALL" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["records"]))')
echo "$ALL" | python3 -c '
import json, sys
d = json.load(sys.stdin)
recs = d["records"]
stamps = [r.get("appended_at") for r in recs]
seqs = [r["sequence"] for r in recs]
ok = bool(recs) and all(isinstance(s, str) and s.endswith("Z") for s in stamps) \
    and stamps == sorted(stamps) and seqs == sorted(seqs) and d.get("has_more") is False
sys.exit(0 if ok else 1)' \
  && ok "every record carries appended_at, monotone with the sequence ($TOTAL records, has_more false)" \
  || bad "appended_at / has_more on the full list"

PAGED=$(python3 - "$BASE" "$PID" "$TOKEN_A" <<'PYEOF'
import json, sys, urllib.request
base, pid, token = sys.argv[1:4]
since, pages, ids, more_seen = 0, 0, [], []
while True:
    req = urllib.request.Request(f"{base}/v1/projects/{pid}/items?since={since}&limit=2",
                                 headers={"authorization": f"Bearer {token}"})
    d = json.load(urllib.request.urlopen(req, timeout=15))
    pages += 1
    assert len(d["records"]) <= 2, "page larger than limit"
    ids += [r["message_id"] for r in d["records"]]
    more_seen.append(d["has_more"])
    if not d["has_more"]:
        break
    assert d["next_cursor"] > since, "cursor did not advance"
    since = d["next_cursor"]
    assert pages < 500
print(pages, len(ids), len(set(ids)), more_seen[0], more_seen[-1])
PYEOF
)
set -- $PAGED
[ "$2" = "$TOTAL" ] && [ "$3" = "$TOTAL" ] && [ "$1" -ge 2 ] && [ "$4" = True ] && [ "$5" = False ] \
  && ok "limit=2 walks the list in $1 pages: every record once, has_more true until the last" \
  || bad "paging: pages=$1 records=$2 distinct=$3 first_more=$4 last_more=$5 total=$TOTAL"

[ "$(items_code "$TOKEN_A" "since=0&limit=0")" = 400 ] && [ "$(items_code "$TOKEN_A" "since=0&limit=abc")" = 400 ] \
  && ok "limit=0 and a non-number are refused (400)" || bad "bad limit was not refused"
items_q "$TOKEN_A" "since=0&kind=need" | python3 -c '
import json, sys
recs = json.load(sys.stdin)["records"]
sys.exit(0 if recs and all(r["kind"] == "need" for r in recs) else 1)' \
  && ok "kind=need returns only needs" || bad "kind filter"
items_q "$TOKEN_A" "since=0&to=$PB" | python3 -c '
import json, sys
recs = json.load(sys.stdin)["records"]
sys.exit(0 if recs and all((r.get("to") or "").lower().endswith("@" + sys.argv[1] + ".local") for r in recs) else 1)' "$PB" \
  && ok "to=<prefix> returns only letters addressed to that mailspace" || bad "to filter"
items_q "$TOKEN_A" "since=0&after=2099-01-01T00:00:00Z" | python3 -c '
import json, sys
d = json.load(sys.stdin)
sys.exit(0 if d["records"] == [] and d["has_more"] is False else 1)' \
  && ok "an after time past every letter returns an empty page" || bad "after filter"

NEEDID=$(echo "$ALL" | python3 -c 'import json,sys; print([r["message_id"] for r in json.load(sys.stdin)["records"] if r["kind"] == "need"][0])')
NEEDH=${NEEDID:4:8}
[ "$NEEDH" = "$NEED" ] && ok "a letter's handle is the 8 hex characters after msg_ ($NEEDH)" || bad "handle $NEEDH is not the need's handle $NEED"
LK=$(lookup_q "$TOKEN_A" "{\"handles\": [\"$NEEDH\", \"0badf00d\"]}")
echo "$LK" | python3 -c '
import json, sys
d = json.load(sys.stdin)
sys.exit(0 if len(d["records"]) == 1 and d["records"][0]["message_id"][4:12] == sys.argv[1] and d["missing"] == ["0badf00d"] else 1)' "$NEEDH" \
  && [ "$(cat "$W/lookup.code")" = 200 ] && ok "lookup returns the letter and names the handle that matched nothing" || bad "lookup: $LK"
lookup_q "$TOKEN_A" '{"handles": ["zz"]}' >/dev/null
[ "$(cat "$W/lookup.code")" = 400 ] && ok "a malformed handle is refused (400)" || bad "bad handle returned $(cat "$W/lookup.code")"
# carol may not see the raw mail from B to A: lookup says missing, as for an unknown handle
HID=${ID1:4:8}
LK=$(lookup_q "$TOKEN_C" "{\"handles\": [\"$HID\"]}")
echo "$LK" | python3 -c '
import json, sys
d = json.load(sys.stdin)
sys.exit(0 if d["records"] == [] and d["missing"] == [sys.argv[1]] else 1)' "$HID" \
  && ok "lookup is scoped by visibility: a letter carol may not see is missing, like an unknown one" || bad "lookup leaked: $LK"
GZ=$(curl -s -m 15 --compressed "$BASE/v1/projects/$PID/items?since=0&limit=1000" -H "authorization: Bearer $TOKEN_A" \
  | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["records"]))')
[ "$GZ" = "$TOTAL" ] && ok "the list decodes the same with Accept-Encoding: gzip ($GZ records)" || bad "gzip list: $GZ vs $TOTAL"
if [ "$LOCAL" = 1 ]; then
  curl -s -m 15 -D - -o /dev/null -H 'accept-encoding: gzip' "$BASE/v1/projects/$PID/items?since=0" -H "authorization: Bearer $TOKEN_A" \
    | grep -qi '^content-encoding: gzip' && ok "the list is gzip-compressed on request" || bad "no content-encoding: gzip"
fi

# --- a window, a handle, a reference, and a prune, on a fresh place --------
# A third place acting as A (same id, same key, empty store) pulls only the
# letters appended from a marker on, then fetches an older one by handle,
# follows a reference, evicts old letters, and resyncs them back byte for byte.
place_of() { # dir mailspace_id key_source_dir
  mkdir -p "$1/.vivi/chat"
  "$VIVI" mailspace init --project "$1" >/dev/null 2>&1
  "$VIVI" role add mind --kind mind --project "$1" >/dev/null 2>&1
  "$VIVI" role add hand --kind hand --project "$1" >/dev/null 2>&1
  python3 - "$1/.vivi/mailspace.toml" "$2" <<'PYEOF'
import re, sys
path, ident = sys.argv[1], sys.argv[2]
text = open(path).read()
open(path, "w").write(re.sub(r'(?m)^id *= *".*"$', 'id = "%s"' % ident, text, count=1))
PYEOF
  rm -f "$1/.vivi/mailspace.id"   # adopt the id on purpose (see the sidecar guard)
  cp "$3/.vivi/chat/identity.ed25519" "$1/.vivi/chat/identity.ed25519"
}
blob_path() { # dir sha -> path of the .eml blob
  echo "$1/.vivi/blobs/${2:0:2}/${2:2:2}/$2.eml"
}
A3=$W/a3
place_of "$A3" "$ID_A" "$A"
write_sync_config "$A3" alice

sleep 2   # the markers land in a later second than every letter above
printf 'From: hand@%s.local\nTo: mind@%s.local\nSubject: window marker one\nMessage-ID: <wm1@smoke.local>\nDate: Mon, 01 Jan 2024 00:00:00 +0000\n\nthe first marker\n' "$PB" "$PA" > "$W/m1.eml"
printf 'From: hand@%s.local\nTo: mind@%s.local\nSubject: window marker two\nMessage-ID: <wm2@smoke.local>\nDate: Mon, 01 Jan 2024 00:00:01 +0000\n\nthe second marker, for context see %s\n' "$PB" "$PA" "$NEED" > "$W/m2.eml"
M1ID="msg_$(openssl rand -hex 12)"; M2ID="msg_$(openssl rand -hex 12)"
TOKEN_BM=$(bearer bob b)
C_M1=$(append "$TOKEN_BM" "$M1ID" "$W/m1.eml" "mind@$PA.local" mail)
C_M2=$(append "$TOKEN_BM" "$M2ID" "$W/m2.eml" "mind@$PA.local" mail)
[ "$C_M1" = 201 ] && [ "$C_M2" = 201 ] && ok "two marker letters are appended after a pause" || bad "marker appends: $C_M1 $C_M2"
read -r WSTAMP WSEQ <<EOF
$(items_q "$TOKEN_A" "since=0&limit=1000" | python3 -c '
import json, sys
r = [x for x in json.load(sys.stdin)["records"] if x["message_id"] == sys.argv[1]][0]
print(r["appended_at"], r["sequence"])' "$M1ID")
EOF
[ -n "$WSTAMP" ] && ok "marker one was appended at $WSTAMP (sequence $WSEQ)" || bad "no appended_at for marker one"

"$VIVI" sync pull --since "$WSTAMP" --refs --project "$A3" >"$W/sync-a3-window.log" 2>&1 \
  && ok "A3 pulls the window from $WSTAMP and follows references" || { bad "A3 windowed pull"; cat "$W/sync-a3-window.log"; }
"$VIVI" mail list --for mind --project "$A3" 2>&1 | grep -q "window marker one" \
  && "$VIVI" mail list --for mind --project "$A3" 2>&1 | grep -q "window marker two" \
  && ok "the window's letters are filed on A3" || bad "markers not filed on A3"
"$VIVI" mail list --for mind --project "$A3" 2>&1 | grep -q "Re: ship the sync" \
  && bad "the reply from before the window was pulled" || ok "letters before the window are not pulled"
grep -qE '1 looked up, 1 filed' "$W/sync-a3-window.log" \
  && "$VIVI" need show "$NEED" --project "$A3" >/dev/null 2>&1 \
  && ok "pull --refs found the handle in marker two's body and filed the need ($NEED)" \
  || { bad "refs did not file the referenced need"; cat "$W/sync-a3-window.log"; }
"$VIVI" sync status --project "$A3" 2>&1 | grep -q "history from: $WSTAMP (sequence $WSEQ)" \
  && ok "sync status records the floor ($WSTAMP, sequence $WSEQ)" \
  || { bad "status floor"; "$VIVI" sync status --project "$A3" 2>&1; }

"$VIVI" sync pull --since 7d --project "$A3" >"$W/sync-a3-since.log" 2>&1 \
  && bad "--since was accepted on a mailspace that already has a cursor" \
  || { grep -qi 'resync' "$W/sync-a3-since.log" && ok "--since is refused once a cursor exists, and says to resync" || { bad "--since refusal text"; cat "$W/sync-a3-since.log"; }; }

REPLYID=$(echo "$ALL" | python3 -c '
import json, sys
recs = [r for r in json.load(sys.stdin)["records"] if r["from"] == "bob" and (r.get("to") or "").lower() == "mind@" + sys.argv[1] + ".local"]
print(sorted(recs, key=lambda r: r["sequence"])[0]["message_id"])' "$PA")
REPLYH=${REPLYID:4:8}
"$VIVI" sync fetch "$REPLYH" --project "$A3" >"$W/fetch-a3.log" 2>&1 && grep -q '1 filed' "$W/fetch-a3.log" \
  && ok "sync fetch files an older letter by handle ($REPLYH)" || { bad "fetch by handle"; cat "$W/fetch-a3.log"; }
case "$("$VIVI" need show "$NEED" --project "$A3" 2>&1)" in
  *"Thread (2 message"*) ok "the fetched reply threads to the need" ;;
  *) bad "the fetched reply did not thread" ;;
esac
"$VIVI" sync status --project "$A3" 2>&1 | grep -q "history from: $WSTAMP (sequence $WSEQ)" \
  && ok "fetch leaves the floor where it was" || bad "fetch moved the floor"
"$VIVI" sync fetch "$REPLYH" --project "$A3" 2>&1 | grep -q '0 filed, 1 already held' \
  && ok "fetching a held handle again files nothing" || bad "repeat fetch"
"$VIVI" sync fetch 0badf00d --project "$A3" 2>&1 | grep -q '1 missing' \
  && ok "a handle that matches nothing is reported as missing, not an error" || bad "missing handle report"
"$VIVI" sync fetch not-a-handle --project "$A3" >/dev/null 2>&1 \
  && bad "a malformed handle was accepted" || ok "a malformed handle is refused before any request"

# prune: the two 2024 markers go (blob and row hidden), the open need and today's reply stay
H1=$(shasum -a 256 "$W/m1.eml" | awk '{print $1}'); H2=$(shasum -a 256 "$W/m2.eml" | awk '{print $1}')
[ -s "$(blob_path "$A3" "$H1")" ] && [ -s "$(blob_path "$A3" "$H2")" ] \
  && ok "A3 holds the markers' exact bytes on disk" || bad "marker blobs missing before the prune"
"$VIVI" sync prune --older-than 1d --dry-run --project "$A3" >"$W/prune-dry.log" 2>&1 \
  && grep -q 'would evict 2 letter' "$W/prune-dry.log" && [ -s "$(blob_path "$A3" "$H1")" ] \
  && ok "prune --dry-run names the two old letters and changes nothing" || { bad "prune dry run"; cat "$W/prune-dry.log"; }
"$VIVI" sync prune --older-than 1d --project "$A3" >"$W/prune.log" 2>&1 \
  && grep -q 'evicted 2 letter' "$W/prune.log" && [ ! -e "$(blob_path "$A3" "$H1")" ] && [ ! -e "$(blob_path "$A3" "$H2")" ] \
  && ok "prune evicts the old letters' blobs and keeps the rest" || { bad "prune"; cat "$W/prune.log"; }
"$VIVI" need show "$NEED" --project "$A3" >/dev/null 2>&1 && ok "the open need is never pruned" || bad "the need was pruned"

# a full replay meets the two bad letters planted above (lying envelope, forged
# From) and refuses them again, so it exits non-zero by design
"$VIVI" sync resync --project "$A3" >"$W/resync-a3.log" 2>&1
grep -q '2 restored' "$W/resync-a3.log" && grep -q '2 refused' "$W/resync-a3.log" \
  && grep -q 'envelope recipients' "$W/resync-a3.log" && grep -q 'claims mailspace' "$W/resync-a3.log" \
  && ok "resync replays the whole project, restores the 2 evicted letters, and refuses the 2 planted bad ones again" \
  || { bad "resync"; cat "$W/resync-a3.log"; }
[ "$(shasum -a 256 "$(blob_path "$A3" "$H1")" 2>/dev/null | awk '{print $1}')" = "$H1" ] \
  && [ "$(shasum -a 256 "$(blob_path "$A3" "$H2")" 2>/dev/null | awk '{print $1}')" = "$H2" ] \
  && ok "the restored letters are byte for byte what was appended" || bad "restored bytes differ"
"$VIVI" sync status --project "$A3" 2>&1 | grep -q 'history from: full history' \
  && ok "after a resync the floor is full history" || bad "floor after resync"
"$VIVI" sync --project "$A3" >"$W/sync-a3-clean.log" 2>&1 && grep -q ' 0 new' "$W/sync-a3-clean.log" \
  && ok "a plain sync after the resync is clean" || { bad "sync after resync"; cat "$W/sync-a3-clean.log"; }

# an empty window, then a plain pull, stays inside the window
A4=$W/a4
place_of "$A4" "$ID_A" "$A"
write_sync_config "$A4" alice
"$VIVI" sync pull --since 2099-01-01 --project "$A4" >"$W/sync-a4.log" 2>&1 && grep -q ' 0 fetched' "$W/sync-a4.log" \
  && "$VIVI" sync status --project "$A4" 2>&1 | grep -q 'history from: 2099-01-01T00:00:00Z (no letters yet)' \
  && ok "an empty window records the requested time as its floor" || { bad "empty window"; cat "$W/sync-a4.log"; }

echo "RESULT pass=$PASS fail=$FAIL"
[ "$FAIL" -eq 0 ]
