cd /private/tmp/claude-501/-Users-ianzepp-work-ianzepp-vivi/2214cfc8-c994-47ee-8080-9f3236d10e73/scratchpad && bash audit-setup.sh /private/tmp/claude-501/-Users-ianzepp-work-ianzepp-vivi/2214cfc8-c994-47ee-8080-9f3236d10e73/scratchpad/audit-work 18141 >/dev/null 2>&1
source /private/tmp/claude-501/-Users-ianzepp-work-ianzepp-vivi/2214cfc8-c994-47ee-8080-9f3236d10e73/scratchpad/audit-work/env.sh
id() { awk '/^sent/{print $2}'; }
sy() { "$VIVI" sync --project "$A" >/dev/null 2>&1; "$VIVI" sync --project "$B" >/dev/null 2>&1; "$VIVI" sync --project "$A" >/dev/null 2>&1; }
echo "## F4"
T1=$("$VIVI" task send --project "$A" --from mind@$PA.local --to hand@$PB.local --subject "T1" --body "done_when: t1" | id)
sy
"$VIVI" task send --project "$B" --from mind@$PB.local --to hand@$PA.local --subject "BT" --body "done_when: bt" --depends-on $T1
sy
echo "-- B (sender): graph"; "$VIVI" graph ready --project "$B" | sed -n 3,6p
echo "-- A (receiver): graph"; "$VIVI" graph ready --project "$A" | sed -n 3,6p
echo "-- A: task list --blocked"; "$VIVI" task list --project "$A" --for hand@$PA.local --blocked
echo "-- A: step dispatches BT?"; "$VIVI" step --project "$A" | grep -c BT
echo "## F5 watch"
OUT=$W/wf5.out
"$VIVI" need watch --project "$B" --for hand@$PB.local --timeout 6s > $OUT 2>&1 &
sleep 1
"$VIVI" need send --project "$A" --from mind@$PA.local --to hand@$PB.local --subject "wake" --body "done_when: w" >/dev/null
sy
wait; cat $OUT
echo "-- list shows it:"; "$VIVI" need list --project "$B" --for hand@$PB.local
echo "## F7 foreign-held"
IT=$("$VIVI" task send --project "$B" --from mind@$PB.local --to hand@$PB.local --subject "B-internal" --body "done_when: i" | awk '/^created/{print $3}')
sy
"$VIVI" step --project "$A" | grep B-internal
"$VIVI" task done --project "$A" --for hand@$PB.local $IT; echo "exit=$?"
echo "## F11 unknown prefix"
"$VIVI" need send --project "$A" --from mind@$PA.local --to hand@deadbeef.local --subject "typo" --body "done_when: x"; echo "exit=$?"
echo "## F10 promote before push"
W=$("$VIVI" want send --project "$A" --from mind@$PA.local --to hand@$PB.local --subject "W-pre" --body "w" | id)
"$VIVI" want promote --project "$A" --for mind@$PA.local $W
sy
"$VIVI" want list --project "$B" --for hand@$PB.local --status all
"$VIVI" need list --project "$B" --for hand@$PB.local --status all | grep W-pre
echo "## F9 kind mismatch"
W2=$("$VIVI" want send --project "$A" --from mind@$PA.local --to hand@$PB.local --subject "W-kind" --body "w" | id)
sy
"$VIVI" want done --project "$B" --for hand@$PB.local $W2
"$VIVI" need reopen --project "$B" --for hand@$PB.local $W2
"$VIVI" need list --project "$B" --for hand@$PB.local | grep W-kind
echo "## F8 prune"
sleep 62
"$VIVI" sync prune --project "$A" --older-than 1m | head -4
"$VIVI" step --project "$A" | grep item_missing | head -2
