# Audit: local (memo, role, rule, goal, graph, step, board, boot, cycle)

Binary: `vivi 10.2.0` (target/debug), local vivi-server. World: harness A=alice, B=bob, both `[sync]`
configured, letters pulled both ways, plus a `cp -R` second checkout of A's `.vivi` (call it C) and a
`prune`/`resync` round. Every finding below was re-run in a freshly built world (`audit-setup.sh`,
then `$SCRATCH/audit-local-repro.sh`, output in `$SCRATCH/audit-local-repro.out`).

Shorthand used in the repros (after `source WORKDIR/env.sh`):

```sh
va() { "$VIVI" "$@" --project "$A"; }
vb() { "$VIVI" "$@" --project "$B"; }
BODY=$'done_when: it works\nwrite_scope: src/'
```

## Summary

41 leaves audited: 5 BROKEN, 7 DIVERGES, 2 WORKS, 27 WORKS-LOCAL, 0 UNTESTED.
Class: none of the 41 touches the server; they are all LOCAL-ONLY (read or write the checkout's
`.vivi/` only). The sync-visible damage is in what the backlog graph (`step`, `graph ready`,
`boot`) *does with pulled and sent letters*, not in the commands being local.

Three most important findings:
1. **Every mailspace's backlog holds every member's work items, and `vivi step` offers them for
   dispatch** (F2): B's `step` dispatches A's local task for A's `hand`; the sender's own sent
   copies dispatch as kind `sent` (wants included), and `graph activate` then refuses them (F1).
   Two Minds dispatching one item is the default outcome.
2. **`--depends-on` edges do not survive a pull** (F3): the letter carries `X-Vivi-Depends-On`,
   the receiver's graph has no edge, the item is `ready`, and `graph audit` reports clean.
3. **Closing is local and the sender's `step` never converges** (F4): the sender dispatches a
   closed item forever; the only clearing verb (`graph complete`) is undone by `graph audit --repair`
   for foreign-held items (F2).

Things that work as a user expects: role rename follows the alias on pull (old and new address both
file under the renamed seat), a letter to an unknown role appears once the role is added, `board
--since` sees pulled letters, memos/rules/goals/roles/charters/imported graphs stay local with no
surprises, and `sync push` says so ("not synced by design: N memo, N kind rule, N trash").

## Matrix

| command | class | result | evidence |
| --- | --- | --- | --- |
| board | LOCAL-ONLY | DIVERGES | `vb board` lists 2 open (T1,N1) while `vb boot` totals `open 2 task 1 need` and `vb graph ready` lists A's local task `T3`; `board --graph` frontier includes foreign-held and sent nodes; items filed for an unknown role are absent (F2, F5, F6). `board --since T` does include pulled letters (WORKS). |
| boot | LOCAL-ONLY | DIVERGES | `vb boot`: handles list contains `1675cdc3 task ... T3 alice-local task` (A's local task, not B's); `unabsorbed mail` holds `fb1c32bc alice@audit.local vouch bob` which `va mail absorb fb1c32bc --for mind` rejects: `message not found`. No sync line (unsent, cursor). (F2, F5) |
| memo save | LOCAL-ONLY | WORKS-LOCAL | `va memo save --for mind --subject s --body x` -> `saved c64006e3`; `va sync push` -> `not synced by design: 1 memo`; `vb memo list --for mind` -> `no memos`. Help text of `memo` does not say it is local; `sync push --help` does ("never memos"). |
| memo delete | LOCAL-ONLY | BROKEN | `va memo delete <need-handle> --for mind` -> `deleted e58705b1`; the need is trashed, `va step` still lists it as kind `trash`. No kind check, unlike `memo absorb` (F8). |
| memo list | LOCAL-ONLY | WORKS-LOCAL | `va memo list --for mind` (also via renamed alias) lists the memo; exit 0. |
| memo search | LOCAL-ONLY | WORKS-LOCAL | `va memo search --for scribe note` matches body text. |
| memo show | LOCAL-ONLY | WORKS-LOCAL | Shows any handle (also non-memos: `memo show 77c49095` printed the need); harmless. |
| memo absorb | LOCAL-ONLY | WORKS-LOCAL | `va memo absorb <memo> --for scribe` -> `absorbed`; on a need handle: `77c49095 is need, not memo`, exit 1. |
| memo dump | LOCAL-ONLY | BROKEN | `va memo dump --for worker --json` -> `"body": ""`; text form prints `Subject:` then a blank body. Not sync-specific (same on an unsynced mailspace): `src/mailspace/dump.rs` only reads the blob when a body filter is given. |
| rule save | LOCAL-ONLY | DIVERGES | `va rule save --for mind --subject s --body b` -> `saved`, `rule list` scope=`project`; `va sync push` -> `not synced by design: 1 kind rule`; `vb rule list` -> `no rules`. Help: "project rules shared by every seat"; `--local` "local to this mailspace" implies the default is shared (F9). |
| rule list | LOCAL-ONLY | WORKS-LOCAL | `--for <old name>` keeps rules written under pre-rename names. |
| rule show | LOCAL-ONLY | WORKS-LOCAL | Pulled letter handle -> `c3029f9e is not a rule`, exit 1 (a pulled letter can never become a rule: pull files only inbox/needs/tasks/wants). |
| rule search | LOCAL-ONLY | WORKS-LOCAL | Matches subject and body, active set by default. |
| rule supersede | LOCAL-ONLY | WORKS-LOCAL | `superseded 4f719ddf with 7a7c19d4`; superseding a retired rule -> `1cdc653b is retired`, exit 1. Same non-sync scope caveat as `rule save`. |
| rule retire | LOCAL-ONLY | WORKS-LOCAL | `retired` then `already retired` (no second event). |
| rule absorb | LOCAL-ONLY | WORKS-LOCAL | On a pulled letter: `is not a rule`, exit 1. |
| goal add | LOCAL-ONLY | WORKS-LOCAL | Stores a project-relative path in `mail.sqlite`; outside-root, symlink-out, dir, missing file all refused. `--for hand@<other>.local` and `--for nobody` are accepted as free-form metadata. |
| goal list | LOCAL-ONLY | WORKS-LOCAL | Registry is per checkout: `vb goal list` -> `no goals`; a `cp -R` copy at another root shows the same row as `MISSING`. |
| goal show | LOCAL-ONLY | WORKS-LOCAL | By handle, prefix or path. |
| goal drop | LOCAL-ONLY | WORKS-LOCAL | Leaves the file. |
| role list | LOCAL-ONLY | WORKS-LOCAL | Roster lives in `.vivi/mailspace.toml`, never travels; `formerly:` aliases shown. Peers cannot list it. |
| role show | LOCAL-ONLY | WORKS-LOCAL | Alias resolves (`role show hand` -> `worker ... formerly: hand`); remote address -> `external recipient ... not allowed`, exit 1. |
| role add | LOCAL-ONLY | WORKS | Pulled letters filed under an unknown name appear in the new role's queue once added (`va role add ghost` -> `task list --for ghost` shows R3). Adding an alias name: `local role 'hand' already exists`. |
| role set | LOCAL-ONLY | WORKS-LOCAL | `--status retired` is not consulted by pull: letters are still filed for the retired seat, no warning (F6). |
| role rename | LOCAL-ONLY | WORKS | `va role rename hand worker`; B sends to `hand@$PA.local` and `worker@$PA.local`; both file under `worker`, no warning. Chains (`hand, worker, courier`) resolve. See F6 for the cosmetic edges. |
| role status | LOCAL-ONLY | WORKS-LOCAL | Alias resolves; pid/host binding is checkout-local. |
| role charter show | LOCAL-ONLY | WORKS-LOCAL | Charter file follows a rename (`charters/`). |
| role charter set | LOCAL-ONLY | WORKS-LOCAL | Never travels. |
| cycle intake | LOCAL-ONLY | BROKEN | Counts are right with pulled letters (events `sync pull`/`delivered`). `vb cycle intake --for mind --json` -> `unabsorbed_mail[].body == ""` for a letter whose body is `hello bob` (same dump bug as memo dump). Cosmetic. |
| graph import | LOCAL-ONLY | WORKS-LOCAL | Imported topologies are local, never pushed. A source id equal to a pulled handle is allowed (bare id then addresses the backlog; use `graph:id`). |
| graph apply | LOCAL-ONLY | WORKS-LOCAL | Refuses removing a done node (`cannot remove node 'a26d7fd6' in state 'done'`). |
| graph show | LOCAL-ONLY | DIVERGES | `graph show backlog` differs between A and B for the same letters: B has `e58705b1 --> 690b4c53`, A has no edge (F3). Imported graphs fine. |
| graph export | LOCAL-ONLY | DIVERGES | Same as `graph show` (`--include-state` shows `690b4c53` blocked on B, ready on A). |
| graph ready | LOCAL-ONLY | DIVERGES | A: `ready=5 blocked=0` incl. dependent T5; B: `ready=3 blocked=1`. Also lists sent copies and foreign-held items as ready (F1, F2, F3). |
| graph audit | LOCAL-ONLY | DIVERGES | Reports `findings missing=0 state=0 kind=0 orphan=0` while T5 has lost its edge (F3); after `sync prune` reports `orphan_node ... the item was deleted or trashed` for evicted letters (F7). |
| graph connect | LOCAL-ONLY | WORKS-LOCAL | Edge is local (not on the other side). Accepts sent copies and foreign-held items that `--depends-on` refuses (F3b). |
| graph complete | LOCAL-ONLY | WORKS-LOCAL | Works on sent copies and foreign-held nodes; for foreign-held items `graph audit --repair` re-opens it (F2). |
| graph activate | LOCAL-ONLY | BROKEN | B: `vb graph activate 1675cdc3 --task 1675cdc3` (A's local task) succeeds; then `vb task done 1675cdc3 --for hand` -> `message not found`, exit 1. A: on its own sent copy -> `activate requires a task handle; got role 'sent'`, exit 1 (F1, F2). |
| graph node add | LOCAL-ONLY | WORKS-LOCAL | Imported graphs only. |
| graph edge add | LOCAL-ONLY | WORKS-LOCAL | Imported graphs only. |
| step | LOCAL-ONLY | BROKEN | A, before any sync: manifest dispatches `3fc3bc3b sent`, `8ba96ad0 sent` (a want), `e58705b1 sent`. B: dispatches A's local task. After B closes T1, A still dispatches it (F1, F2, F4). `step --apply` on a sent copy: `not_settled`; on an evicted/unknown handle: exit 1. |

## Findings

### F1 The sender's own sent copy of remote-addressed work is dispatched, as kind `sent`, wants included — BROKEN
- Repro (fresh world, no sync needed):
  ```sh
  va need send --from mind --to hand@$PB.local --subject N1 --body "$BODY"     # prints: sent <N1>, queued for sync
  va want send --from mind --to hand@$PB.local --subject W1 --body "$BODY"     # prints: sent <W1>
  va task send --from mind --to hand@$PB.local --subject T1 --body "$BODY"     # prints: sent <T1>
  va step                    # all three under dispatches, kind column = sent, no exceptions
  va graph activate <T1> --task <T1>   # error: activate requires a task handle; got role 'sent' (exit 1)
  ```
- Observed: `nod_... 3fc3bc3b sent 1 yes T1 task to bob`, `... 8ba96ad0 sent ... W1 want to bob`. The want
  is not parked (on B the same want is `want_requires_promotion`). Expected: sent copies of work
  addressed elsewhere are not this mailspace's dispatchable work; a want is parked everywhere.
- Cause: `src/mailspace/delivery.rs::attach_sent_work` mints the node on the sent copy when no local
  recipient exists; `src/mailspace/step.rs::adjudicate_node` derives kind from the row's folder role
  (`kind_for_role(local_role)`), not from the node kind, so `sent` bypasses the `want` and kind checks.
  `graph ready` and `step --json` carry the same rows. Severity: blocks use (a Mind loop over `step`
  dispatches work it cannot activate and that belongs to the other side).

### F2 Work held for another mailspace's role is in this mailspace's backlog, dispatched by `step`, counted by `boot` — BROKEN
- Repro:
  ```sh
  T3=$(va task send --from mind --to hand --subject T3 --body "$BODY" | sed -n 's/^created hand //p')  # A-local task
  va sync; vb sync
  vb step                   # dispatches $T3: "T3 alice-local task", kind task
  vb board                  # does not list it
  vb boot                   # totals "open 2 task", handles list includes $T3
  vb graph activate $T3 --task $T3    # succeeds, node active
  vb task done $T3 --for hand         # error: message not found: $T3 (exit 1)
  vb graph complete $T3 && vb graph audit --repair   # audit: node_state ... [repaired]; $T3 is back in `vb step`
  ```
- Observed: B's Mind is offered, and can activate, a task for A's `hand`. B cannot close it with any
  lifecycle verb (`--for hand` -> not found; `--for hand@<A>.local` -> `external recipient ... not allowed`),
  `graph complete` clears it but `graph audit --repair` ("folder wins") re-opens it. Expected: per the
  skill, work for another mailspace is "held under its address ... without it landing in a local queue".
- Cause: `src/sync/filer.rs::placements` fallback files a non-local letter under its raw To address
  with `attach: true`, and `ingest_row` calls `backlog_attach` for every work kind; `step`, `graph
  ready`, `boot` handles and `graph audit` do not filter nodes by "does this mailspace hold the item".
  Both mailspaces therefore dispatch every item. Severity: blocks use (duplicate dispatch, stuck nodes).

### F3 Dependency edges are not rebuilt on pull; `graph audit` is blind to it — DIVERGES
- Repro:
  ```sh
  va need send --from mind --to hand@$PB.local --subject N1 --body "$BODY"; va sync; vb sync   # N1 pulled by B
  vb task send --from hand --to mind@$PA.local --subject T5 --body "$BODY" --depends-on <N1>
  vb graph ready                 # blocked: T5          (edge N1 -> T5)
  vb sync; va sync
  va graph ready                 # blocked: (none)      T5 is ready
  va graph audit                 # findings missing=0 state=0 kind=0 orphan=0
  ```
  The pulled letter holds `X-Vivi-Depends-On: <N1>` (grep it in `A/.vivi/blobs`).
- Same on a local-to-B task (`T6`, depends on N1): ready on A. `graph audit --repair` does not mint the
  edge; `sync pull --refs` does not either. Expected: the receiver's graph mirrors the letter's
  dependency header, or the audit reports the drift.
- Cause: `src/sync/filer.rs::ingest_row` calls `backlog_attach(kind, &handle, &subject, &[])` with an
  empty dependency list; `backlog_audit.rs` only checks node existence, kind and state. Severity: blocks use.
- F3b (same area): `--depends-on` cannot cite a sent copy of remote-addressed work:
  `va task send ... --depends-on <T1>` -> `dependency '<T1>' has role 'sent'; expected a task, need, or
  want handle` (exit 1), so a sender cannot order two letters it sent to another mailspace, while
  `graph connect <b> <a>` accepts the same handles (a local-only edge, never visible to the receiver).

### F4 The sender's graph never learns that the other side closed an item — DIVERGES
- Repro:
  ```sh
  # T1 sent by A to B's hand, pulled by B
  vb task done <T1> --for hand --verdict clean_pass --repo r --tip abc --note shipped
  vb sync; va sync                # A gets "Re: T1" as plain inbox mail
  va step | grep <T1>             # still a dispatch, kind sent
  va step --apply <T1>            # exception not_settled: settle the item ... before apply
  ```
- Observed: closing is local (as the brief expects), the receipt is a plain `X-Vivi-Kind: mail` letter
  with no machine-readable close header, so the sender's node stays `ready` forever and `A step`
  re-offers it every loop. The only clear is `va graph complete <T1>`. The same happens for B's
  `sent` N2 after A closes it. Same story between two checkouts of one mailspace id: `va task done
  $T3` on A, `sync` both, and C's `step` still dispatches `$T3` (the receipt arrives in C's inbox).
  Severity: surprises (a Mind loop re-dispatches closed work).

### F5 `boot` counts and lists the other mailspace's work, and shows a governance letter nobody can absorb — DIVERGES
- Repro: after the F2 repro, `vb boot`; compare `vb board`. And `vb boot | sed -n '/unabsorbed/,/^$/p'`,
  then `vb mail absorb <vouch handle> --for mind` (exit 1, `message not found`) and `--for postmaster`
  (`unknown local role 'postmaster'`).
- Observed: boot `totals open N task` and the handles list include B-held and A-held items the board
  does not (board `tasks: 1`, boot `open 2 task`). The `vouch bob` letter, filed under account
  `postmaster@audit.local`, sits in `unabsorbed mail` in every checkout forever (no role can absorb it).
  Boot, board and `mailspace status` print nothing about sync (unsent letters, last pull): a queued
  letter is visible only in `vivi sync status`.
- Cause: boot's handle inventory reads the whole backlog rather than the roster's accounts (see F2);
  the unabsorbed query includes rows whose account is not a local role. Severity: surprises.

### F6 Letters for an unknown or retired role, and rename edges — DIVERGES (cosmetic)
- Repro:
  ```sh
  vb task send --from mind --to ghost@$PA.local --subject R3 --body "$BODY"; vb sync
  va sync pull          # stdout: "sync pull: 1 fetched, 1 new (1 tasks) ..."  stderr: "warning: filed for unknown role ghost"  exit 0
  va board              # R3 absent;  va task list --for ghost -> unknown local role 'ghost' (exit 1)
  va step | grep R3     # present;  va boot lists it too
  ```
- Observed: the warning is stderr-only and the summary line does not count it; the sender got no signal
  (it only saw `queued for sync: ghost@...`, and `sync directory` lists members, not roles), so a typo
  in a role name is a silent dead letter that still shows in `step` and `boot`. A `retired` role
  keeps receiving and keeps its seat on the board with no warning. `role rename mind scribe` silently
  adopts earlier rows filed raw under `scribe` (they join the renamed seat's queue). Rename itself is
  correct: old and new addresses file under the canonical role (`task list --for hand` works as alias).
  Severity: surprises. There is no `role remove`/`delete` (`unrecognized subcommand`).

### F7 `sync prune` evicts the sent copies of open remote-addressed work and leaves orphan nodes — DIVERGES
- Repro: after the F1 repro and a sync, `va sync prune --older-than 1s`; `va graph audit`; `va step`;
  `va sync resync`.
- Observed: `evicted ... 3fc3bc3b sent T1 ... 8ba96ad0 sent W1` although help says "never open work";
  then `graph audit`: `orphan_node task 3fc3bc3b ... (no message resolves to this node; the item was
  deleted or trashed)`; `step`: `item_missing`; `graph audit --repair` repairs nothing (`orphan=3`
  unchanged); `graph ready` still lists them; unabsorbed inbox mail also disappears from `boot`.
  `sync resync` restores everything (audit `orphan=0`). Expected: sent copies of open work are kept,
  or the audit says "evicted, restore with resync". Severity: surprises.

### F8 `memo delete` deletes any record the identity holds; trashed work still dispatches — BROKEN
- Repro:
  ```sh
  N1=$(va need send --from mind --to hand@$PB.local --subject N1 --body "$BODY" | sed -n 's/^sent //p')
  va memo delete $N1 --for mind     # deleted <N1>   (memo absorb on it says "is need, not memo")
  va step | grep $N1                # still a dispatch, kind trash
  va task send --from mind --to hand@$PB.local --subject Q --body "$BODY"   # queued
  va memo delete <Q> --for mind; va sync push   # "not synced by design: 1 trash"; B never gets Q
  ```
- Cause: `src/local_mailspace_command.rs` `MemoCommand::Delete` calls `move_item(..., "trash", ...)`
  with no memo check; the node is not touched. Not sync-specific, but it silently drops queued
  outbound letters and there is no other delete verb. Severity: surprises.

### F9 Rules and memos do not travel; help text does not say so — DIVERGES (cosmetic)
- `va rule save` default scope is `project`; `va sync push` -> `not synced by design: ... kind rule`;
  `vb rule list` -> `no rules`; a `cp -R` second checkout of A shows the rule only because it copied
  the sqlite. `rule --help`/`memo --help` do not mention sync; `sync push --help` lists only "memos,
  drafts, or trash" (rules are the unnamed `kind rule`); the skill states "Nothing syncs rules yet".
  There is no rule evaluation engine: rules are never applied to letters, pulled or local.
  Severity: cosmetic.

## Second checkout of one mailspace id (copy of `.vivi`, same key)
Node ids and handles are deterministic across checkouts (`gph_3b1d8b812e8948cc`, same `nod_...`),
letters pull identically, but all local state diverges silently: roles/aliases (A renamed `worker`->
`courier`, C still files the same letter under `worker`), closes (F4), rules, memos, goals, graph
connects/activations, cycle cursors (event ids are per checkout). Nothing reconciles them; this is
the documented "local state" boundary, noted here because `step` makes each checkout dispatch alone.

## Not exercised / out of scope
- `sync join` with a second key under an already-claimed prefix (sync internals).
- need/task/want lifecycle, mail send/reply, chat, pool.
