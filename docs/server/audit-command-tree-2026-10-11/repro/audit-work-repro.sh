# consolidated repro (runs against a fresh harness world)
cd /private/tmp/claude-501/-Users-ianzepp-work-ianzepp-vivi/2214cfc8-c994-47ee-8080-9f3236d10e73/scratchpad && bash audit-setup.sh /private/tmp/claude-501/-Users-ianzepp-work-ianzepp-vivi/2214cfc8-c994-47ee-8080-9f3236d10e73/scratchpad/audit-work 18141 >/dev/null 2>&1
source /private/tmp/claude-501/-Users-ianzepp-work-ianzepp-vivi/2214cfc8-c994-47ee-8080-9f3236d10e73/scratchpad/audit-work/env.sh
id() { awk '/^sent/{print $2}'; }
sy() { "$VIVI" sync --project "$A" >/dev/null 2>&1; "$VIVI" sync --project "$B" >/dev/null 2>&1; "$VIVI" sync --project "$A" >/dev/null 2>&1; }
N=$("$VIVI" need send --project "$A" --from mind@$PA.local --to hand@$PB.local --subject "N1" --body "done_when: n1" | id)
T=$("$VIVI" task send --project "$A" --from mind@$PA.local --to hand@$PB.local --subject "T1" --body "done_when: t1" | id)
W=$("$VIVI" want send --project "$A" --from mind@$PA.local --to hand@$PB.local --subject "W1" --body "w1" | id)
sy
echo "## F2: A cannot list what it sent"
"$VIVI" need list --project "$A" --from mind@$PA.local --status all
"$VIVI" task dump --project "$A" --to hand@$PB.local --status all | sed -n 3p
"$VIVI" mail list --project "$A" --for mind@$PA.local --folder sent | head -3
echo "## F3: --depends-on a remote-sent item"
"$VIVI" task send --project "$A" --from mind@$PA.local --to hand@$PB.local --subject "T2" --body "done_when: t2" --depends-on $T; echo "exit=$?"
echo "## F1: B closes, A learns nothing (no note) / a mail only (note)"
"$VIVI" task done --project "$B" --for hand@$PB.local $T
"$VIVI" need done --project "$B" --for hand@$PB.local $N --note "N done by bob"
sy
"$VIVI" graph ready --project "$A" | sed -n 3,5p
"$VIVI" graph ready --project "$B" | sed -n 3,5p
"$VIVI" mail list --project "$A" --for mind@$PA.local
echo "## F6: A closes own sent copy"
"$VIVI" need done --project "$A" --for mind@$PA.local $N
"$VIVI" need list --project "$A" --for mind@$PA.local --status all
"$VIVI" graph ready --project "$A" | sed -n 3,5p
"$VIVI" step --project "$A" | sed -n 3,6p
echo "## F6b: reopen of sent copy lands in A's own tasks folder"
"$VIVI" task reopen --project "$A" --for mind@$PA.local $T
"$VIVI" task list --project "$A" --for mind@$PA.local
