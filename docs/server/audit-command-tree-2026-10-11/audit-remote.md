# Audit: remote (sync, chat, pool)

Binary under test: `vivi 10.2.0` (target/debug) against `vivi-server` (target/debug), local 127.0.0.1:18144.
Harness: `audit-setup.sh WORKDIR PORT`; two synced checkouts A (alice) and B (bob). `[chat]` was added by hand
(`server = $BASE`, `recipient = "hand"`) because the harness only configures `[sync]`. Reproduction scripts for
the headline findings are in the WORKDIR: `audit-remote/repro.sh` (R1-R8) and `audit-remote/repro2.sh` (R9-R12),
outputs `repro.out` / `repro2.out`. Run `audit-setup.sh`, then `repro.sh`, then `repro2.sh` in that one world
(`repro.sh` appends `[chat]` to both checkouts, so it is not re-runnable without a fresh setup).

## Summary

28 leaves exercised. Result counts: WORKS 14, WORKS-LOCAL 5, DIVERGES 6, BROKEN 3, UNTESTED 0 at leaf level
(pool kinds fly, docker, ssh and runpod are UNTESTED, listed as four extra rows below).

Three most important findings:
1. **There is no vivi command to vouch for a member or to create a project (F1).** `sync join` tells the user to
   "ask a member to vouch for fingerprint ...", but the client cannot send a vouch (the envelope `postmaster` is
   unreachable from `mail send`). Onboarding is possible only with raw HTTP. Key loss is then a dead end (F4).
2. **One permanently rejected chat message blocks the whole chat outbox, in every room, with exit 0 and no way to
   drop it (F2).** `chat flush` and `chat send` both exit 0 while nothing is delivered.
3. **Three silent "your letter did not arrive" paths in sync (F6, F7, F8):** an over-64KiB letter is announced as
   "queued for sync" and then skipped forever; a letter to an unregistered prefix is accepted and goes nowhere;
   a refused pulled record is reported once and the next `sync` is green.

Relation of rooms to the project mailspace: they are two unrelated stores. Room items are bound to provider
`vivi-room`, `sync push` skips every one of them ("already bound to vivi-room"), and a pulled room letter cannot be
answered with `mail reply`. Chat letters do **not** sync as project letters (see F5 for what I think should change).

## Matrix

Class: REMOTE = talks to a server itself, LOCAL-ONLY = changes/shows state that never leaves the checkout.

| command | class | result | evidence |
| --- | --- | --- | --- |
| sync push | REMOTE | DIVERGES | `mail send --to hand@$PB.local --body-file big70k` prints `queued for sync`; `sync push` -> `skipped 6744f5ad: 73084 bytes, over the 64 KiB cap`, exit 0, `sync status` `unsent: 0` (F6). Letter to `hand@deadbeef.local` accepted 201, never delivered (F7). Two concurrent runs: `not bound ...: database is locked`, exit 1 (F12). Server down: `stopped: ... unreachable; 2 letter(s) left unsent`, exit 1, nothing lost. Rate limit: `stopped: ... rate limit hit (30 sends/min per member)`, resumes on next run. |
| sync pull | REMOTE | DIVERGES | forged From: `refused msg_.. From address claims mailspace b5d2ff74 ...` exit 1, then second `sync` exit 0 (F8). `state.json since=9999` -> `0 fetched ... cursor 9999`, new letter never arrives (F10). `--since 2999-01-01` accepted, plain pulls then return nothing. `--since` after a cursor exists is refused with a clear message. `--since` formats `7d`, `90m`, `YYYY-MM-DD`, RFC 3339 all work; `0m`, `1w`, `yesterday` rejected, exit 1. Unknown role: `warning: filed for unknown role ghost`, appears after `role add`. |
| sync resync | REMOTE | WORKS | `sync resync` -> `47 fetched, 0 new, 46 already present`; evicted letters come back (`5 restored`); still re-refuses forged record. With a corrupt `state.json` it errors and cannot repair (F9). |
| sync fetch | REMOTE | WORKS | `sync fetch 835b735d` -> `1 filed` after prune; handle not visible/nonexistent -> `1 missing (2845441e)` exit 0 (documented); malformed handle exit 1; uppercase hex accepted; duplicate handles collapsed. |
| sync prune | LOCAL-ONLY | WORKS-LOCAL | `sync prune --older-than 1m` evicted 6 letters incl. already-pushed `sent` copies; room-bound letters untouched; `--dry-run` identical selection; works with the server stopped; `mail show <evicted>` -> `message not found` with no hint (F16). |
| sync join | REMOTE | BROKEN | Unvouched key: `this key has not been vouched for as 'carol' ... ask a member to vouch` but no vivi command can vouch (F1). Registered name + new key: dead end (F4). Repeat join of the same key exits 1 (`already registered ... vivi sync already works`). |
| sync status | LOCAL-ONLY | WORKS-LOCAL | prints identity (two fingerprint forms), server, project, name, address, cursor, history floor, unsent; `unsent` excludes over-cap letters (F6); errors on a corrupt `state.json` (F9). |
| sync directory | REMOTE | WORKS | `alice  79600ade.local  5287b85218941113`; bad key -> clear refusal; wrong project -> `project '...' was not found on http://...`. |
| chat status | LOCAL-ONLY | WORKS-LOCAL | unconfigured: `chat server: https://vivi-chat.example` (placeholder shown as if real) while `chat flush` says `no chat server configured` (F16). |
| chat join | REMOTE | WORKS | `joined <room> seat 2 as bob-seat`; repeat join is idempotent; full room: `is full: no open seat`; taken label refused; unknown room `not found`; malformed id rejected. |
| chat rooms | LOCAL-ONLY | WORKS-LOCAL | lists room, seat, label and server; prints the placeholder server when unset. |
| chat send | REMOTE | BROKEN | `chat send $R --to nobody` -> `queued ... HTTP 400 ... unknown recipient label: nobody`, exit 0; every later send, in any room, queues behind it (F2). Plain, `--kind need/task/want`, `--reply-to`, `--ref` all deliver otherwise; 33 refs, >64KiB body, bad kind rejected up front. |
| chat flush | REMOTE | BROKEN | `flush: 0 sent (0 duplicate), 1 queued / first failure: ...` exit 0; cannot drop a row (F2, F3). Delivers correctly after outage and after rate-limit clears. |
| chat read | REMOTE | WORKS | default previews without advancing; `--mark-read` advances; `--all` replays; no room -> `no rooms joined`; two rooms -> `2 rooms joined; pass a room id: ...`. Direct messages (`--to seat`) are visible to every seat (documented). |
| chat watch | REMOTE | DIVERGES | `--once` with an arrival only in room 2 took 26 s (rooms are long-polled one after another, 25 s each) (F11). `watch` also advances the read cursor, so `read` previews stop showing what it printed. |
| chat pull | REMOTE | DIVERGES | files need/task/want/plain into `needs/tasks/wants/inbox` of `[chat] recipient` with backlog nodes, idempotent, skips items addressed to other seats. Items are then never pushed (F5) and `mail reply` refuses them (F5). Missing/unknown recipient role errors cleanly. |
| chat resync | REMOTE | WORKS | `6 fetched, 0 new, 4 already present, 2 restored`; closed need stays in `done`. |
| chat prune | LOCAL-ONLY | WORKS-LOCAL | evicted closed room need and plain item, kept open task/want and project-bound letters; `--dry-run` matches; needs no server. |
| chat leave | LOCAL-ONLY | DIVERGES | `left <room>`; outbox rows for that room stay forever (`outbox depth` never returns to 0, flush reports `queued`), read and pull cursors are dropped so the next join replays everything (F3). |
| chat room create | REMOTE | WORKS | `created <room> / seat 1 label ...`; seats 1 and 9 rejected by the server (`seats must be 2..8`); empty label rejected. Server side has no admission control (any key can create rooms). |
| pool create | REMOTE | WORKS | local worktree pool: `created here b-here-slot-1 ... seeded`; `--slots 3` over max refused. |
| pool status | REMOTE | WORKS | human, `--porcelain`, `--json` all consistent; `--json` with `--porcelain` refused. |
| pool destroy | REMOTE | WORKS | `--yes` required; `1 slot(s) destroyed`; unknown slot named; has no `--porcelain` flag unlike its siblings (F16). |
| pool scale | REMOTE | WORKS | scale-down needs `--yes`; `scaled here 2 2`; `scale 0 --yes` empties the pool. |
| pool dispatch | REMOTE | DIVERGES | works on the local worktree pool, writes `Re:` replies that sync as letters. Another member can dispatch a task that was already dispatched and closed elsewhere (F15). |
| pool wait | REMOTE | WORKS | `collected b869de83 here b-here-slot-1 finished "committed audit-branch 569d6d96fb"`; nothing open -> error, exit 1. |
| pool reconcile | REMOTE | WORKS | `nothing-open` when idle; frees slots. |
| pool doctor | REMOTE | WORKS | local: `12 ok, 1 warn`; `--test --pool here` on the local kind: `27 ok, 0 warn, 0 fail, 1 skipped` in 54 s; config validation errors name the fix. Only local kind exercised. |
| pool * on provider `fly` | REMOTE | UNTESTED | needs a paid Fly account and token; only validation errors (`fly.app is required`) were reachable. |
| pool * on provider `docker` | REMOTE | UNTESTED | would use a Docker daemon I did not start; not contacted. |
| pool * on provider `ssh` | REMOTE | UNTESTED | needs a remote host; not contacted. |
| pool * on provider `runpod` | REMOTE | UNTESTED | rents billable GPU pods; only validation errors and the `--allow-spend` refusal were reachable. |

## Findings

### F1 No vivi command can vouch for a member or create a project — BROKEN
- Repro (clean world; `C` is a fresh checkout with its own key and `[sync] name = "carol"`):
  ```
  vivi mail send --project "$A" --from mind --to postmaster --subject v --body-file vouch.json
  vivi mail send --project "$A" --from mind --to postmaster@$PA.local ...
  vivi mail send --project "$A" --from mind --to postmaster@deadbeef.local ...   # accepted, pushed as plain mail
  vivi sync --project "$A"; vivi sync join --project "$C"
  ```
- Observed: the first two fail with `unknown local role 'postmaster'`; the third is sent and pushed but is plain mail
  (envelope `postmaster@deadbeef.local`, not `postmaster`), so `sync join` still says
  `this key has not been vouched for as 'carol' ... ask a member to vouch for fingerprint F under that name`.
  Adding a local role `postmaster` and sending to it produces envelope `postmaster@<prefix>.local`, also not a vouch.
  The server only treats an envelope of exactly `postmaster` as governance; `sync/select.rs` and
  `link.rs::envelope_to` always build the envelope from the To/Cc addresses. There is also no `vivi` command to
  create a project (the harness uses `POST /v1/projects` with curl).
- Expected: a documented `vivi` route (for example `vivi sync vouch <name> <pem|fingerprint>`) for the step the error
  message tells the user to perform.
- Likely cause: `src/sync/link.rs` `envelope_to`, `src/sync/push.rs`, and the missing subcommand in
  `src/cli` / `src/local_sync_command.rs`. Severity: blocks use (onboarding needs raw HTTP; the skill does not say so).

### F2 A permanently rejected chat message blocks the whole outbox, in every room — BROKEN
- Repro (`$R`, `$R2` are two joined rooms of B):
  ```
  vivi chat send $R  --project "$B" --to nobody --subject s --body b      # server: 400 unknown recipient label
  vivi chat send $R  --project "$B" --subject good  --body g
  vivi chat send $R2 --project "$B" --subject other --body o
  vivi chat flush --project "$B"; echo $?
  ```
- Observed: all three print `queued ... unknown recipient label: nobody; retries on the next chat command`, exit 0.
  `flush: 0 sent (0 duplicate), 1 queued / first failure: ...`, exit 0, `outbox depth: 3`. Room 2 never receives
  `other`. There is no command to inspect or drop an outbox row.
- Expected: a 400 is a permanent refusal, not a retryable failure: dead-letter it (or fail at the prompt, as the
  skill says "a bad send fails at the prompt instead of sitting in the outbox"); never stop the pass for later
  rows or other rooms; non-zero exit from `flush` when nothing could be delivered.
- Likely cause: `src/chat/outbox.rs::deliver_all` breaks on the first error (`summary.error`, `break`) and
  `post_row` treats every error as retry; `command.rs::flush` returns `Ok(())` after printing the failure.
  Same head-of-line effect for a rate-limited message (HTTP 429): a retry-after message in room A delays room B.
  Severity: blocks use for that mailspace's chat until someone edits `.vivi/chat/state.sqlite` by hand.

### F3 `chat leave` orphans queued messages and drops cursors — DIVERGES
- Repro: after F2, `vivi chat leave $R --project "$B"; vivi chat flush --project "$B"; vivi chat status --project "$B" | tail -1`.
- Observed: `left <room>`, then `flush: 1 sent, 2 queued / first failure: room '<R>' is not joined`, exit 0,
  `chat outbox depth: 2` permanently (rows for a left room are skipped, never removed; leaving does unblock the
  other rooms). Re-joining replays the room from zero because the read and pull cursors were dropped
  (`chat read` shows everything again, `chat pull` refetches and re-counts "already present").
- Expected: leave either flushes or discards that room's rows (and says so), and keeps cursors for a re-join.
- Likely cause: `src/chat/state.rs` `leave_room`. Severity: surprises.

### F4 Losing the checkout key is a dead end — BROKEN
- Repro:
  ```
  rm "$B/.vivi/chat/identity.ed25519"; vivi chat status --project "$B"    # mints a new key
  vivi sync --project "$B"; vivi sync join --project "$B"
  # a member vouches the new key under the same name (raw PUT, see repro.sh R3)
  ```
- Observed: `sync` -> `refused the bearer mint for 'bob' ... (run vivi sync join)`; `join` -> `this key has not been
  vouched for as 'bob' ... ask a member to vouch`; the vouch is refused by the server:
  `{"error":{"code":"conflict","message":"name bob is already admitted for a different key"}}`. Every step the CLI
  suggests leads to the next suggestion; no key rotation exists. The only exit is a new `[sync] name` and a new
  mailspace id (the `id` line removed), i.e. a new address prefix and a lost identity. The key file is gitignored
  and 0600, so a fresh clone or a disk loss always lands here.
- Expected: a rotation path (admin re-vouch that replaces a name's key, or a clear statement that the name is lost)
  and a join error that distinguishes "not vouched" from "name taken by another key".
- Likely cause: `vivi-server/src/admissions.rs` binds a name to the first admitted key and refuses a second;
  `src/sync/http.rs::join_error` still says "ask a member to vouch ... under that name", which the server then
  refuses. Severity: blocks use for that member.

### F5 Room items never reach the project, and are listed as skipped forever — DIVERGES
- Repro: A `chat send $R --kind task ...`; B `chat pull $R`; then `vivi sync push --project "$B"` and
  `vivi mail reply <handle> --project "$B" --from hand --body ack`.
- Observed: `sync push: 0 sent, 0 already on server, 1 skipped` / `skipped fc385bf8: already bound to vivi-room
  <room>`, on every push and every `vivi sync` run (exit 0); `sync status` says `unsent: 0`. The skip list is
  capped at 10 lines plus `... and N more skipped`, so after a few room items the actionable skips (over-cap
  letters, F6) disappear into the tail. `mail reply` to the pulled room letter fails with
  `reply parent has no other local identity or remote address to receive the reply`; only
  `chat send --reply-to <handle>` works. Project members who are not in the room (rooms hold 2..8 seats, created
  with no admission control) never see these items, and a room item cannot be re-pushed to the project (every row
  bound elsewhere is skipped).
- Expected / opinion: chat letters should not auto-sync (different identity, admission and size model), but the two
  surfaces should not interfere: `BoundElsewhere` for a `vivi-room` row is policy, not actionable, and should be
  counted like memos rather than listed; `mail reply` should accept the room address or say to use
  `chat send --reply-to`; the skill should say rooms and project sync are separate.
- Likely cause: `src/sync/select.rs` (`SkipReason::actionable` includes `BoundElsewhere`), reply addressing in the
  mail reply path. Severity: surprises.

### F6 A letter over 64 KiB is "queued for sync" and then never sent, with exit 0 — DIVERGES
- Repro: `python3 -c 'print("y"*70000)' > big.txt; vivi mail send --project "$A" --from mind --to hand@$PB.local
  --subject big70k --body-file big.txt; vivi sync status --project "$A" | tail -1; vivi sync push --project "$A"; echo $?`
- Observed: send prints `sent 6744f5ad / queued for sync: hand@<PB>.local`; `sync status` `unsent: 0 letter(s)`;
  push `skipped 6744f5ad: 73084 bytes, over the 64 KiB cap`, exit 0; B never gets it. Re-listed on every push.
- Expected: reject or warn at send time (the cap is known client-side: `MAX_LETTER_BYTES`), count it in `unsent`,
  and make `sync push` exit non-zero while it is stuck.
- Likely cause: `src/sync/select.rs` (skip, not failure) and `PushReport::failure` ignoring `skipped`.
  Severity: surprises (a message the sender believes was delivered).

### F7 A letter to an unregistered address prefix is accepted and goes nowhere — DIVERGES
- Repro: `vivi mail send --project "$A" --from mind --to hand@deadbeef.local --subject typo --body x; vivi sync --project "$A"`.
- Observed: `queued for sync: hand@deadbeef.local`, push `1 sent`, server answers 201, exit 0. Nobody holds the prefix;
  only the sender can read it back (`sync fetch <handle>` as B -> `1 missing`, exit 0). No warning at send, push or
  pull. A prefix typo is indistinguishable from success.
- Expected: check the recipient prefix against the cached member directory (or have the server flag an unknown
  prefix) and warn; `sync directory` already has the data.
- Likely cause: `vivi-server/src/projects.rs` `AppendItem::new` ("domains need not be registered") plus no client
  check. Severity: surprises.

### F8 A refused pulled record is reported once, then sync is green — DIVERGES
- Repro: `repro.sh` R5 (alice PUTs a letter whose From claims bob's prefix; `sync` on B twice).
- Observed: first `sync`: `1 refused: msg_..` / `refused msg_..: From address claims mailspace b5d2ff74 but the
  sender is registered as aa726e9e`, exit 1. Second `sync`: `0 fetched ... 0 refused`, exit 0, cursor past the
  record. `sync status` shows no refusal count. `sync resync` refuses it again (exit 1), so the record is
  recoverable only by someone knowing to run `resync`.
- Expected: keep a refused count (or a list) visible in `sync status` until acknowledged, or keep exit non-zero.
- Likely cause: `src/sync/pull.rs` advances the cursor over refused records; nothing persisted in `SyncState`.
  Severity: surprises (a cron loop logs the failure once and then looks healthy).

### F9 A corrupt `sync/state.json` blocks `resync`, `fetch` and `status` — BROKEN
- Repro: `printf '{"project":"%s","sinc' "$PID" > "$B/.vivi/sync/state.json"; vivi sync resync --project "$B"`
  (also `sync fetch <h>`, `sync status`, `sync pull`; a 0-byte file behaves the same).
- Observed: `sync state <path> is unreadable: EOF while parsing a string at line 1 column ...`, exit 1. `sync push`
  alone still works. The repair command (`resync`) loads the state before resetting it, and `fetch` loads a cursor it
  never uses. The message does not say that deleting the file fixes it.
- Expected: `resync` (and `fetch`) ignore or replace an unreadable state file; error text names the fix.
- Likely cause: `src/sync/pull.rs::pull_with` -> `SyncState::load` before `apply_plan`. The write path is atomic, so
  this needs disk-full or hand editing. Severity: surprises.

### F10 A cursor ahead of the server silently skips everything new — DIVERGES
- Repro: set `since` to 9999 in `$B/.vivi/sync/state.json`; A sends a letter and syncs; `vivi sync --project "$B"`.
- Observed: `0 fetched ... cursor 9999`, exit 0, the new letter is not delivered; `sync status` shows `cursor: 9999`
  and `history from: full history`. The server answers `next_cursor = since` when nothing follows, so a restored
  or recreated server (sequence goes backwards) is undetectable.
- Expected: when `since` exceeds the server head, warn or reset; the server could return its head sequence.
- Severity: surprises, low likelihood (needs a server rollback or a hand-edited state file).

### F11 `chat watch` polls rooms one after another — DIVERGES
- Repro: two joined rooms, A sends in the second one only; `time vivi chat watch --once --project "$B"`.
- Observed: 25-26 s before the message prints (room 1's 25 s long-poll runs first). With n rooms the worst
  latency is (n-1) x 25 s. `watch` also stores the cursor after each cycle, so `chat read` (a preview) then shows
  nothing for what `watch` already printed.
- Expected: poll rooms concurrently, or one server call for all rooms; document the cursor sharing.
- Likely cause: `src/chat/watch.rs::run` iterates rooms serially with `POLL_WAIT_SECS = 25`. Severity: surprises.

### F12 Two syncs at once on one checkout fail with `database is locked` — BROKEN
- Repro: `repro2.sh` R12: five new letters, then `vivi sync --project "$A" & vivi sync --project "$A" & wait`.
- Observed: `not bound 5f1c870a: not bound: failed to upsert remote binding: database is locked`, `1 letter(s)
  refused or not bound; see above`; 8 such lines and 6 of 6 runs exited 1 in `repro2.sh` (two of those exits were
  the 30 sends/min rate limit from the test's own volume). The letters were accepted by the server; the binding is
  written on a later pull (`bind_held`), so state converges (a longer 6-round run delivered 30 of 30 letters), but
  the run exits 1. No `state.json` corruption seen (the write is atomic; the shared `state.json.tmp` name makes a
  rename race conceivable, not observed).
- Expected: a busy timeout or a cross-process lock around a sync pass.
- Likely cause: SQLite `bind_remote` without a busy timeout (`src/storage`), `src/sync/push.rs::bind_sent`.
  Severity: surprises (cron overlapping an interactive run).

### F13 `sync fetch` exits 0 when it fetched nothing — DIVERGES (documented behaviour)
- Repro: `vivi sync fetch deadbeef --project "$B"; echo $?` (or a handle only its sender may read, F7).
- Observed: `sync fetch: 1 handle(s), 0 filed, 0 already held, 1 missing (deadbeef)`, exit 0. The skill documents
  "reported as missing, not an error", so this is by design, but a script cannot tell success from a miss, and a
  letter that is hidden by visibility rules looks the same as a handle that never existed.
- Likely cause: `src/local_sync_command.rs::fetch` uses `report.failure()` which ignores `missing`.
  Severity: cosmetic.

### F14 Switching `[sync] project` strands history and the old cursor — DIVERGES
- Repro: `repro2.sh` R11 (create a second project, point B's `[sync] project` at it, join, sync, switch back).
- Observed: every existing letter is `skipped ...: already bound to vivi-project <P1>` and never pushed to P2;
  letters sent while on P2 are bound to P2 and never pushed to P1; `state.json` holds one project, so switching back
  shows `sync cursor: 0 / history from: nothing pulled yet` and the next pull refetches everything (idempotent).
- Expected: a documented re-seed path, or per-project state. Severity: surprises (rare).

### F15 `pool dispatch` has no cross-member claim: a completed task can be dispatched again — DIVERGES
- Repro: `repro2.sh` R9: A sends a task to B; B dispatches on a local worktree pool, waits, `task done`; both sync;
  A (holding the sent copy) runs `vivi pool dispatch <task> --model fake/model --branch audit-branch-a`.
- Observed: A's dispatch succeeds (exit 0), collects, and writes two more `Re: pool job` replies that sync to B
  (4 replies in the thread). "Done" and open-dispatch state are local; nothing tells A the task was closed or
  already collected by B. On paid pools this is duplicated spend.
- Expected: at least a warning when a pulled reply of kind `pool result` already exists for the task.
- Severity: surprises (the general "closed here, still open there" of the brief, with a cost).

### F16 Smaller things worth fixing — cosmetic
- `chat status` and `chat rooms` print `chat server: https://vivi-chat.example` when `[chat] server` is unset,
  while `chat flush/read/send/pull` say `no chat server configured`.
- Fingerprints: `chat status` / `sync status` headline `43a8030be5b4a616`; `sync directory`, `chat join` and
  `chat read` labels print the wire fingerprint (`5287b85218941113...`). A user comparing them sees two different
  values for one key (only `sync status` labels the second one `wire`).
- `sync prune`'d letters disappear from `mail list`, `mail show` -> `message not found: <h>` with no pointer to
  `sync fetch`; the 6-line summary does name `resync`/`fetch`.
- `pool destroy` has no `--porcelain`; every other pool subcommand does, and the skill says agents pass it.
- `sync pull --since 2999-01-01` is accepted and records a floor in the future; later plain pulls return nothing.
- `[sync] server = "127.0.0.1:18144"` (no scheme) reports `project server unreachable: builder error`.
- `sync join` a second time exits 1 (the message is clear); a provisioning script must tolerate it.
- Governance letters (vouch mail to `postmaster`) are filed under a non-role account `postmaster@<domain>`, which
  `mail list --for` rejects (`external recipient ... is not allowed`) but `boot` counts as unabsorbed mail.
- `sync` prints the same `skipped ...` block on every run once any letter is bound elsewhere (see F5).
- `chat send --body ""` and a missing `--body-file` (`error: No such file or directory`, no path) are accepted/unhelpful.
- Rooms: `POST /v1/rooms` needs no credentials, so any key can create rooms on the same server that hosts the
  admission-controlled project API.

## Not found broken (checked)
- Push/pull against a stopped, frozen (SIGSTOP: 15 s timeout per phase, 30 s total) and wrong-port server: exit 1,
  no state change, full recovery after restart.
- Wrong or malformed project id: `project '...' was not found on <server>` / `invalid project id`, exit 1; state
  file untouched (but a push with only bound-elsewhere letters exits 0 without contacting the server).
- State file from another project reads as empty (no silent skip), and is overwritten on the next pull.
- Push of the same bytes the server already holds under another id: `0 sent, 1 already on server`, no second row
  at the sender; the receiver files it under the server's id (handles differ on the two sides, so a handle quoted
  by the sender does not resolve at the receiver).
- A verbatim copy of a checkout (same id, key and name) works as a replica; a second checkout with the same id and
  a new key cannot join (`address prefix already held`, by design).
- `chat prune` / `sync prune` never touch the other surface's letters; both restore byte for byte with local state
  (closed need stays in `done`) via `chat resync` / `sync resync`.
- No bearer, signature or key bytes in `-v` output; `identity.ed25519` is 0600.
- `vivi board` with the chat server frozen returns in 1.5 s (bounded probe).
