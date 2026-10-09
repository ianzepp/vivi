# Vivi Chat Phase 02: Client Delivery (Traveling Identity And Sync)

This is the vivi-side delivery record for phase 02. The phase design, the
wire contract revisions (R7/R8), and the vivi-server units (P2-3, P2-4) live
in the phase's declared delivery home,
[`docs/phase-02-identity-and-sync.md`](https://github.com/ianzepp/vivi-server/blob/main/docs/phase-02-identity-and-sync.md)
in the `vivi-server` repository. This document records what the client units
shipped, the decisions made while building them, what was verified, and what
is carried forward. It does not restate the design.

## Interpreted Phase Problem

Handles were per-store: a short handle is a prefix of a message id, and ids
were seeded with store-local components (`account`, `local_role`,
`seed_hint`), so one need minted into three mailspaces had three ids and
three handles. A need that crossed a room could not be referenced from the
far side, and a citation inside a body ("supersedes `abcd1234`") was wrong
the moment the body left its store.

Phase 02 makes the id travel: minted once at creation, carried inside the
item, and imported verbatim at ingest, so the same item has the same id and
the same handle in every store. The client units are the ingest half (P2-1),
the mint (P2-2), pulling room items into a mailspace (P2-5), sending work
items across a room (P2-6), and the cache lifecycle (P2-7).

## Outcome Record

All client units landed on `main` (commit hashes read from git log on
2026-10-09):

| Unit | What | Commit |
| --- | --- | --- |
| P2-1 | Ingest honors a presented traveling id (validation, precedence, conflict guard) | `24c446e` |
| P2-2 | The chat Message-ID is minted as a traveling id at creation | `dcc0a76` |
| P2-5 | `vivi chat pull`: room items file into the mailspace by kind | `6a1a081` |
| P2-6 | `vivi chat send --kind/--reply-to/--ref`: work items cross the room | `438050d` |
| P2-7 | `vivi chat prune` and `vivi chat resync` | `4a4b764` |

Test counts: 366 across the suite after P2-7 (318 at the end of phase 01).
`cargo fmt --check` and the hygiene test pass with no budget raised. The
repo carries a large pre-existing clippy backlog (about 70 pedantic findings,
mostly missing `# Errors` docs); the new modules add none, and the new
handlers in `chat/command.rs` are documented.

## Wire Consumed

The client builds against the wire recorded in the vivi-server phase doc,
as implemented in vivi-server `be98c4e`:

- send body gains optional `kind` (`need`, `task`, `want`), `in_reply_to`, and
  `refs` (at most 32); a send with a `kind` must carry a `Message-ID` header
  of exactly `msg_` plus 24 lowercase hex
- poll messages gain `kind`, `in_reply_to`, `refs`, each omitted when empty
- the server returns every record in a room to every seat; it does not
  filter direct messages by recipient

## Decisions

**Ids and ingest**

- Id precedence at ingest: the trusted local `message_id_hint` (merge, drafts),
  then a validated `presented_id`, then the remote-bound seeded mint, then the
  local fallback. Local-only mail keeps the seeding it always had.
- A presented id must be exactly `msg_` plus 24 lowercase hex. A malformed id
  is an error before any blob is written, never a silent fallback: a fallback
  would give the item a store-local id and break the same-handle guarantee.
- A presented id is foreign input. An id that already names different bytes is
  refused rather than overwritten, and one delivery batch cannot give a
  presented id to two recipients.
- `mint_traveling_id` reuses the existing digest with the store-local seed
  components excluded; the caller supplies the randomness.

**Rendering and filing (P2-5)**

- A pulled item's `.eml` is a pure function of `(room_id, record)`: no clock,
  no local identity, no cursor. Every store derives the same `content_id` for
  the same item, and an evicted item is rebuilt byte-identically. The local
  recipient lives only in the `messages.account` column.
- Every record field is flattened to header-safe text (labels, subjects, ids);
  the exact seat label is kept in `X-Vivi-Chat-From`. Non-ASCII subjects are
  RFC 2047 encoded.
- Filing: `need`/`task`/`want` go to `needs`/`tasks`/`wants` with an
  `X-Vivi-Kind` header and a backlog node; plain chat goes to `inbox`; the
  seat's own sends come back as read `sent` copies with no backlog node.
  Unknown future kinds degrade to plain mail instead of failing a pull.
- Each item gets one `vivi-room` remote binding: room = mailbox, cursor = UID,
  fixed `uidvalidity` 1 (the room id's entropy plays that role, D4), and a
  `delivered` event in the same transaction so watchers see arrivals.
- Down-sync is opt-in: it requires `[chat] recipient = "<role>"`. Unset keeps
  chat print-only, exactly as before. The recipient is explicit rather than
  defaulting to "the only role", so adding a role later cannot silently
  reroute items.
- Direct items addressed to another seat are skipped and counted.
- The sync cursor is independent of the read cursor, so `chat read
  --mark-read` and the board badge can never make pull skip items.
- Ingest is idempotent and never touches local state: an existing row is left
  exactly as it is, so a replay or resync cannot reopen a closed need or reset
  a read flag.
- An id the room reuses with different bytes is a reported `Conflict`
  outcome, not an abort. The item is refused and named on stderr and the rest
  of the room still syncs, so a divergent or hostile item cannot wedge a seat's
  sync.
- Ids that are not traveling ids (sent before phase 02, or by another client)
  file under a stable store-local id and the pull summary says so.

**Sending (P2-6)**

- `Draft` bundles addressing, content, kind, and threading, and is validated
  against the server's limits (kind, 32 refs, ids of 1-256 bytes) before
  anything is enqueued, so a bad send fails at the prompt instead of becoming a
  row the server will never accept.
- `kind`, `in_reply_to`, and `refs` are stored in the outbox envelope only
  when set, so plain chat envelopes and payloads are byte-identical to phase
  01 and a queued work item retries under the same id with its kind intact.
- `--reply-to`/`--ref` take a local handle or a full `msg_` id. A full id
  passes through even when this mailspace has not seen it, since a reference
  may name an item that lives elsewhere (D3).
- Send prints `handle <8 hex>`, the handle the recipient will see.

**Cache lifecycle (P2-7)**

- Prune policy: explicit only (keep-all by default, answering the phase's
  open question), bound to `vivi-room` items only, never open
  needs/tasks/wants, never starred or absorbed items, and only items dated
  before the cutoff. The cutoff compares the item's own date, not when it was
  pulled. Pruning is purely local and still works after `chat leave`.
- Eviction soft-deletes the row and drops a blob only when no live row still
  references it; the `blobs` and `messages` rows stay so a restore is in place.
- `resync` is the same handler as `pull` with the poll started at cursor zero,
  and is the only thing that restores; a plain pull resumes at its cursor.
- Prune ages accept days (`30d`) via a local parse. The shared interval parser
  deliberately rejects `d` and a test pins that, so it was not extended.

**Open questions from the phase doc, as resolved**

- Prune default: keep-all; nothing prunes on its own.
- Whether `kind` becomes a mail header: the wire carries it as a JSON field
  (the server seals it at rest); the client writes it as a local `X-Vivi-Kind`
  header on the rendered item, using the crate's existing header path.
- Multi-room local stores: one shared mailspace; the room id is the remote
  binding's mailbox, so items from several rooms coexist and prune can filter
  by room.
- Lazy fetch of dangling references: not built (see carried-forward items).

## Verification

- Unit tests per module: `storage/sync` (insert, existing, conflict, evict,
  restore, shared blobs), `chat/item` (determinism, hostile fields, encoding),
  `chat/sync` (filing, idempotent replay, skips, legacy ids, reply links,
  cursors, collisions), `chat/outbox` (wire body, validation limits, queued
  retry), `chat/prune` (policy matrix, dry run, round trip).
- Live end-to-end against a build of vivi-server at `c8bc1e7` (the lockfile
  pinned this repo at `ad8206f`): 33 checks, all passing.
  - The legacy smoke legs still pass against the new client (plain send,
    board badge, `watch --once`, `read` with origin labels).
  - A need citing a local handle crosses the room, files on the far side under
    the same handle, and a replay files nothing new.
  - A need closed on the far side stays closed after a pull.
  - A reply sent back threads to the original on the sender.
  - A direct task for another seat is not filed as inbound work.
  - After `prune` and `resync` the set of blobs on disk is byte-for-byte the
    set before, a need closed locally comes back still closed, and a second
    resync is a no-op.
- The live script was a throwaway in a session scratchpad and is not committed.
  The acceptance extension belongs in vivi-server's
  `scripts/two-checkout-smoke.sh` (P2-9); the legs above are its outline.

## Open Items Carried Forward

1. **Outbox poison row.** A queued send the server permanently rejects stays
   queued forever and blocks every send behind it; nothing lets an operator
   drop it. Client-side validation makes this unlikely for the new fields but
   not impossible (a server rule change, for example). A dead-letter state for
   permanently rejected rows is the fix.
2. **No automatic receipt.** Closing a received need locally does not tell the
   sender. The P2-9 acceptance leg (the receipt lands on the sender and
   threads to the original) works today only by hand, with `vivi chat send
   --reply-to <handle>`. Whether `need done` on a room-sourced item should
   enqueue that reply, and what the receipt body carries, is undecided and
   unowned by any P2 unit. The phase-01 deferrals also remain: `via=room`
   completion receipts, reopen rails, evidence-bearing receipt bodies, and
   `--to <mind>@<room>` addressing.
3. **Local replies cannot reach a remote seat.** Observed: `vivi mail reply`
   on a pulled item fails with "reply parent has no other local identity to
   receive the reply", because the sender is a room seat, not a local role.
   Replies to a remote seat go through `chat send --reply-to`.
4. **One recipient role per mailspace.** Every pulled item files for the single
   `[chat] recipient`; there is no routing to a specific local role.
5. **Reply links form only against a live local parent.** A reply whose parent
   is not yet in the mailspace is stored without a link; a later resync links
   it once the parent arrives. Nothing links retroactively on a plain pull.
6. **Dangling references are unlabeled.** D3 calls for a reference to an
   absent item to resolve as labeled absence ("not in this workspace; origin
   room R, seat A"). Today it simply stays unresolved, and lazy fetch from
   room history is not built.
7. **Pre-phase-02 items do not share handles.** Items whose wire ids are not
   traveling ids file under store-local ids, so they get different handles on
   different machines. There is no migration.
8. **Sync is manual.** `chat watch` prints arrivals but does not ingest them,
   and nothing pulls in the background; there is no live sync.
9. **Resync downloads the whole room in one response.** The poll has no page
   size, so resync cost grows with room history.
10. **Cursor range.** The remote binding's UID is a `u32`; a room cursor above
    4,294,967,295 is counted as malformed and skipped.
11. **Prune and resync trust the server's `created_at`.** An item with an
    unparseable timestamp is skipped and counted as malformed, so it is never
    filed (and is counted again on each resync); prune compares that same
    date.
12. **Sidecar limit (vivi-server).** The room mailspace is still written but
    not read by the server; folding it into reads is P2-8, owned by the
    server session.
