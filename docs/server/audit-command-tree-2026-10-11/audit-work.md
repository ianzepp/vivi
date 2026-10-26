# Audit: work (need, task, want, trace)

Binary: `vivi 10.2.0` (target/debug), local `vivi-server`, two synced checkouts A (alice) and B (bob).
Server and checkouts were reset between scenarios; every repro below was re-run end to end on a fresh world.

## Summary
33 leaf commands: WORKS 5, WORKS-LOCAL 4, DIVERGES 12, BROKEN 12, UNTESTED 0.
The sync layer moves letters correctly (push, pull, resync, role rename, trace across a synced thread all work). The lifecycle layer is local-only and nothing carries it across.
Top findings:
1. F1: every state move (done, reopen, promote, drop, bind, absorb, priority, verdict) stays on the side that ran it. The only signal is an optional `--note`, which travels as a plain `Re:` mail with no machine-readable meaning. The author's item stays open in `graph ready`, `step` and `need bind` units. `need closed on B, open on A` is confirmed.
2. F5: `need/task/want watch` never wake for anything that arrived by `sync pull` (event kind is `sync`, filtered out). A pulled need is invisible to every watcher; a loop built on `need watch` never sees remote work or the reply to a close.
3. F4/F3: dependencies cannot be declared on an item sent to a remote recipient (`has role 'sent'`), and a `X-Vivi-Depends-On` header is ignored on pull, so the receiver's `graph ready`/`step` dispatch a task whose prerequisite is open while `task list --blocked` says it is blocked.
Also: the sender cannot list its own open remote-sent work (F2), `step` on the sender dispatches work it handed to the other side and work held under a foreign address (F7), `sync prune` evicts open outbound work (F8).

Harness note: re-running `audit-setup.sh` on the same PORT while the previous server is still up does not start a new server (bind fails); the old server keeps serving and `server.pid` ends up naming a dead process. Use `lsof -iTCP:PORT` to find the live pid. Results above are unaffected (each world got its own project id).

## Matrix
Common preamble for all evidence: `source $W/env.sh`; `id() { awk '/^sent/{print $2}'; }`. `$N/$T/$W` below are handles printed by the matching `send`.

| command | class | result | evidence |
| --- | --- | --- | --- |
| task send | SYNCED-WRITE | BROKEN | Plain send to `hand@$PB.local` WORKS (`queued for sync: hand@..`, B pulls it into `tasks`). `--depends-on <handle of an item sent to the remote>` fails: `error: ... dependency '5758a6a9' has role 'sent'; expected a task, need, or want handle` (exit 1). See F3, F4. |
| task from | SYNCED-WRITE | WORKS | `$VIVI task from --project $B $W --for hand@$PB.local --to hand@$PA.local --subject s --body 'done_when: x'` -> `sent fda2a87b / queued for sync`, A pulls it as a task, trace links it to the want. Source must be a want (`task from currently supports wants only; .. is need`). Without `--to`: `delivery needs at least one To or Cc recipient`. |
| task watch | LOCAL-ONLY | BROKEN | Watcher on B for `hand@$PB.local`; A sends a task, both sync, B lists it, watcher: `error: mailspace watch timed out without a matching event`. See F5. |
| task list | SYNCED-READ | BROKEN | Pulled tasks list fine (`--for/--from <remote addr>/--to`). Tasks the caller sent to a remote role are never listed: `task list --project $A --to hand@$PB.local --status all` -> `no tasks in tasks/done`. See F2. |
| task show | SYNCED-READ | WORKS | `task show --project $A 5758a6a9` shows the sent copy and, after sync, the pulled `Re:` replies as `[mail captured]` in the thread; `--json` OK. |
| task dump | SYNCED-READ | BROKEN | Same hole as list: `task dump --project $A --to hand@$PB.local --status all` -> `count: 0`. Pulled tasks dump fine, including `--participant <remote addr>`. See F2. |
| task absorb | LOCAL-ONLY | DIVERGES | Absorbed open task disappears from `board` (`tasks: 0`) but stays in `graph ready` and `step` dispatches; `task done` then refuses (`is absorbed and can no longer be changed`). The other side never learns. See F14. |
| task done | LOCAL-ONLY (+mail with --note) | DIVERGES | B: `task done --for hand@$PB.local $T` -> `done`, `sync unsent: 0`; A: `graph ready` still lists `$T`. With `--note` a plain `Re:` mail is pushed, state unchanged on A. `--verdict/--repo/--tip` stay in B's `mailspace_item_metadata`, not in the letter. Run on A's own sent copy: role becomes `done` but the graph node stays `open` (F6). See F1, F6. |
| task reopen | LOCAL-ONLY (+mail with --note) | DIVERGES | B reopens, A unaware (no letter without note). On A's sent copy it moves into A's own `tasks` folder: `task reopen --for mind@$PA.local $T` -> `task list --for mind@$PA.local` shows `$T  open  tasks`, while B holds the same task open. Also accepts any kind (F9). |
| need send | SYNCED-WRITE | BROKEN | Plain send to remote role WORKS, unknown remote role is filed on B under its name with `warning: filed for unknown role ghost` (exit 0) and appears once `role add ghost` runs. `--depends-on` has the F3/F4 problems. `--to hand@deadbeef.local` (no such member) is accepted silently (F11). `--reply-to <h>` with no `--to` errors (F12). |
| need watch | LOCAL-ONLY | BROKEN | Control: B sends a need to its own hand while the watcher runs -> `event=delivered ... handle=7ac05042`. Needs pulled from A: no event. See F5. |
| need list | SYNCED-READ | BROKEN | `need list --project $A --from mind@$PA.local --status all` -> `no needs in needs/done` for a need A sent to B. Only shows after A closes the sent copy. See F2. |
| need show | SYNCED-READ | WORKS | `need show --project $A $N` shows sent copy plus pulled `Re:` replies; shows `units` line after bind. |
| need dump | SYNCED-READ | BROKEN | `need dump --project $A --from mind@$PA.local --status all` -> `count: 0` (F2). |
| need absorb | LOCAL-ONLY | DIVERGES | Sealed open need stays `open` in `need list`, vanishes from `board`, still dispatched by `step`, and `need done/reopen/mark` all refuse. Sender may absorb its own sent copy. See F14. |
| need done | LOCAL-ONLY (+mail with --note) | DIVERGES | The key case. B `need done --for hand@$PB.local $N`; sync both; A: `need list` empty, `graph ready` lists `$N`, `step` dispatches it. With `--note` A's inbox gets `Re: N1` mail only. Done twice with note sends two letters. See F1, F6. |
| need reopen | LOCAL-ONLY (+mail with --note) | DIVERGES | Same silence as done; accepts a want or task handle and moves it into `needs` (F9). |
| need bind | LOCAL-ONLY | DIVERGES | A binds its sent copies (`bound $N`), B binds and completes both units, need joins to `done` on B. A afterwards: `need show` -> `units 1e14341c:open 969def26:open`, need open, `graph ready` lists all three. Binding is independent on each side and never reconciled. `need bind $N $N` binds a need to itself (`bound`). See F1. |
| need mark | LOCAL-ONLY | WORKS-LOCAL | `marked 6b57b4c2 blocked`; `sync unsent: 0`; `need list --mark blocked` works on the pulled item; no letter. A mark on a remote-sent copy is only visible in `need show --json` (not listable, F2). |
| need unmark | LOCAL-ONLY | WORKS-LOCAL | `unmarked ..`, second run `already unmarked`; local only. |
| want send | SYNCED-WRITE | BROKEN | Plain send WORKS (B lists it as open want). `--depends-on` as for task (F3/F4). |
| want watch | LOCAL-ONLY | BROKEN | Same event filter as task/need watch (F5). |
| want list | SYNCED-READ | BROKEN | A: `want list --project $A --for mind@$PA.local --status all` -> `no wants` right after sending a want to B (F2). `--mark` and `--sort priority` work on pulled wants. |
| want show | SYNCED-READ | WORKS | Thread shows pulled replies; `--json` OK (shows marks, not priority metadata, on a sent copy). |
| want dump | SYNCED-READ | BROKEN | A: `want dump --for mind@$PA.local --status all` -> `count: 0`; B (receiver) dumps the want fine (F2). |
| want absorb | LOCAL-ONLY | DIVERGES | Same as need/task absorb; absorbed want blocks `promote`/`set-priority` (F14). |
| want set-priority | LOCAL-ONLY | DIVERGES | On a pulled want WORKS-LOCAL (`want list --sort priority` ranks it). On the author's sent copy: `updated 64be4ed8` (exit 0) but `want list --for mind@$PA.local --status all` -> `no wants` and `want show --json` has no priority; value only in `mailspace_item_metadata`. Never sent to the other side. |
| want promote | LOCAL-ONLY (+mail with --note) | DIVERGES | B promotes a pulled want: B holds a need, A's sent copy is still a want. A promoting its own sent copy files the want into A's own `needs` queue while B holds it as a want. Before the first push the promote rides along (letter travels as a need, F10), after the push it is silent. Accepts task handles (F9). |
| want done | LOCAL-ONLY (+mail with --note) | DIVERGES | Same as need done (F1, F6). `task done <want handle>` also succeeds (F9). |
| want drop | LOCAL-ONLY (+mail with --note) | DIVERGES | `dropped 4f37dff5`; with `--note obsolete now` A gets `Re: W-drop` mail only; A's want stays open (F1). |
| want mark | LOCAL-ONLY | WORKS-LOCAL | Same as need mark; `want list --mark urgent` works. |
| want unmark | LOCAL-ONLY | WORKS-LOCAL | Same as need unmark. |
| trace | SYNCED-READ | WORKS | 4-hop thread need -> reply need -> task -> mail reply, A and B print identical node and edge sets (`captured`/`inferred`); `--json` and `--limit` honored; unknown handle -> `message not found` exit 1. Local `event` edges differ per side (expected, ledger is local). Shows other members' internal items held by address (F7). |

## Findings

### F1 Close, reopen, promote, drop, bind, priority and verdict never reach the other side — DIVERGES
- Repro (fresh world; B closes what A sent):
  ```
  N=$($VIVI need send --project $A --from mind@$PA.local --to hand@$PB.local --subject N1 --body 'done_when: n1' | id)
  T=$($VIVI task send --project $A --from mind@$PA.local --to hand@$PB.local --subject T1 --body 'done_when: t1' | id)
  $VIVI sync --project $A; $VIVI sync --project $B
  $VIVI task done --project $B --for hand@$PB.local $T
  $VIVI need done --project $B --for hand@$PB.local $N --note 'N done by bob'
  $VIVI sync --project $B; $VIVI sync --project $A
  $VIVI graph ready --project $A      # ready 2650b565, 5758a6a9 (T), 7c7b39fe (N)   <- both still open on A
  $VIVI graph ready --project $B      # ready: only the want
  $VIVI step --project $A             # still dispatches T and N (kind "sent")
  $VIVI mail list --project $A --for mind@$PA.local   # only "Re: N1" (the note); nothing for T
  ```
- Observed: without `--note` no letter exists at all (`sync unsent: 0`). With `--note` the letter is `X-Vivi-Kind: mail`, `Subject: Re: ...`, body = the note text; it carries no done/reopen/promote marker, so nothing can act on it mechanically. Verified for done, reopen, want promote, want drop. `--verdict clean_pass --repo vivi --tip abc1234` are written only to B's local item metadata; the note letter has the note body only. Absorb, mark, set-priority, need bind (and the need-join that completes a bound need) are silent too. A's `need bind` units stay `open` after B finished every unit.
- Expected: the author (or any member holding a copy) should be able to see that the item was closed, by whom, and with what verdict; a closed need on one side should not be a ready node on the other.
- Likely cause: `src/mailspace/delivery.rs` `Mailspace::move_item`: with a note it composes a `note_reply` mail, without one it only calls `move_message_to_role` and logs a local event. Sync (`src/sync/push.rs`, `select.rs`) selects letters only; the event ledger is never transported. Severity: blocks use for any Mind/Hand loop that crosses mailspaces (surprises when only humans read mail).

### F2 The author cannot list or dump work it sent to a remote role — BROKEN
- Repro:
  ```
  $VIVI need send --project $A --from mind@$PA.local --to hand@$PB.local --subject N1 --body 'done_when: n'
  $VIVI need list --project $A --from mind@$PA.local --status all     # no needs in needs/done
  $VIVI need list --project $A --to hand@$PB.local --status all       # no needs in needs/done
  $VIVI need dump --project $A --participant mind@$PA.local --status all   # count: 0
  $VIVI mail list --project $A --for mind@$PA.local --folder sent    # lists it
  $VIVI graph ready --project $A                                      # lists it
  ```
  The same holds for task and want `list`/`dump` and for `board`.
- Observed: the only local row is role `sent`; `list`/`dump` read roles `needs|tasks|wants|done` (`status_roles`, `want_status_roles` in `src/local_work_command.rs`), so the item is invisible with every filter. After `need done --for mind@$PA.local $N` on the sent copy it suddenly appears as `done`. Marks and priority set on the sent copy are therefore unlistable too.
- Expected: `--from <me>` / `--status all` should include what I sent to a remote mailspace (the docs promise "every member holds every work item").
- Likely cause: list/dump filter on folder role; remote-sent items never get a work folder (`src/mailspace/backlog.rs` mints the node, no folder row). Severity: surprises, hides open outbound work from the author.

### F3 `--depends-on` refuses any item that was sent to a remote recipient — BROKEN
- Repro:
  ```
  T=$($VIVI task send --project $A --from mind@$PA.local --to hand@$PB.local --subject T1 --body 'done_when: t1' | id)
  $VIVI task send --project $A --from mind@$PA.local --to hand@$PB.local --subject T2 --body 'done_when: t2' --depends-on $T
  # error: message error: dependency '5758a6a9' has role 'sent'; expected a task, need, or want handle   (exit 1)
  ```
  Same for `need send --depends-on <need>` and `want send --depends-on`.
- Observed: a Mind cannot chain two work items it hands to a remote Hand. The sent copy is the item's node in the backlog graph (`attach_sent_work`), so the refusal is inconsistent with the graph.
- Likely cause: `resolve_backlog_dep` in `src/mailspace/backlog.rs` matches `tasks|needs|wants|done` and rejects `sent`. Severity: blocks use for ordered cross-mailspace work.

### F4 The dependency header travels but the receiver builds no edge: ready vs blocked disagree — DIVERGES
- Repro:
  ```
  T1=$($VIVI task send --project $A --from mind@$PA.local --to hand@$PB.local --subject T1 --body 'done_when: t1' | id)
  sync A; sync B; sync A
  $VIVI task send --project $B --from mind@$PB.local --to hand@$PA.local --subject BT --body 'done_when: bt' --depends-on $T1
  sync B; sync A
  $VIVI graph ready --project $B          # blocked 7181f0c9 (BT)         <- sender is right
  $VIVI graph ready --project $A          # ready 7181f0c9, ...  blocked (none)
  $VIVI task list --project $A --for hand@$PA.local --blocked   # 7181f0c9 open [761c9428]  <- same item, "blocked"
  $VIVI step --project $A | grep BT       # BT is dispatched
  ```
- Observed: the letter carries `X-Vivi-Depends-On: <T1>`. On A, `task list --blocked` (header-derived) calls BT blocked, while `graph ready` and `step` (edge-derived) call it ready and dispatch it, with its prerequisite still open. A's `work_graph_edges` has no row for it.
- Likely cause: `src/sync/filer.rs` `ingest_row` calls `backlog_attach(kind, &handle, &subject, &[])` with an empty dependency list. Severity: surprises, premature dispatch on the receiving side.

### F5 `need|task|want|mail watch` never fire for letters filed by `sync pull` — BROKEN
- Repro:
  ```
  $VIVI need watch --project $B --for hand@$PB.local --timeout 12s &      # background
  $VIVI need send --project $A --from mind@$PA.local --to hand@$PB.local --subject wake --body 'done_when: w'
  sync A; sync B; sync A
  wait    # error: message error: mailspace watch timed out without a matching event
  $VIVI need list --project $B --for hand@$PB.local     # the need is there (last_event: sync pull)
  ```
  Control: a need sent locally to B's own hand while a watcher runs fires `event=delivered status=needs kind=need handle=7ac05042`. `--since 1h --once` on B also lists only the local `moved` events, never the `delivered` ones from pull. The reply mail from a remote close does not wake `mail watch` or `need watch --handle $N` either.
- Likely cause: `src/mailspace/watch.rs` `event_kind` takes the first word of the event's command; pull records command `sync pull`, so kind is `sync`, which is not in the accepted set (`mail, task, need, want, graph`). Severity: blocks use of any event-driven agent loop on a synced mailspace.

### F6 Closing or reopening the author's own sent copy updates the folder, not the graph — DIVERGES (local, caused by remote-sent items)
- Repro:
  ```
  N=$($VIVI need send --project $A --from mind@$PA.local --to hand@$PB.local --subject N1 --body 'done_when: n' | id)
  $VIVI need done --project $A --for mind@$PA.local $N          # done $N
  $VIVI need list --project $A --for mind@$PA.local --status all   # N1  done  done
  $VIVI graph ready --project $A                                # ready ... $N   (node still open)
  $VIVI step --project $A                                       # dispatches $N with kind "done"
  $VIVI step --project $A --apply $N                            # only this completes the node
  ```
  Reopen: `task reopen --for mind@$PA.local $T` on a remote-sent task moves it into A's own `tasks` queue (`task list --for mind@$PA.local` -> `$T open tasks`), so A now owns an open local task for work that B owns. `want promote` of a sent want does the same into A's `needs`.
- Likely cause: `sync_backlog_node` in `src/mailspace/backlog.rs` completes a node only for moves `tasks|needs|wants -> done`; a move from `sent` does nothing. `move_item` also files reopened/promoted sent copies under the sender's own role. Severity: surprises; `need list` says done, `step` and `graph ready` say open.

### F7 `step` on the author dispatches work it handed away, and work held under a foreign address that cannot be closed — DIVERGES
- Repro:
  ```
  # A sends N (done_when clause) to hand@$PB.local; sync both
  $VIVI step --project $A     # node 38b10219  kind sent  clauses 1  (dispatched)
  $VIVI step --project $B     # same node, kind need  (dispatched)
  # B sends an internal task mind@$PB.local -> hand@$PB.local; sync both
  IT=$($VIVI task send --project $B --from mind@$PB.local --to hand@$PB.local --subject B-internal --body 'done_when: i' | awk '/^created/{print $3}')
  $VIVI step --project $A | grep B-internal                   # dispatched on A
  $VIVI task done --project $A --for hand@$PB.local $IT       # error: external recipient 'hand@..local' is not allowed for local mailspace delivery
  $VIVI task list --project $A --for hand@$PB.local           # same error
  $VIVI board --project $A                                    # shows neither
  ```
- Observed: `board` is per local identity and hides these items, `step`/`graph ready` do not, so a coordinator driving off `step` double-dispatches the item both sides hold and also dispatches another member's private work. The foreign-held node can only be completed with `graph complete`; `task done --for <that address>` and `--for hand` both fail (`message not found`), and `--to <addr>` filtering is the only way to see it.
- Likely cause: pull attaches a backlog node for every row including sent and foreign-held rows (`placement.attach`, `src/sync/filer.rs`); `step` has no ownership filter. Severity: surprises, wasted or conflicting dispatch.

### F8 `sync prune` evicts open outbound work — BROKEN
- Repro (needs ~70 s for the age):
  ```
  $VIVI need send --project $A --from mind@$PA.local --to hand@$PB.local --subject N1 --body 'done_when: n'
  sync A; sync B; sleep 70
  $VIVI sync prune --project $A --older-than 1m        # evicted ... b51af63c sent ... N-prune-open
  $VIVI need show --project $A b51af63c                # message not found
  $VIVI step --project $A                              # item_missing  no message resolves to this node
  $VIVI sync resync --project $A                       # restores it
  ```
- Observed: prune says it keeps "open needs/tasks/wants" but the author's open sent copies (role `sent`) are evicted; node stays open, message gone, `trace`/`show`/`mail list` lose it until `resync`. Recoverable.
- Likely cause: `src/sync/prune.rs` reuses chat prune's policy, `KEEP_ROLES = [needs, tasks, wants]` in `src/chat/prune.rs`; `sent` rows of open work are not protected. Severity: surprises.

### F9 `done`/`reopen`/`promote` do not check the item kind — BROKEN (local, not sync specific)
- Repro (item pulled on B):
  ```
  $VIVI need reopen --project $B --for hand@$PB.local $W2     # W2 is a want: moved into needs, bypassing promote
  $VIVI task reopen --project $B --for hand@$PB.local $N      # a need becomes a task
  $VIVI want promote --project $B --for hand@$PB.local $T     # a task becomes a need
  $VIVI task done --project $B --for hand@$PB.local $W2       # done on a want works
  $VIVI need bind --project $B $N $N                          # bound (a need bound to itself)
  ```
  `want set-priority`/`want mark` do check (`... is not a want`).
- Likely cause: `move_item` picks the target role from the verb only. Severity: surprises.

### F10 Same command, different cross-side effect depending on whether a push happened — DIVERGES
- Repro:
  ```
  W=$($VIVI want send --project $A --from mind@$PA.local --to hand@$PB.local --subject W-pre --body w | id)
  $VIVI want promote --project $A --for mind@$PA.local $W        # before the first sync
  sync A; sync B
  $VIVI want list --project $B --for hand@$PB.local --status all   # no wants
  $VIVI need list --project $B --for hand@$PB.local --status all   # W-pre arrives as a NEED
  ```
  Promote after the push is silent (F1).
- Likely cause: `src/sync/select.rs` `classify` derives the envelope kind from `effective_kind(local_role, ..)` at push time; the letter bytes still say `X-Vivi-Kind: want`. Severity: surprises (timing-dependent, and envelope vs letter disagree).

### F11 A send to an unknown member prefix is accepted silently — BROKEN (cosmetic)
- Repro: `$VIVI need send --project $A --from mind@$PA.local --to hand@deadbeef.local --subject typo --body 'done_when: x'` -> `sent 260b50fc / queued for sync: hand@deadbeef.local`, exit 0; push sends it; every member holds it under that address; nobody acts on it. No check against `vivi sync directory`.
- Related: `--from hand@$PB.local` (remote role) on A errors with `external recipient 'hand@..' is not allowed for local mailspace delivery`, which blames a recipient that was passed as `--from`. Severity: cosmetic/surprises.

### F12 `--reply-to` on need/task/want send and `task from` require an explicit `--to` — BROKEN (cosmetic)
- Repro: `$VIVI need send --project $B --from hand@$PB.local --reply-to $N --subject Re --body 'done_when: r'` -> `error: message error: delivery needs at least one To or Cc recipient` (same for `task send`, `task from`). `mail reply --from hand@$PB.local $N --body ..` defaults to the parent sender and works across mailspaces (`queued for sync: mind@..`).
- Observed vs expected: the Project Sync docs say a reply is addressed to the From with no `--to`; that is true for `mail reply` only. Severity: cosmetic.

### F13 Two checkouts of one mailspace id (copied `.vivi/`) never share lifecycle state — DIVERGES (unsupported topology)
- Repro: `cp -R $A/.vivi $W/a2/.vivi`; B sends a need and a task to `hand@$PA.local`; both A and A2 pull; `task done` on A (no note); `need done --note x` on A2; sync A2 then A.
- Observed: A2 still lists the task open; A still lists the need open (`need list --for hand@$PA.local` -> open). The note letter lands on A as a `sent` row (`1 new (1 sent)`, From is a local role), not in the inbox. The server accepted the copy as the same member (same key).
- Expected per docs: unsupported (do not copy `.vivi`), so reported only for completeness. Severity: cosmetic.

### F14 `absorb` seals an open item that stays dispatchable — DIVERGES (local)
- Repro: `$VIVI task absorb --project $B --for hand@$PB.local $T; $VIVI board --project $B` -> `tasks: 0`; `$VIVI step --project $B` -> `$T` still dispatched; `$VIVI task done --project $B --for hand@$PB.local $T` -> `is absorbed and can no longer be changed`. The sender may absorb its own sent copy too. Nothing travels.
- Likely cause: absorb sets `absorbed_at` only; graph node untouched. Severity: surprises.

### F15 Re-running `done --note` is not idempotent — cosmetic
- `need done --for hand@$PB.local $N --note first` then `--note second` on the already-done need: both exit 0 and push two `Re:` letters (A's inbox shows two). `need reopen` on an open item prints `reopened` and exits 0.

## Things that worked and are worth recording
- Remote-addressed send of need/task/want, pull into the right folder by kind, `queued for sync` line, push then pull ordering, idempotent pull, resync preserving local state (done role, absorbed) and restoring evicted letters byte for byte.
- Role rename: letters addressed to the old name file under the renamed role; replies from the renamed role resolve on the sender (`Re:` from `worker@..` reaches `mind`-renamed-`lead`).
- Unknown remote role: filed under its name with a warning and exit 0, visible after `role add`.
- Multi-recipient (local + remote `--to`, remote `--cc`): one local delivered copy, one pushed letter, remote side files it for the right role.
- `trace` across a synced thread is identical on both sides.
