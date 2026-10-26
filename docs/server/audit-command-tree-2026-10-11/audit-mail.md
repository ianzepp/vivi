# Audit: mail (mail subtree + mailspace subtree)

Binary: `vivi 10.2.0` (debug build), private server `127.0.0.1:18142`, harness world
`SCRATCH/audit-mail` (A = alice `f3b1eba8`, B = bob `bf1b6a7e`). All repros below use the
harness env (`source SCRATCH/audit-mail/env.sh` gives `$VIVI $A $B $PA $PB $W`).

## Summary
18 commands: 4 WORKS, 7 WORKS-LOCAL, 2 DIVERGES, 5 BROKEN, 0 UNTESTED.
Most important findings:
1. `mail watch` and `mailspace watch` never wake for anything that arrives through `vivi sync` (F1).
2. `mail deliver` rejects remote recipients (known issue, confirmed), and the inverse (a From that names another mailspace) is pushed and then refused by every puller; `mailspace import` of a foreign mailspace does the same (F2, F3).
3. `mailspace import` between two checkouts of one project dies with a raw SQLite UNIQUE error, leaves an orphan row, and its dry run says it would work (F4). Also: oversized letters are "queued for sync" and silently never sent (F5).
The `.vivi/mailspace.id` sidecar guard works as designed (clear message, nothing changed, every command refuses) - see "Guard verification".

## Matrix
| command | class | result | evidence |
| --- | --- | --- | --- |
| mail send | SYNCED-WRITE | DIVERGES | Works for local (`--to hand`), remote (`--to hand@$PB.local`, `--cc` mixed local+remote, non-ASCII subject/body, `--body @file`): `sent a67aa51c / queued for sync: hand@bf1b6a7e.local`, B pulls it byte for byte. Unknown local role is refused (`unknown local role 'ghost'`, exit 1). But: a >64 KiB letter prints `queued for sync` and is never sent (F5); a prefix nobody holds (`--to hand@deadbeef.local`) or an unsynced mailspace also says `queued for sync` (F7); every local-to-local letter is uploaded too (F10). |
| mail reply | SYNCED-WRITE | WORKS | `mail reply --project $B --from hand a67aa51c --body ...` -> `sent 34217e07 / queued for sync: mind@f3b1eba8.local`; after sync A's `mail thread` shows the pair with `[mail captured]` link. Also works replying to own sent copy (To defaults to the original's To), to a pulled need, with `--to hand@$PB.local --cc mind@$PB.local --subject`, and a second reply. Unknown handle: `message not found: zzzzzzzz` exit 1. Reply to a governance letter From `alice@audit.local`: `reply parent has no other local identity or remote address` exit 1 (sane). |
| mail list | SYNCED-READ | WORKS | Shows locally sent and pulled letters on both sides; `--from hand@$PB.local`, `--from mind@a.local` (legacy domain), `--from mind@local`, `--to ... --folder sent`, `--status absorbed`, `--json` all consistent. Notes: `--for hand@$PB.local` -> `external recipient ... not allowed for local mailspace delivery` exit 1; `--folder all` -> `no messages in all` exit 0 (F11). |
| mail show | SYNCED-READ | WORKS | `mail show --project $B a67aa51c` prints the pulled letter; multiple handles ok; non-ASCII ok. Text view omits Cc and attachments (JSON has `cc`) (F11). |
| mail thread | SYNCED-READ | BROKEN | Only `--limit`: `mail thread --project $A 34217e07 --limit 2` -> `Thread (5 message(s))` with 5 shown (F9). Without it threads cross the sync boundary correctly (`1 reply link(s)`, parent_content_id set, same on both sides). |
| mail deliver | SYNCED-WRITE | BROKEN | Known issue confirmed: `mail deliver --project $A r.eml` (To: hand@$PB.local) -> `error: message error: external recipient 'hand@bf1b6a7e.local' is not allowed for local mailspace delivery`, exit 1; same for mixed local+remote and `--folder sent` (F2). Foreign From is accepted then refused on pull (F3). Re-delivering bytes already pulled makes a duplicate unread row (F2c). |
| mail absorb | LOCAL-ONLY | WORKS-LOCAL | `mail absorb --project $B --for hand a67aa51c --note "read it"` -> `absorbed a67aa51c`; idempotent; wrong role -> `held by identity 'hand'; retry with --for hand` exit 1; survives a later sync; the sender's side is untouched (by design). `mail show` never changes read state; "unread" = unabsorbed. |
| mail watch | SYNCED-READ | BROKEN | Live: `mail watch --project $B --for hand --since $NOW --timeout 14s` while A sends and both sync -> `error: message error: mailspace watch timed out without a matching event`, exit 1, though `mail list --for hand` shows the new letter (F1). |
| mail dump | SYNCED-READ | WORKS | `mail dump --project $A --from hand@$PB.local` -> 3 pulled letters; `--participant`, `--to ... --folder sent`, `--since 1h`, `--output`, `--json` fine; large-output guard works (`refusing large stdout dump`, exit 1; `--confirm-large` ok). `--for hand@$PB.local` -> external-recipient error (F11). |
| mailspace init | LOCAL-ONLY | WORKS-LOCAL | `mailspace init --project $W/c` mints id + sidecar + `.vivi/.gitignore`; second run is a no-op; guard honoured. Quirk: `--project $W/nonexistent` silently creates the directory (F11). |
| mailspace status | LOCAL-ONLY | DIVERGES | Counts only known roles: a pulled letter for unknown role `ghost` is in the store but `total unread mail` ignores it and `mail list --for ghost` errors (F6). No sync information (unsent, cursor). `--json` field `found` is always true. |
| mailspace description | LOCAL-ONLY | WORKS-LOCAL | `--set` persists (multi-line, non-ASCII ok), id line preserved; does not travel (`B` still `(none)` after sync both ways). `--set ""` then show prints a blank line, not `(none)`. |
| mailspace archive export | LOCAL-ONLY | WORKS-LOCAL | `archive --set $W/archive-b` (git repo; non-git refused), absorb two letters, `archive export` -> `scanned 3  written 1  unchanged 2`, then `--json` `unchanged 3`; pulled letters archived with their `sync pull` event and absorb note. `archive --clear` works. No sync interplay. |
| mailspace watch | SYNCED-READ | BROKEN | Same cause as mail watch (F1): `mailspace watch --project $B --for hand --once --since 2026-01-01 --kinds mail,task,need,want` prints nothing though hand has 6 pulled letters and 1 pulled need; a locally sent need shows up; `--kinds sync` -> `unsupported kind 'sync'`. |
| mailspace import | LOCAL-ONLY (feeds sync) | BROKEN | `cp -R $A $W/a4; mailspace import --project $W/a4 --from $B --dry-run` -> `imported=3 deduped=19`, exit 0; real run -> `error: failed to upsert remote binding: UNIQUE constraint failed: remote_bindings.account, remote_bindings.remote_mailbox, remote_bindings.remote_uidvalidity, remote_bindings.remote_uid`, exit 1, store left with +1 row (F4). Importing a foreign mailspace into a synced one makes poison letters (F3). Import copies no roles (`mail list --for hand` -> unknown role on the target). |
| mailspace identity add | LOCAL-ONLY | WORKS-LOCAL | `identity add scribe` -> `added scribe@f3b1eba8.local`; duplicate, alias name, bad name (`bad name!`, `x@y.local`) refused, exit 1; `Scribe2` is lowercased. Not visible to the other side (directory lists members, not roles). |
| mailspace identity list | LOCAL-ONLY | WORKS-LOCAL | Lists role + address + `formerly:` aliases. After renaming `builder` back to `hand` it prints `formerly: hand, builder` (the current name is its own alias; cosmetic). |
| mailspace identity rename | LOCAL-ONLY | WORKS-LOCAL | `rename hand builder` -> `historical mail sent as hand still resolves under builder`; B (unaware) keeps sending to `hand@$PA.local` and A files it under `builder`; `--from hand`, `--from hand@$PA.local`, `list --for hand` all resolve. Collisions (`mind`, existing alias, unknown, same name, address form) refused with exit 1. |

## Guard verification (sidecar `.vivi/mailspace.id`)
Result: WORKS. Not a finding, recorded because the brief asked for it.
- Config id edited to `11111111-...` while sidecar keeps `84d67fe2-...`: every one of 28 commands tried (`mailspace status|init|description|archive|watch|import|identity add|list`, `mail send|list|show|thread|absorb|reply|deliver|watch|dump`, `sync`, `sync status`, `role list`, `need list`, `board`, `chat status`, `goal list`, `rule list`, `step`, `trace`) refuses with exit 1 and the same message: `mailspace id conflict: <toml> has id 1111... but <.vivi/mailspace.id> has 84d6.... Keep the id the letters you already sent were minted under (their From address starts with its first 8 characters): to keep the config's id, delete <sidecar>; to keep the sidecar's, set the id line in <toml> to <sidecar id>. Nothing was changed.` Both files and `mail.sqlite` were byte/mtime-identical afterwards (sha256 compared).
- Config lost its id line: next open restores it from the sidecar and prints `vivi: ... had lost its mailspace id; restored <id> from ...mailspace.id`. Sidecar deleted: re-derived silently. Malformed sidecar / config id / uppercase UUID: clear `not a canonical lowercase hyphenated UUID` error, exit 1.
- Second checkout adopting A's id (copy of `.vivi/mailspace.toml`, or `git clone` of a repo that tracks it): sidecar is backfilled, same addresses minted, a new checkout key generated. `sync` -> `project server refused the bearer mint for 'alice'`; after a vouch under a new name, `sync join` -> `another member of project ... already holds the address prefix f3b1eba8 ...` (clear, but see F8). A tracked config prints `warning: .vivi/mailspace.toml is tracked by git, so every clone would share this mailspace's id; run: git rm --cached -r .vivi`.
- Both copies lost (config id removed and sidecar deleted): a NEW id is minted silently (by design); the checkout then sends under a new prefix its server identity is not registered for -> F3.
- The guard is not consulted when a checkout is only the SOURCE of `mailspace import` (source with conflicting id imported fine, exit 0; cosmetic).

## Findings
### F1 `mail watch` / `mailspace watch` are blind to every letter that arrives via sync - BROKEN
- Repro (live):
  ```
  NOW=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  "$VIVI" mail watch --project "$B" --for hand --since $NOW --timeout 14s --until-count 1 &
  "$VIVI" mail send --project "$A" --from mind --to hand@$PB.local --subject probe --body x
  "$VIVI" sync --project "$A"; "$VIVI" sync --project "$B"
  wait                                   # error: message error: mailspace watch timed out without a matching event ; exit 1
  "$VIVI" mail list --project "$B" --for hand | grep probe   # the letter is there
  ```
  Same for a pulled need: `"$VIVI" need send --project "$A" --from mind --to hand@$PB.local --subject n --body x`, sync both, then `"$VIVI" mailspace watch --project "$B" --for hand --once --since 2026-01-01 --kinds mail,task,need,want` prints nothing while `"$VIVI" need list --project "$B" --for hand` shows it. A local send on B wakes the watcher (control run).
- Observed vs expected: a watcher is the wake-up mechanism for agents; it must fire when a remote letter is filed. It never does, and there is no kind that selects them (`--kinds sync` -> `unsupported kind 'sync'; expected mail, task, need, want, graph`).
- Likely cause: `src/mailspace/watch.rs` `event_kind()` takes the first word of `event.command`; pull records the event with command `sync pull`, so kind = `sync`, which `matches_event()` drops against the default `mail,task,need`. Severity: blocks use (anything driven by a watch loop never sees remote work).

### F2 `mail deliver` rejects remote recipients; no CLI path puts an attachment on a remote letter - BROKEN
- Repro: `printf 'From: mind@%s.local\nTo: hand@%s.local\nSubject: r\nMessage-ID: <r@audit.local>\n\nbody\n' $PA $PB > r.eml; "$VIVI" mail deliver --project "$A" r.eml`
- Observed: `error: message error: external recipient 'hand@bf1b6a7e.local' is not allowed for local mailspace delivery`, exit 1. Identical for a mixed `To: hand@$PA.local, hand@$PB.local` letter and with `--folder sent`. Expected (known issue): either accept and queue like `mail send`, or say that remote recipients need `mail send`.
- Consequence: `mail send` has no attachment option and `deliver` is the only way to inject a raw .eml, so an attachment can never be sent to a remote address. (An eml with an attachment delivered to a local role is stored byte for byte - blob sha256 equals the file's - but `mail show` shows only the body.)
- (c) Re-delivering bytes already pulled creates a second, unread row: `"$VIVI" mail deliver --project "$B" $B/.vivi/blobs/ea/e8/eae852ba...eml` -> `delivered hand dc47d481` next to the pulled `a67aa51c`; it is queued as unsent and pushed again (`sync push: 1 sent, 0 already on server`), and `mail show eae852ba` is `ambiguous content_id prefix`. Delivering the same eml twice into the same mailspace is idempotent (`c3a5a8c8` both times); only the pulled-row case duplicates.
- Likely cause: `Mailspace::deliver_raw` (`src/mailspace/delivery.rs:201-220`) resolves every To/Cc with `resolve_identity` (`src/mailspace/identity.rs:261`, the source of the error text), not with `classify_recipient` (`src/mailspace/address.rs`) that `send`/`reply` use. The same `resolve_identity` call is why `--from <remote>` on send, `list --for`, and `dump --for` print the "recipient" error (F11). Severity: surprises.

### F3 Letters whose From names another mailspace are pushed, accepted, then refused by every puller - BROKEN
- Repro A (deliver):
  ```
  printf 'From: hand@%s.local\nTo: mind@%s.local\nSubject: forged\nMessage-ID: <f@audit.local>\n\nx\n' $PB $PA > f.eml
  "$VIVI" mail deliver --project "$A" f.eml        # delivered mind 2850ba66
  "$VIVI" sync --project "$A"
  ```
  -> `sync push: 2 sent`, `sync pull: ... 1 refused: msg_2850ba66...`, `refused msg_...: From address claims mailspace bf1b6a7e but the sender is registered as f3b1eba8`, `error: message error: 1 record(s) refused (...)`, exit 1. The next sync is clean (cursor moved on).
- Repro B (import): create mailspace C with a role and a letter, `"$VIVI" mailspace import --project "$A" --from "$C"`, `sync --project "$A"` -> the imported letter (From `mind@c3401456.local`) shows in `mail list --for hand`, `sync status` says `unsent: 1`, sync pushes it ("1 sent") and the pull refuses it, exit 1.
- Repro C (id re-minted after both copies lost): `cp -R $A $W/a2`, delete the `id =` line and `.vivi/mailspace.id` in `$W/a2/.vivi`, `mail send --project $W/a2 --from mind --to hand@$PB.local ...`, `sync` in a2 and in B -> both: `refused msg_...: From address claims mailspace 917527e1 but the sender is registered as f3b1eba8`, exit 1; the recipient never gets the letter.
- Observed vs expected: the sender sees `sent`/`1 sent`, the server stores it, every other member gets one failing `sync` and loses the letter. Expected: refuse at push (or at deliver/import) with the reason.
- Likely cause: `src/sync/select.rs` `select()`/`classify()` have no From-prefix check (any unbound row but memo/draft/trash goes); the check exists only on pull (`src/sync/filer.rs:372`), and the server only stamps the sender prefix. Severity: surprises (one failed sync per member, silent loss of the letter).

### F4 `mailspace import` between checkouts of one project fails with a raw SQLite error, is not atomic, and the dry run is wrong - BROKEN
- Repro (after the A<->B exchange in this world, which includes mind->mind and hand->hand letters):
  ```
  cp -R "$A" "$W/a4"
  "$VIVI" mailspace import --project "$W/a4" --from "$B" --dry-run   # imported=3 deduped=19, exit 0
  "$VIVI" mailspace import --project "$W/a4" --from "$B"             # exit 1
  sqlite3 "$W/a4/.vivi/mail.sqlite" 'select count(*) from messages' # 50; A has 49
  ```
  Real run: `error: failed to upsert remote binding: UNIQUE constraint failed: remote_bindings.account, remote_bindings.remote_mailbox, remote_bindings.remote_uidvalidity, remote_bindings.remote_uid`. Re-running fails identically. One B row (`msg_a1048ee1...`, B's sent copy of a local need) stays in the target's `mind` sent folder.
- Observed vs expected: the import should merge or report a conflict per record; it should be all-or-nothing; `--dry-run` must predict the failure. Importing B into an unsynced fresh mailspace works (`imported=20`), so only mailspaces that both hold bindings to the same project collide.
- Trigger found in data: the same server item (uid 18) is bound under account `mind` in A (A's sent row) and under account `mind` in B (B's Cc copy); the binding key is (account, mailbox, uidvalidity, uid), and role names are not unique across mailspaces. Any pair of same-named roles that exchange a letter collides.
- Likely cause: `remote_bindings` UNIQUE key does not include the mailspace; the binding upsert (`src/storage/ingest.rs:408`) runs after message rows are inserted by the import in `src/mailspace/merge.rs`, and the whole import is not one transaction (a row survives the error). Severity: blocks use (consolidating or restoring across checkouts of one project is impossible); the leftover row is minor.
- Related, not a defect of import itself: import copies no roles, so `mail list --for hand` on the target says `unknown local role 'hand'` until `role add`; merged accounts are by role name (B's `hand` letters land in A's `hand`).

### F5 Oversized letters: `mail send` says "queued for sync", sync skips them forever with exit 0 - DIVERGES
- Repro: `python3 -c "print('x'*70000)" > big.txt; "$VIVI" mail send --project "$A" --from mind --to hand@$PB.local --subject big --body-file big.txt; "$VIVI" sync status --project "$A"; "$VIVI" sync --project "$A"` (run twice).
- Observed: `sent e152b604 / queued for sync: hand@bf1b6a7e.local`, exit 0; `sync unsent: 0 letter(s)`; every sync prints `skipped e152b604: 73088 bytes, over the 64 KiB cap`, exit 0; B never gets it.
- Expected: refuse at send time, or `sync status` must count it as unsent/skipped so the problem is visible without reading sync's push summary.
- Likely cause: cap checked only in `src/sync/select.rs` (`MAX_LETTER_BYTES`); `mail send` does not know it. Severity: surprises.

### F6 A letter pulled for an unknown role (or a non-role address) is filed but invisible - DIVERGES
- Repro: `"$VIVI" mail send --project "$A" --from mind --to ghost@$PB.local --subject s --body x` (accepted, queued); sync both. B prints `warning: filed for unknown role ghost`, exit 0. Then on B: `mail list --for ghost` -> `error: message error: unknown local role 'ghost'`; `mailspace status` totals do not include it (`total unread mail` unchanged); `mail show dc96a6d0` and `mail list --to ghost` do find it. After `role add ghost --kind hand` it appears in `--for ghost` and status.
- Same family: governance mail filed under account `postmaster@audit.local` cannot be listed with `--for` (`external recipient 'postmaster@audit.local' is not allowed ...`); a pulled need whose To is a foreign-prefix address (A pulled B's local need) is filed under account `hand@bf1b6a7e.local` and absent from `need list --for hand` / status.
- Observed vs expected: the receiver's status says nothing is waiting while a letter sits in the store; only a one-line sync warning exists, and the sender is never told. Expected: count such letters in `mailspace status` (an "unknown roles" line) and let `mail list --for` list any account that holds mail.
- Severity: surprises.

### F7 "queued for sync" is not a delivery promise (typo'd prefix, no [sync] table) - DIVERGES
- Repro: `"$VIVI" mail send --project "$A" --from mind --to hand@deadbeef.local --subject s --body x` -> `queued for sync: hand@deadbeef.local`; sync -> `1 sent`; nobody ever receives it. In an unsynced mailspace (`$W/c`): `mail send ... --to hand@$PB.local` -> `queued for sync: hand@bf1b6a7e.local`, while `sync` says `no [sync] table in mailspace.toml`.
- Expected: warn when the prefix is not in `sync directory` / when no [sync] is configured (a local send cannot know the first, but it can know the second).
- Severity: surprises.

### F8 The server's "remove the id line" remedy is defeated by the sidecar guard - BROKEN (misleading message)
- Repro: second checkout with A's `mailspace.toml` (so same id), vouched under a new name, `"$VIVI" sync join --project "$D"` -> `another member of project ... already holds the address prefix f3b1eba8 ...; if nothing was sent from this mailspace yet, remove the id line from .vivi/mailspace.toml so a new one is minted, then run vivi sync join again`. Removing only the id line: next command prints `had lost its mailspace id; restored f3b1eba8-... from .../mailspace.id` and the join fails with the same message again (loop). It succeeds only after also deleting `.vivi/mailspace.id` (`joined project ... as alice2`, new id `efc5bf32`).
- Also: letters already in the store keep the old prefix; the unsent one (`from D`, From `mind@f3b1eba8.local`) was then pushed and refused by every puller (F3).
- Fix target: the join error text (and/or the guard's remedy) must name both files. Severity: surprises.

### F9 `mail thread --limit N` does not limit - BROKEN (cosmetic)
- Repro: `"$VIVI" mail thread --project "$A" 34217e07 --limit 2` -> `Thread (5 message(s))` with 5 printed (JSON also 5). `--limit 1` -> 1, `--limit 0` -> `No matching thread messages.`.
- Likely cause: `src/mailspace/thread.rs` `connected_content_ids()` inserts a dequeued id into `visited` before the `visited.len() >= limit` check, so every already-queued neighbour is still included; the limit is soft. Severity: cosmetic.

### F10 Local-to-local mail leaves the checkout; needs/tasks/wants are visible to every member - DIVERGES
- Repro: `"$VIVI" mail send --project "$A" --from mind --to hand --subject local --body x; "$VIVI" sync --project "$A"` -> `push: 1 sent`; the server list as alice (bearer minted with the harness key) contains `to=hand@f3b1eba8.local` items (sequence 3, 4, 7, ...), bob's list does not (mail is visible only to sender and addressed prefix). A need sent between two roles of B (`need send --project "$B" --from mind --to hand`) is pulled by A (`1 new (1 needs)`).
- Observed vs expected: the selection rule is "every unbound row except memo/draft/trash" (`src/sync/select.rs`), so a user who types a purely local send is uploading it; the server's `scoped_visible()` makes work items member-visible by design. Not a bug against the code, but `mail send --help` still says "Deliver local mail inside the current project mailspace only" and `mail --help` says "no external side effects". Severity: surprises.

### F11 Cosmetic and wording issues
- `mail show` text omits Cc: B's Cc copy `69300cd9` shows `To: hand@bf1b6a7e.local` only; `--json` has `cc: [mind@bf1b6a7e.local, hand@f3b1eba8.local]`. Attachments are not mentioned in either view.
- `--body "@bob please review"` -> `failed to read body file bob please review` (any body starting with `@` is a path; same for `reply`). No escape.
- `mail list --folder all` -> `no messages in all` (exit 0) while `--folder bogus` errors; `dump` accepts `all`.
- `external recipient '...' is not allowed for local mailspace delivery` is also printed for `--from hand@$PB.local`, `list --for`, `dump --for`, a display-name form (`Bob <hand@...>`) and a 7-character prefix typo; it names a recipient problem for a sender/filter problem and never says "not a mailspace address".
- `mail watch` timeout says `mailspace watch timed out ...`; `mailspace status --json` field `found` is constant true.
- `mailspace init --project <missing dir>` creates the directory tree; `mailspace status --project <dir without .vivi>` prints the raw `failed to read mailspace config ... No such file or directory (os error 2)` while cwd discovery prints the friendly `No Vivi mailspace found ... init: vivi mailspace init`.
- `sync prune --older-than 1m` on a copy of B evicted B's own sent letters (`debd552e sent`, `67a8fbd3 sent`) although the banner says "local mail are kept" ("local" = never bound to the project). Afterwards `mail show 4f31c1a9` and `mail thread` say `message not found: ...` with no hint about eviction or `sync fetch`.
- `identity list` shows the current name among its own aliases after renaming back (`formerly: hand, builder` under `hand`).
- `mailspace import --from <source whose id disagrees with its sidecar>` succeeds (guard not applied to sources).
