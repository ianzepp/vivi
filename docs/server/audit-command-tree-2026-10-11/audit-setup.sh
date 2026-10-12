#!/bin/bash
# audit-setup.sh WORKDIR PORT
#
# Boots a private local vivi-server on PORT and builds two synced checkouts of
# one project under WORKDIR, then writes WORKDIR/env.sh for `source`.
#
#   A = alice, mailspace roles: mind, hand      ($A, prefix $PA, address mind@$PA.local)
#   B = bob,   mailspace roles: mind, hand      ($B, prefix $PB, address hand@$PB.local)
#
# Both have [sync] configured, have joined, and are admitted (alice created the
# project and vouched for bob). Nothing is pushed yet. Local only: 127.0.0.1.
# Stop the server with: kill $(cat WORKDIR/server.pid)
set -u
[ -d /opt/homebrew/bin ] && export PATH=/opt/homebrew/bin:$PATH
W=${1:?workdir}; PORT=${2:?port}
VIVI=${VIVI:-/Users/ianzepp/work/ianzepp/vivi/target/debug/vivi}
SRVBIN=${SRVBIN:-/Users/ianzepp/work/ianzepp/vivi-server/target/debug/vivi-server}
BASE=http://127.0.0.1:$PORT
A=$W/a; B=$W/b
[ -x "$VIVI" ] && [ -x "$SRVBIN" ] || { echo "missing binary: $VIVI or $SRVBIN"; exit 1; }
rm -rf "$W"; mkdir -p "$W/data" "$A" "$B"
jget() { python3 -c "import json,sys; print(json.load(sys.stdin)$1)"; }

PORT=$PORT DATA_DIR="$W/data" STORE_ENCRYPTION_KEY="$(openssl rand -base64 32)" \
  nohup "$SRVBIN" >"$W/server.log" 2>&1 &
echo $! > "$W/server.pid"
UP=0
for _ in $(seq 1 40); do
  curl -s -m 2 "$BASE/healthz" 2>/dev/null | grep -q '"ok"' && { UP=1; break; }
  sleep 0.5
done
[ "$UP" = 1 ] || { echo "server did not come up; see $W/server.log"; exit 1; }

setup_checkout() {
  "$VIVI" mailspace init --project "$1" >/dev/null 2>&1
  "$VIVI" role add mind --kind mind --project "$1" >/dev/null 2>&1
  "$VIVI" role add hand --kind hand --project "$1" >/dev/null 2>&1
  "$VIVI" chat status --project "$1" >/dev/null 2>&1   # mints the checkout key
}
setup_checkout "$A"; setup_checkout "$B"
make_pems() { # dir tag
  { printf '\x30\x2e\x02\x01\x00\x30\x05\x06\x03\x2b\x65\x70\x04\x22\x04\x20'; cat "$1/.vivi/chat/identity.ed25519"; } > "$W/$2.der"
  openssl pkey -inform DER -in "$W/$2.der" -out "$W/$2.priv.pem" 2>/dev/null
  openssl pkey -in "$W/$2.priv.pem" -pubout -out "$W/$2.pub.pem" 2>/dev/null
}
make_pems "$A" a; make_pems "$B" b
id_of() { sed -n 's/^id *= *"\(.*\)"/\1/p' "$1/.vivi/mailspace.toml" | head -1; }
ID_A=$(id_of "$A"); ID_B=$(id_of "$B"); PA=${ID_A:0:8}; PB=${ID_B:0:8}

CREATE=$(python3 -c 'import json,sys; print(json.dumps({"public_key_pem": open(sys.argv[1]).read(), "creator_name": "alice"}))' "$W/a.pub.pem" \
  | curl -s -m 15 -X POST "$BASE/v1/projects" -H 'content-type: application/json' -d @-)
PID=$(echo "$CREATE" | jget '["project_id"]' 2>/dev/null)
[ -n "$PID" ] || { echo "project create failed: $CREATE"; exit 1; }
for pair in "$A:alice" "$B:bob"; do
  printf '\n[sync]\nserver = "%s"\nproject = "%s"\nname = "%s"\n' "$BASE" "$PID" "${pair#*:}" >> "${pair%%:*}/.vivi/mailspace.toml"
done

"$VIVI" sync join --project "$A" >"$W/join-a.log" 2>&1 || { echo "A join failed"; cat "$W/join-a.log"; exit 1; }
# alice vouches for bob with governance mail to postmaster, then bob joins
sign_challenge() {
  local pem ch
  pem=$(python3 -c 'import json,sys; print(json.dumps({"public_key_pem": open(sys.argv[1]).read()}))' "$W/$1.pub.pem")
  ch=$(curl -s -m 15 -X POST "$BASE/v1/projects/$PID/identities/challenge" -H 'content-type: application/json' -d "$pem")
  CID=$(echo "$ch" | jget '["challenge_id"]')
  echo "$ch" | jget '["challenge"]' | python3 -c 'import base64,sys; sys.stdout.buffer.write(base64.b64decode(sys.stdin.read().strip()))' > "$W/chal.bin"
  openssl pkeyutl -sign -inkey "$W/$1.priv.pem" -rawin -in "$W/chal.bin" -out "$W/sig.bin" 2>/dev/null
  SIG=$(base64 < "$W/sig.bin" | tr -d '\n')
}
sign_challenge a
TOKEN_A=$(curl -s -m 15 -X POST "$BASE/v1/projects/$PID/auth/token" -H 'content-type: application/json' \
  -d "{\"name\":\"alice\",\"challenge_id\":\"$CID\",\"signature_base64\":\"$SIG\"}" | jget '["token"]')
python3 - "$W/b.pub.pem" > "$W/vouch-bob.eml" <<'PY'
import json, sys
pem = open(sys.argv[1]).read()
body = json.dumps({"vouch": {"name": "bob", "public_key_pem": pem}})
print("From: alice@audit.local\nTo: postmaster@audit.local\nSubject: vouch bob\n"
      "Message-ID: <vouch-bob@audit.local>\nDate: Sun, 11 Oct 2026 12:00:00 +0000\n"
      "MIME-Version: 1.0\nContent-Type: text/plain; charset=utf-8\n\n" + body)
PY
CODE=$(curl -s -m 15 -o /dev/null -w '%{http_code}' -X PUT "$BASE/v1/projects/$PID/items/msg_$(openssl rand -hex 12)" \
  -H "authorization: Bearer $TOKEN_A" -H 'content-type: message/rfc822' -H 'x-vivi-to: postmaster' -H 'x-vivi-kind: mail' \
  --data-binary @"$W/vouch-bob.eml")
[ "$CODE" = 201 ] || { echo "vouch failed: $CODE"; exit 1; }
"$VIVI" sync join --project "$B" >"$W/join-b.log" 2>&1 || { echo "B join failed"; cat "$W/join-b.log"; exit 1; }

cat > "$W/env.sh" <<EOF
export PATH=/opt/homebrew/bin:\$PATH
export VIVI=$VIVI
export BASE=$BASE
export PID=$PID
export W=$W
export A=$A
export B=$B
export ID_A=$ID_A
export ID_B=$ID_B
export PA=$PA
export PB=$PB
EOF
echo "ready: server $BASE (pid $(cat "$W/server.pid")), project $PID"
echo "  A=$A  mind@$PA.local, hand@$PA.local   (alice)"
echo "  B=$B  mind@$PB.local, hand@$PB.local   (bob)"
echo "  source $W/env.sh"
