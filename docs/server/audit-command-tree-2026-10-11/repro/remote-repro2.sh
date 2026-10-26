#!/bin/bash
source /private/tmp/claude-501/-Users-ianzepp-work-ianzepp-vivi/2214cfc8-c994-47ee-8080-9f3236d10e73/scratchpad/audit-remote/env.sh
quiet() { grep -v '^skipped' | grep -v 'more skipped'; }
hdr() { echo; echo "######## $*"; }

hdr "R9 pool dispatch ignores a task the other member already completed (F15)"
rm -rf $W/poolrepo $W/poolroot-a $W/poolroot-b; mkdir -p $W/poolrepo $W/poolroot-a $W/poolroot-b
( cd $W/poolrepo && git init -q -b main && git config user.email t@example.invalid && git config user.name t && echo hello > README && git add README && git commit -q -m init )
for pair in "$A:poolroot-a" "$B:poolroot-b"; do d=${pair%%:*}; r=${pair##*:}
cat >> $d/.vivi/mailspace.toml <<EOM

[pools.here]
provider = "local"
slots = 1

[pools.here.local]
root = "$W/$r"
worktree_of = "$W/poolrepo"

[pools.here.harness]
name = "echo"
run = "echo receipt-for {model}"
models = ["fake/model"]

[pools.here.committer]
name = "audit-bot"
email = "audit-bot@example.invalid"
EOM
done
$VIVI task send --project "$A" --from mind --to hand@$PB.local --subject "pool job" --body "branch: audit-branch
from: main
do a thing" | head -1
$VIVI sync --project "$A" 2>&1 | quiet; $VIVI sync --project "$B" 2>&1 | quiet
T=$($VIVI mail list --project "$B" --for hand --folder tasks | grep "pool job" | awk '{print $1}'); echo "task handle $T"
$VIVI pool create --porcelain --project "$B" | tail -1
echo "\$ B dispatch + wait + done"; $VIVI pool dispatch $T --model fake/model --porcelain --project "$B" | cut -c1-110; $VIVI pool wait $T --porcelain --timeout 60 --project "$B"; $VIVI task done $T --for hand --project "$B"
$VIVI sync --project "$B" 2>&1 | quiet; $VIVI sync --project "$A" 2>&1 | quiet
echo "\$ A (sender, holds sent copy) dispatches the SAME task, B already did and closed it"
$VIVI pool create --porcelain --project "$A" | tail -1
$VIVI pool dispatch $T --model fake/model --branch audit-branch-a --porcelain --project "$A" | cut -c1-110; echo "exit=${PIPESTATUS[0]}"
$VIVI pool wait $T --porcelain --timeout 60 --project "$A"
echo "replies in the thread (all synced as letters):"; $VIVI mail thread $T --project "$A" | grep -c '^Subject: Re: pool job'
$VIVI pool destroy --all --yes --project "$A" | tail -1; $VIVI pool destroy --all --yes --project "$B" | tail -1
echo "\$ pool destroy --porcelain"; $VIVI pool destroy --all --yes --porcelain --project "$B" 2>&1 | head -2

hdr "R10 prune evicts a sent letter; mail show has no hint (F16)"
$VIVI sync prune --older-than 1m --dry-run --project "$A" | head -3
sleep 61; $VIVI sync prune --older-than 1m --project "$A" | head -3
H=$($VIVI mail list --project "$A" --for mind --folder sent 2>&1 | head -1 | awk '{print $1}'); echo "handle(first sent after prune) $H"
HH=$(sqlite3 $A/.vivi/mail.sqlite "select substr(message_id,5,8) from messages where deleted_at is not null limit 1"); echo "evicted handle $HH"
$VIVI mail show $HH --project "$A" 2>&1 | head -2

hdr "R11 switching [sync] project: history is never re-pushed, one cursor (F14)"
CREATE=$(python3 -c 'import json,sys; print(json.dumps({"public_key_pem": open(sys.argv[1]).read(), "creator_name": "bob"}))' $W/b.pub.pem | curl -s -m 15 -X POST "$BASE/v1/projects" -H 'content-type: application/json' -d @-)
P2=$(echo "$CREATE" | python3 -c 'import json,sys; print(json.load(sys.stdin)["project_id"])'); echo "P2=$P2"
cp $B/.vivi/mailspace.toml $W/b-ms.bak
python3 - $B/.vivi/mailspace.toml $P2 <<'PY'
import sys,re
p,new=sys.argv[1:3]; s=open(p).read()
s=re.sub(r'(\[sync\][^\[]*?project = ")[^"]*(")', lambda m: m.group(1)+new+m.group(2), s, count=1, flags=re.S); open(p,'w').write(s)
PY
$VIVI sync join --project "$B" | head -1
echo "\$ sync (B under P2): every old letter is 'already bound to vivi-project <P1>'"; $VIVI sync --project "$B" 2>&1 | head -4 | cut -c1-140
cp $W/b-ms.bak $B/.vivi/mailspace.toml
echo "\$ back on P1:"; $VIVI sync status --project "$B" | grep -E "cursor|history"

hdr "R12 two syncs at once on one checkout (F12)"
for r in 1 2 3; do
  for i in 1 2 3 4 5; do $VIVI mail send --project "$A" --from mind --to hand@$PB.local --subject "conc r$r n$i" --body x >/dev/null; done
  ( $VIVI sync --project "$A" >$W/c1.$r 2>&1; echo "exit=$?" >>$W/c1.$r ) &
  ( $VIVI sync --project "$A" >$W/c2.$r 2>&1; echo "exit=$?" >>$W/c2.$r ) &
  wait
done
grep -h -E "database is locked|exit=[1-9]|rate limit" $W/c1.* $W/c2.* | sort | uniq -c | head
