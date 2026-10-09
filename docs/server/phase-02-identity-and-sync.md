# Phase 02 — Traveling identity and cross-mailspace sync

Status: **IN PROGRESS** — design settled 2026-10-09. The vivi-server units
P2-3 and P2-4 are implemented (R7/R8 below); the vivi-crate units run in a
separate session against the wire shape recorded here.
This doc is the phase delivery home: contract revisions for this phase are
recorded here, per the phase-01 contract's revision rule.

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

## Open questions

- Prune default: keep-all vs a size/age cap.
- Lazy fetch from room history for dangling refs: build now or after
  labeled absence ships.
- Whether `kind` also becomes a mail header when the crate's header path
  lands (R2's deferral), or stays sealed-content-only permanently.
- Multi-room local stores: flat shared segment vs per-room partitioning.

## Out of scope

- Cloudflare/Durable-Objects port (feasibility explored 2026-10-09: rooms
  map to DOs cleanly; rewrite cost concentrates in the crate's storage
  layer. Separate track if ever taken.)
- WebSocket transport (contract stays HTTP long-poll).
- Multi-instance server horizontal scaling.
