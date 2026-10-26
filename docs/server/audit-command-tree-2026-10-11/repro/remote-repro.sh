#!/bin/bash
# Reproduces the headline findings of audit-remote from a clean harness world.
source /private/tmp/claude-501/-Users-ianzepp-work-ianzepp-vivi/2214cfc8-c994-47ee-8080-9f3236d10e73/scratchpad/audit-remote/env.sh
quiet() { grep -v '^skipped' | grep -v 'more skipped'; }
hdr() { echo; echo "######## $*"; }
tok() { # tok a|b
  local who=$1 name=alice; [ "$who" = b ] && name=bob
  { printf '\x30\x2e\x02\x01\x00\x30\x05\x06\x03\x2b\x65\x70\x04\x22\x04\x20'; cat $([ $who = a ] && echo $A || echo $B)/.vivi/chat/identity.ed25519; } > $W/t.$who.der
  openssl pkey -inform DER -in $W/t.$who.der -out $W/t.$who.priv.pem 2>/dev/null
  openssl pkey -in $W/t.$who.priv.pem -pubout -out $W/t.$who.pub.pem 2>/dev/null
  local pem ch cid sig
  pem=$(python3 -c 'import json,sys; print(json.dumps({"public_key_pem": open(sys.argv[1]).read()}))' $W/t.$who.pub.pem)
  ch=$(curl -s -m 15 -X POST "$BASE/v1/projects/$PID/identities/challenge" -H 'content-type: application/json' -d "$pem")
  cid=$(echo "$ch" | python3 -c 'import json,sys; print(json.load(sys.stdin)["challenge_id"])')
  echo "$ch" | python3 -c 'import json,sys,base64; sys.stdout.buffer.write(base64.b64decode(json.load(sys.stdin)["challenge"]))' > $W/t.$who.chal
  openssl pkeyutl -sign -inkey $W/t.$who.priv.pem -rawin -in $W/t.$who.chal -out $W/t.$who.sig 2>/dev/null
  sig=$(base64 < $W/t.$who.sig | tr -d '\n')
  curl -s -m 15 -X POST "$BASE/v1/projects/$PID/auth/token" -H 'content-type: application/json' -d "{\"name\":\"$name\",\"challenge_id\":\"$cid\",\"signature_base64\":\"$sig\"}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["token"])'
}
for d in "$A" "$B"; do printf '\n[chat]\nserver = "%s"\nrecipient = "hand"\n' "$BASE" >> $d/.vivi/mailspace.toml; done

hdr "R1 no vivi route vouches a member (F1)"
C=$W/carol; mkdir -p $C; $VIVI mailspace init --project $C >/dev/null 2>&1; $VIVI role add hand --kind hand --project $C >/dev/null 2>&1; $VIVI chat status --project $C >/dev/null 2>&1
printf '\n[sync]\nserver = "%s"\nproject = "%s"\nname = "carol"\n' "$BASE" "$PID" >> $C/.vivi/mailspace.toml
{ printf '\x30\x2e\x02\x01\x00\x30\x05\x06\x03\x2b\x65\x70\x04\x22\x04\x20'; cat $C/.vivi/chat/identity.ed25519; } > $C/k.der; openssl pkey -inform DER -in $C/k.der -out $C/k.priv.pem 2>/dev/null; openssl pkey -in $C/k.priv.pem -pubout -out $C/k.pub.pem 2>/dev/null
python3 -c 'import json,sys; print(json.dumps({"vouch":{"name":"carol","public_key_pem":open(sys.argv[1]).read()}}))' $C/k.pub.pem > $C/vouch.json
echo "\$ vivi mail send --from mind --to postmaster ...";            $VIVI mail send --project "$A" --from mind --to postmaster --subject v --body-file $C/vouch.json 2>&1 | head -2
echo "\$ vivi mail send --from mind --to postmaster@\$PA.local ...";  $VIVI mail send --project "$A" --from mind --to postmaster@$PA.local --subject v --body-file $C/vouch.json 2>&1 | head -2
echo "\$ vivi mail send --from mind --to postmaster@deadbeef.local ..."; $VIVI mail send --project "$A" --from mind --to postmaster@deadbeef.local --subject v --body-file $C/vouch.json 2>&1 | head -2
$VIVI sync --project "$A" 2>&1 | quiet
echo "\$ vivi sync join (carol)"; $VIVI sync join --project $C 2>&1 | cut -c1-200

hdr "R2 chat outbox poison blocks every later send, all rooms (F2); chat leave orphans rows (F3)"
$VIVI chat room create --project "$A" --label alice-seat | sed -n 1p
R=$($VIVI chat rooms --project "$A" | awk '{print $1}' | head -1); echo "R=$R"
$VIVI chat join $R --project "$B" --label bob-seat | head -1
R2=$($VIVI chat room create --project "$A" --label alice-2 | sed -n 's/^created //p'); $VIVI chat join $R2 --project "$B" --label bob-2 | head -1
echo "\$ chat send --to nobody";            $VIVI chat send $R --project "$B" --to nobody --subject s --body b | head -1 | cut -c1-230; echo "exit=${PIPESTATUS[0]}"
echo "\$ chat send (good, same room)";      $VIVI chat send $R --project "$B" --subject good --body g | head -1 | cut -c1-230
echo "\$ chat send (good, OTHER room)";     $VIVI chat send $R2 --project "$B" --subject other --body o | head -1 | cut -c1-230
echo "\$ chat flush";                       $VIVI chat flush --project "$B"; echo "exit=$?"
$VIVI chat status --project "$B" | tail -1
echo "A reads room 2 (is 'other' there?):"; $VIVI chat read $R2 --project "$A" --all | tail -1
echo "\$ chat leave R then flush";          $VIVI chat leave $R --project "$B"; $VIVI chat flush --project "$B"; echo "exit=$?"; $VIVI chat status --project "$B" | tail -1

hdr "R3 key loss is a dead end (F4)"
cp $B/.vivi/chat/identity.ed25519 $W/b-key.bak; rm $B/.vivi/chat/identity.ed25519; $VIVI chat status --project "$B" | head -1
echo "\$ vivi sync (B, new key)";      $VIVI sync --project "$B" 2>&1 | quiet | tail -1 | cut -c1-230
echo "\$ vivi sync join (B)";         $VIVI sync join --project "$B" 2>&1 | cut -c1-230
{ printf '\x30\x2e\x02\x01\x00\x30\x05\x06\x03\x2b\x65\x70\x04\x22\x04\x20'; cat $B/.vivi/chat/identity.ed25519; } > $W/b2.der; openssl pkey -inform DER -in $W/b2.der -out $W/b2.priv.pem 2>/dev/null; openssl pkey -in $W/b2.priv.pem -pubout -out $W/b2.pub.pem 2>/dev/null
python3 - $W/b2.pub.pem > $W/vouch-b2.eml <<'PY'
import json,sys
print("From: alice@audit.local\nTo: postmaster@audit.local\nSubject: vouch bob again\nMessage-ID: <vouch-bob-2@audit.local>\nDate: Sun, 11 Oct 2026 12:00:00 +0000\nMIME-Version: 1.0\nContent-Type: text/plain; charset=utf-8\n\n"+json.dumps({"vouch":{"name":"bob","public_key_pem":open(sys.argv[1]).read()}}))
PY
TA=$(tok a)
echo "\$ member vouches the new key under 'bob' (raw PUT, the only way):"
curl -s -m 15 -X PUT "$BASE/v1/projects/$PID/items/msg_$(openssl rand -hex 12)" -H "authorization: Bearer $TA" -H 'content-type: message/rfc822' -H 'x-vivi-to: postmaster' -H 'x-vivi-kind: mail' --data-binary @$W/vouch-b2.eml; echo
cp $W/b-key.bak $B/.vivi/chat/identity.ed25519

hdr "R4 oversize letter: 'queued for sync', then silently never sent (F6); unknown prefix black hole (F7)"
python3 -c 'print("y"*70000)' > $W/big70.txt
$VIVI mail send --project "$A" --from mind --to hand@$PB.local --subject big70k --body-file $W/big70.txt
$VIVI mail send --project "$A" --from mind --to hand@deadbeef.local --subject typo-prefix --body x
echo "\$ sync status (A)"; $VIVI sync status --project "$A" | tail -1
echo "\$ sync push (A)";   $VIVI sync push --project "$A" 2>&1 | grep -v 'already bound'; echo "exit=${PIPESTATUS[0]}"
$VIVI sync --project "$B" 2>&1 | quiet; echo "B has big70k/typo-prefix?"; $VIVI mail list --project "$B" --for hand | grep -E "big70k|typo" || echo "  neither"

hdr "R5 refused record is reported once, then sync is green (F8)"
TA=$(tok a)
cat > $W/forged.eml <<EOM
From: hand@$PB.local
To: mind@$PB.local
Subject: forged-from-bob
Message-ID: <forged-1@$PB.local>
Date: Sun, 11 Oct 2026 22:10:00 +0000
MIME-Version: 1.0
Content-Type: text/plain; charset=utf-8

x
EOM
curl -s -m 15 -o /dev/null -w 'server PUT -> %{http_code}\n' -X PUT "$BASE/v1/projects/$PID/items/msg_$(openssl rand -hex 12)" -H "authorization: Bearer $TA" -H 'content-type: message/rfc822' -H "x-vivi-to: mind@$PB.local" -H 'x-vivi-kind: mail' --data-binary @$W/forged.eml
echo "\$ sync (B) #1"; $VIVI sync --project "$B" 2>&1 | quiet | tail -2 | cut -c1-200; echo "exit=${PIPESTATUS[0]}"
echo "\$ sync (B) #2"; $VIVI sync --project "$B" 2>&1 | quiet | tail -1 | cut -c1-200; echo "exit=${PIPESTATUS[0]}"

hdr "R6 state file: corrupt blocks resync and fetch (F9); cursor ahead silently skips (F10)"
S=$B/.vivi/sync/state.json; cp $S $W/state.good
printf '{"project":"%s","sinc' "$PID" > $S
for c in "sync resync" "sync fetch 835b735d" "sync status"; do echo "\$ vivi $c"; /bin/bash -c "$VIVI $c --project $B" 2>&1 | tail -1 | cut -c1-120; done
python3 - $S "$PID" <<'PY'
import json,sys; json.dump({"project":sys.argv[2],"since":9999,"floor":{"sequence":0}}, open(sys.argv[1],"w"))
PY
$VIVI mail send --project "$A" --from mind --to hand@$PB.local --subject after-ahead --body x >/dev/null; $VIVI sync --project "$A" >/dev/null 2>&1
echo "\$ sync (B) with since=9999"; $VIVI sync --project "$B" 2>&1 | quiet | tail -1; $VIVI mail list --project "$B" --for hand | grep after-ahead || echo "  after-ahead NOT delivered"
cp $W/state.good $S

hdr "R7 fetch of a handle the viewer cannot see: exit 0 (F13)"
H=$($VIVI mail list --project "$A" --for mind --folder sent | grep typo-prefix | awk '{print $1}')
echo "\$ sync fetch $H (B)"; $VIVI sync fetch $H --project "$B"; echo "exit=$?"

hdr "R8 chat watch latency across rooms (F11)"
$VIVI chat flush --project "$B" >/dev/null 2>&1
$VIVI chat join $R --project "$B" --label bob-seat >/dev/null 2>&1; $VIVI chat read $R --mark-read --project "$B" >/dev/null 2>&1; $VIVI chat read $R2 --mark-read --project "$B" >/dev/null 2>&1
$VIVI chat send $R2 --project "$A" --subject s --body "second room" >/dev/null
s=$(date +%s); timeout 60 $VIVI chat watch --once --project "$B" | tail -2; echo "elapsed=$(( $(date +%s)-s ))s"
