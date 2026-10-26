#!/bin/bash
# Clean-world reproduction of the local-group findings. Run after audit-setup.sh.
source /private/tmp/claude-501/-Users-ianzepp-work-ianzepp-vivi/2214cfc8-c994-47ee-8080-9f3236d10e73/scratchpad/audit-local/env.sh
va() { "$VIVI" "$@" --project "$A"; }
vb() { "$VIVI" "$@" --project "$B"; }
BODY=$'done_when: it works\nwrite_scope: src/'
h() { echo; echo "##### $*"; }

h "F1 setup: A sends need/task/want to B's hand, and a local task to its own hand"
N1=$(va need send --from mind --to hand@$PB.local --subject "N1 need to bob" --body "$BODY" | sed -n 's/^sent //p')
T1=$(va task send --from mind --to hand@$PB.local --subject "T1 task to bob" --body "$BODY" | sed -n 's/^sent //p')
W1=$(va want send --from mind --to hand@$PB.local --subject "W1 want to bob" --body "$BODY" | sed -n 's/^sent //p')
T3=$(va task send --from mind --to hand --subject "T3 alice-local task" --body "$BODY" | sed -n 's/^created hand //p')
echo "N1=$N1 T1=$T1 W1=$W1 T3=$T3"
h "F1 A: vivi step (before any sync) -- sent copies dispatch, want included, kind=sent"
va step
h "F1 A: graph activate on a sent copy"
va graph activate "$T1" --task "$T1"; echo "[exit $?]"

h "F2 sync A then B; B: vivi step lists alice's local task T3"
va sync; vb sync
vb step
h "F2 B: board does NOT list T3, boot counts it"
vb board | sed -n 1,12p
vb boot | sed -n '/^totals/p;/handles/,/^$/p'
h "F2 B: activate alice's local task, then try to close it"
vb graph activate "$T3" --task "$T3" | head -8
vb task done "$T3" --for hand; echo "[exit $?]"

h "F3 dependency edges lost on pull: B sends T5 depending on N1 (pulled need)"
T5=$(vb task send --from hand --to mind@$PA.local --subject "T5 depends on N1" --body "$BODY" --depends-on "$N1" | sed -n 's/^sent //p')
echo "T5=$T5"
echo "--- B graph ready (T5 blocked)"; vb graph ready | sed -n 4,8p
vb sync; va sync
echo "--- A graph ready (T5 ready, nothing blocked)"; va graph ready | sed -n 4,8p
echo "--- A graph audit (clean)"; va graph audit
echo "--- A: header carries the dependency"; f=$(grep -rl "T5 depends on N1" "$A/.vivi/blobs" | head -1); grep -i "x-vivi-depends-on" "$f"
h "F3b --depends-on on a sent copy is refused"
va task send --from mind --to hand@$PB.local --subject "T2" --body "$BODY" --depends-on "$T1"; echo "[exit $?]"

h "F4 closing on the other side never reaches the sender's step"
vb task done "$T1" --for hand --verdict clean_pass --repo r --tip abc --note shipped
vb sync; va sync
echo "--- A step still dispatches T1"; va step | grep "$T1"
echo "--- A step --apply"; va step --apply "$T1" | sed -n '/reason/,$p'

h "F5 boot unabsorbed mail holds the governance letter nobody can absorb"
va boot | sed -n '/unabsorbed mail/,/^$/p'
va mail absorb "$(va boot | sed -n 's/^  \([0-9a-f]\{8\}\) .*vouch bob/\1/p' | head -1)" --for mind; echo "[exit $?]"

h "F6 rename alias filing and unknown-role warning"
va role rename hand worker
vb task send --from mind --to hand@$PA.local --subject "R1 old name" --body "$BODY" >/dev/null
vb task send --from mind --to ghost@$PA.local --subject "R3 unknown role" --body "$BODY" >/dev/null
vb sync >/dev/null
echo "--- A pull (stdout then stderr)"; va sync pull
echo "--- A board has worker's R1 but not ghost's R3"; va board | grep -E "R1|R3"; echo "(R3 grep exit $?)"
echo "--- A step has both"; va step | grep -E "R1|R3"

h "F7 prune evicts sent copies of open work; audit calls them deleted"
va sync prune --older-than 1s | head -8
va graph audit | head -6
va step | sed -n '/reason/,$p' | head
va sync resync | tail -1

h "F8 memo delete trashes any record; trashed work still dispatches"
va memo delete "$N1" --for mind
va step | grep "$N1"

h "F9 rules do not sync; memo dump has no bodies"
va rule save --for mind --subject "Always test" --body "run tests"
va sync push
vb rule list
va memo save --for mind --subject "s" --body "hello body" >/dev/null
va memo dump --for mind --json | grep '"body"'
