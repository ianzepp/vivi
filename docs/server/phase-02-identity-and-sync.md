# Phase 02 — Traveling identity and cross-mailspace sync

Status: **DELIVERED** 2026-10-09, with P2-8 (optional) deferred. See
[Outcome](#outcome) for commits, verification, and what is carried forward.
This doc is the phase delivery home: contract revisions for this phase are
recorded here, per the phase-01 contract's revision rule.

**Direction change:** a later design conversation moved the tenant from the room
to the project and removed the room concept. See the
[Phase 03 draft](phase-03-project-mailspace-hub.md). Phase 02 stays the record of
what shipped.

## The problem

Handles are per-store. A short handle is a pure function of a message id
(8-char prefix of the id basis), and message ids are hash-derived — but the
seed embeds store-local namespace components (`account`, `local_role`,
`seed_hint`), so the same need minted into three mailspaces gets three ids
and three handles. Two failure modes follow when a need travels from
developer A's laptop through a server room to developer B's laptop:

1. **Self-identity** — the need's own handle differs per store, so "the need
   I sent you" cannot be referenced across machines.
2. **Intra-body references** — needs/tasks/wants carry text citing handles
   of earlier items ("supersedes `abcd1234`"). Those references are wrong as
   soon as the body is copied to any other namespace.

The target workflow: two developers, same GitHub project, local clones each
with their own mailspace, both seated in one server room. A local LLM
(tugboat/speedboat Mind) files a need; it crosses the room; the other
developer's Mind absorbs it — replacing the GitHub-issue round trip. Handles
must survive that crossing.

## What the code already gives (verified 2026-10-09)

| Fact | Evidence |
| --- | --- |
| Handles are pure functions of ids; no per-store counters | `vivi/src/storage.rs` `short_handle`, `SHORT_HANDLE_LEN` |
| Ids are SHA-256-derived but namespace-seeded | `vivi/src/storage.rs` `opaque_message_id`, `fallback_message_id`, `remote_bound_message_id` |
| Mail items are content-addressed `.eml` blobs with a SQLite index | `vivi/src/storage/schema.rs` `blobs`, `blob_relpath`, `write_blob_if_absent` |
| A remote-sync binding table exists (IMAP-shaped: mailbox/uid/uidvalidity/stale) | `vivi/src/storage/schema.rs` `remote_bindings` |
| An append-only event log exists per store | `vivi/src/storage/schema.rs` `mailspace_events` |
| `SendRequest` already has `kind`, `reply_to`, `depends_on`; the server passes them empty | `vivi-server/src/messages.rs` `store_message` |
| The chat layer already mints a per-send `Message-ID`; sends are idempotent by it | phase-01 contract, sidecar index |
| Rooms replay from cursor 0; cursors are per-seat and monotonic | phase-01 contract, poll/read path |
| The server's read path is the sidecar; the room mailspace is written but unread | contract recorded v1 limit |
| Bodies are sealed server-side; the server can never rewrite body text | `crypto.rs`, sealed `StoredContent` |

## Design decisions

- **D1 — One traveling id.** The chat `Message-ID` and the mailspace
  message id unify into one namespace. The id is minted once at creation,
  reuses `opaque_message_id` machinery with store-local seed components
  excluded, and travels inside the item. Interim home: the sealed
  `StoredContent` JSON (grows `message_id`, `in_reply_to`, `refs`).
  Final home: a mail header once the crate grows the custom-header path the
  phase-01 contract deferred here. Every store **imports** the presented id
  at ingest instead of minting (validate `msg_[0-9a-f]{24}`; foreign input;
  existing collision math covers the 96-bit case).
- **D2 — Handles preserve themselves.** Same id → same 8-char handle in
  every store, because `short_handle` is a pure prefix function. No
  translation layer, no alias tables, no body rewriting.
- **D3 — Dangling references degrade honestly.** A stable id reference to
  a node absent from the local store resolves as labeled absence ("not in
  this workspace; origin room R, seat A") — the same posture as
  `resolve_message_token` reporting ambiguity instead of guessing. Lazy
  fetch from room history is the follow-on, not a prerequisite.
- **D4 — Sync is the IMAP model the crate already implements internally.**
  Room = mailbox, cursor = UID, per-seat cursors = per-client state,
  replay-from-0 = full resync, 125-bit random room ids = UIDVALIDITY solved
  by entropy, `remote_bindings` rows bind pulled items
  (`provider = vivi-room`, `remote_mailbox = room_id`, `remote_uid = cursor`),
  content-addressed blobs dedupe across stores for free.
- **D5 — Append-only mutation semantics.** Closure is an event
  (`need done`), revisions are new items, edits never mutate. Replication is
  convergent by construction; there is no merge problem and no EXPUNGE
  problem. Prune is local cache eviction (`deleted_at` local-only, blob
  droppable, resync rebuilds byte-identically). A sent local-origin item is
  shared — closable, supersedeable, never recallable. `read_state`,
  `starred`, `absorbed_at` stay local forever.
- **D6 — Kind-aware crossing.** The send path carries `kind` (and
  `reply_to`/`depends_on`) so pulled items file into local folders by kind
  (`needs/`, `wants/`, `tasks/`); the room store stops flattening everything
  to role `inbox`.

## Wire contract impact (additive)

- **R7** — sealed `StoredContent` grows `message_id`, `in_reply_to`, `refs`
  (and `kind`, below). Wire responses already carry `message_id`.
- **R8** — send request grows optional `kind`; server passes it through to
  the stored mail item. Absent `kind` behaves as today (plain chat message).

### R7/R8 as implemented (vivi-server P2-3, P2-4)

This is the wire the vivi client units build against.

**Send** — `POST /v1/rooms/:id/messages` body gains three optional fields:

| Field | Type | Rule |
| --- | --- | --- |
| `kind` | string | `need`, `task`, or `want` (case-insensitive, stored lowercase). `mail`, empty, or absent is a plain chat message. Anything else is `400 invalid_request`. |
| `in_reply_to` | string | Message-ID this item answers. Non-empty, at most 256 bytes. |
| `refs` | string[] | Message-IDs this item cites. At most 32 entries, each non-empty and at most 256 bytes. |

- **Traveling id (D1).** A send with a `kind` must carry a `Message-ID`
  header of exactly `msg_` + 24 lowercase hex (`msg_[0-9a-f]{24}`), else
  `400 invalid_request`. The id is minted client-side once and the server
  stores it verbatim. Plain chat keeps accepting any client `Message-ID`
  (up to 256 bytes) or none, so existing clients are unaffected.
- **Dedup** is unchanged: a replayed `Message-ID` returns the original with
  `"duplicate": true` and stores nothing.
- `in_reply_to` and `refs` are never resolved server-side (D3). A ref to an
  item the room has not seen stays a ref.
- Validation runs before the rate limiter, so a malformed send does not
  spend a rate-limit slot.

**Poll** — `GET /v1/rooms/:id/messages` message objects gain `kind`,
`in_reply_to`, `refs`. Each is **omitted** when absent or empty, so a plain
chat message serializes byte-for-byte as in phase 01. (The proposal said no
response shape changes; the down-sync unit P2-5 cannot file pulled items by
kind without `kind` on the wire, so the response grows these three additive
fields.)

**At rest.** The sealed `StoredContent` carries `message_id`, `kind`,
`in_reply_to`, `refs`, all defaulted, so records sealed before phase 02 still
decode. The sidecar index is unchanged and holds none of them in the clear.

**Placement (D6).** `need` / `task` / `want` file into the recipient's
`needs` / `tasks` / `wants` folder with the matching mail kind; plain chat
stays in `inbox`. Because role and kind match, the crate also mints a
`backlog` node per delivered item in the room mailspace.

**Deviation from the proposal: `reply_to` / `depends_on` stay empty.** The
crate resolves both against the room mailspace and errors on a handle it has
not stored (`resolve_reply_parent`, `backlog_validate_deps`), which is the
dangling case D3 says must degrade rather than fail. Threading therefore
rides in the sealed content as `in_reply_to` / `refs`. Populating the
crate-level fields is deferred until traveling ids exist in the room store
(P2-1/P2-2) and the target is known to be present.

## Work units (two repos; vivi crate ships first, server bumps the lockfile)

| Unit | Repo | Work |
| --- | --- | --- |
| P2-1 | vivi | Ingest honors a presented traveling id: validation + branch where presented id wins over namespace-seeded mint. Local-only mail keeps current seeding. |
| P2-2 | vivi | Creation mints the traveling id once (send/outbox path); the chat `Message-ID` becomes the message id, one namespace. |
| P2-3 | vivi-server | `store_message` writes `message_id`/`in_reply_to`/`refs` into the sealed content; sidecar unchanged. |
| P2-4 | vivi-server | Kind pass-through: populate `SendRequest.kind`/`reply_to`/`depends_on`; kind-aware placement in the room store. |
| P2-5 | vivi | Chat client down-sync: pulled items ingest as real mail items under traveling ids, `local_role` from kind, one `remote_bindings` row each. |
| P2-6 | vivi | Outbox up-sync beyond chat: needs/wants/tasks sendable with kind riding. |
| P2-7 | vivi | Prune policy + explicit resync (pull from cursor 0 through the same ingest path). |
| P2-8 | vivi-server (optional) | Fold the sidecar index into mailspace reads or make the two writes transactional — retires the recorded phase-01 limit; first reader is exactly this phase. |
| P2-9 | both | Acceptance extension (below). |

Order: P2-1, P2-2 → P2-3, P2-4 → P2-5, P2-6 → P2-7, P2-9. P2-8 slots
anywhere after P2-3.

## Acceptance

Extend `scripts/two-checkout-smoke.sh` with a needs-crossing leg: checkout
A files a need citing a prior local handle; sync; checkout B pulls it and
`vivi task show <handle>` resolves to the same handle A sees; B closes it;
the receipt lands on A and threads to the original. Plus a prune-and-resync
leg: B prunes, resyncs from cursor 0, byte-identical replica. The deployed
URL gate (`SERVER_URL=… scripts/two-checkout-smoke.sh`, against staging and
then production) runs every leg except offline queue/flush, which has to
restart the server and so stays a local-mode leg.

## Outcome

| Unit | Repo | Commit |
| --- | --- | --- |
| P2-1 ingest honors a presented traveling id | vivi | `24c446e` |
| P2-2 the chat Message-ID is minted as a traveling id | vivi | `dcc0a76` |
| P2-3, P2-4 sealed fields, `kind` pass-through, placement (R7/R8) | vivi-server | `be98c4e` |
| P2-5 `chat pull` down-sync | vivi | `6a1a081` |
| P2-6 `chat send --kind/--reply-to/--ref` | vivi | `438050d` |
| P2-7 `chat prune` / `chat resync` | vivi | `4a4b764` |
| P2-9 acceptance legs in the smoke script | vivi-server | `2bdc777` |
| P2-8 fold the sidecar into mailspace reads | vivi-server | deferred |

The client decisions and their reasons are recorded in vivi's
[`docs/factory/vivi-chat-phase-02-delivery.md`](https://github.com/ianzepp/vivi/blob/main/docs/factory/vivi-chat-phase-02-delivery.md);
this doc does not restate them.

**Verified.** 35 server tests. `scripts/two-checkout-smoke.sh` passes 29 of 29
locally, and 24 passed, 0 failed, 1 skipped (the local-only offline leg)
against the staging service running `main` at `2bdc777`. The phase 02 legs
cover a need crossing the room under one handle with an identical content hash,
a replay filing nothing new, a closed need staying closed, a receipt threading
to the original, a task for another seat being skipped, and prune plus resync
restoring the blob set byte-for-byte. Production passed the same run (24
passed, 0 failed, 1 skipped) after `release` advanced to `b47e972`; a health
probe every 5 seconds across the swap saw only HTTP 200.

**Carried forward** (the full client list is in the vivi record):

- A direct message is not private. The server returns every record in a room to
  every seat; the client skips items addressed to another seat, but `to` is a
  routing label, not access control. This is the phase 01 contract, unchanged.
- P2-8 stays deferred. The room mailspace is still written but not read, and
  the sidecar index is the read path. Folding them would need one item per
  message in the room store: vivi refuses to give one presented id to two
  recipients in a single delivery, so a broadcast cannot reuse the traveling id
  for every recipient copy.
- There is no automatic receipt: closing a received need does not tell the
  sender. The acceptance receipt is a reply sent by hand with
  `chat send --reply-to`.
- A dangling reference stays unresolved rather than rendering as labeled
  absence (D3), and lazy fetch from room history is not built.
- Pre-phase-02 items do not share handles across machines, and sync is manual
  (`chat pull`); nothing pulls in the background.
- A queued send the server permanently rejects blocks the outbox behind it.
- The offline queue/flush leg stays local-only. Prune's smallest cutoff is `1m`,
  so the full smoke takes about two minutes, and every deployed run leaves one
  throwaway room behind.

## Open questions (resolved)

- Prune default: keep-all. Nothing prunes on its own; `chat prune` is explicit
  and never touches open needs, tasks, or wants.
- Lazy fetch from room history for dangling refs: not built; unresolved refs
  stay unresolved.
- Whether `kind` also becomes a mail header: on the wire it is a JSON field,
  sealed at rest by the server. The client writes it as a local `X-Vivi-Kind`
  header on the rendered item.
- Multi-room local stores: one shared mailspace per project, with the room id
  as each item's remote-binding mailbox, so prune can filter by room.

## Out of scope

- Cloudflare/Durable-Objects port (feasibility explored 2026-10-09: rooms
  map to DOs cleanly; rewrite cost concentrates in the crate's storage
  layer. Separate track if ever taken.)
- WebSocket transport (contract stays HTTP long-poll).
- Multi-instance server horizontal scaling.
