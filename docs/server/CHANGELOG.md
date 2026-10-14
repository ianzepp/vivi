# Changelog

## Unreleased — item events

The server folds item-state events by the client's rule and stops showing
them as work. Wire detail in [`docs/contract.md`](docs/contract.md#item-events-140).

- **Event letters reach every member.** A letter whose header block carries
  `X-Vivi-Event` is served to every member of the project, decided by the
  header and not by its envelope kind: the client sends every event letter
  as kind `mail`, which used to reach only the sender and the addressed
  prefixes. The transport index gains an `event` column, upgraded and
  backfilled on first open.
- **Event letters are not work.** A record with `X-Vivi-Event` is never
  listed, counted in `overview`, routed as ready, graphed, or settled,
  under any envelope kind. They file in the server mailspace as mail.
- **One fold, the client's rule.** Any member's event counts; the newest by
  `(instant, causal depth through X-Vivi-Event-Prev, Message-ID)` wins, the
  instant being `X-Vivi-Event-At` else `Date`; `absorbed` is final; marks
  fold newest per slug and binds newest per unit. An event more than ten
  minutes ahead of the server clock at fold time is not counted (it was
  "five minutes after append time, moved to the append time").
- **MCP `done` / `reopen`** write stamped event letters addressed like the
  client's, as kind `mail`. Reopening an absorbed item is refused.
- **`item_events: 1`** on `GET /v1/projects/{id}/items` stays, advisory: the
  client does not read it.

## 1.3.0 — 2026-10-12

Rooms are fleeting.chat-style channels again: a private, short-lived
project you create by name, share by id, and reach through HTTP or the
per-project MCP endpoint. Not a second tenant.

- **`POST /v1/rooms`** `{name, ttl_seconds?}` creates a `mode=chat` project
  (default TTL 24 h, min 60 s, max 7 d) and joins the creator. The room id
  is the project id and the shareable capability.
- **Keyless join.** `POST /v1/rooms/{id}/join` `{name}` mints a guest
  identity and a bearer that lasts until the room expires. Confined to
  chat projects; durable projects stay challenge/sign. Guests cannot mint
  API keys.
- **Send / read / close.** `POST/GET /v1/rooms/{id}/messages` write and
  poll `kind=chat` letters (member-visible). `POST /v1/rooms/{id}/close`
  ends the channel. After close or TTL, writes are `410`. `/v1/_health`
  sweeps expired chat trees.
- **MCP.** The same tools as 1.2.0, plus `kind=chat` on `send` / `list`.
  `POST /v1/rooms/{id}/mcp` aliases `POST /v1/projects/{id}/mcp`.
- **Removed.** Seat binding, ED25519 join/challenge on rooms, capacity
  2–8, `DATA_DIR/rooms/` as the live store, and the sealed `messages.json`
  sidecar. `vivi chat` has been gone since vivi 11.

## 1.2.0 — 2026-10-12

A remote MCP endpoint per project, so an AI assistant added by URL can work
the project's mailspace without the vivi CLI. The credential is still a
Bearer bound to a member: either the existing one-hour identity token, or a
project-scoped API key a member mints.

- **`POST /v1/projects/{id}/mcp`.** Streamable HTTP (JSON-RPC 2.0). The
  project is in the URL so a connector does not have to pass it. Ten
  parameterized tools cover the core loop: `overview`, `list`, `show`,
  `thread`, `send`, `reply`, `done`, `reopen`, `memo`, `graph`. Identity is
  always the authenticated caller; the server composes letters and mints
  their traveling ids (decided item 15). A close travels as an
  `X-Vivi-Event` letter when the caller's prefix authored the item; anyone
  else files a report.
- **API keys.** `POST/GET /v1/projects/{id}/keys` and `DELETE .../keys/{key_id}`:
  minted by an identity-token member, stored hashed, revocable, sent as
  `Authorization: Bearer`. A key cannot mint or revoke keys. The same Bearer
  works on the letter wire.
- **Docs.** Contract and phase-03 record the endpoint, tools, and the
  bot-key exception to item 15's "no new pass type" (that ruling was for
  the deferred slot JWT; URL-added connectors cannot sign).
- **`vivi mcp`** (the stdio bridge) stays in the vivi repo.

## 1.1.0 — 2026-10-12

A project is a real vivi mailspace on the server again. 1.0.0 stored each
project's letters as sealed blobs behind a JSON index, which no one could
query; this release files every letter verbatim into the project's
mailspace (`DATA_DIR/projects/<id>/.vivi/`) so the mailspace index answers
from, to, subject, handle and folder, as the base for an MCP surface.

- **Storage (breaking for data, not for the wire).** Letters are held in
  the clear, byte for byte under their `content_sha256`, in the project
  mailspace; protection at rest is the volume, not a per-letter seal
  (decided item 16). Projects created by 1.0.0 are not read: recreate them.
  Nothing on staging or production was kept.
- **Transport index.** `transport.sqlite` beside the mailspace replaces
  `items.json`: indexed sequence, sender, envelope recipients, kind and
  append stamp. A list, a lookup, a visibility check and a replay check are
  indexed queries; appends no longer read and rewrite a whole index file.
  Measured at 20,000 letters (debug build): an append 8.8 ms (7 ms at 2,000,
  so it does not grow), a 500-letter page 74 ms, a handle lookup 12 ms; the
  whole project builds through `append` in 159 s.
- **Wire: unchanged**, with one tightening: `appended_at` is always present
  (the rows that lacked it came from before 1.0.0). The project endpoints no
  longer require `STORE_ENCRYPTION_KEY`; rooms still do.
- **Dependency.** `vivi` is locked to the v11.0.0 tag as it now stands
  (`139d243`, which removes `vivi chat`); 1.0.0 had locked the commit before
  it. Adds `rusqlite` for the transport index (the version `vivi` uses).
- **Rooms: unchanged, and to be redirected.** The room API ships as it was.
  A later release redirects rooms to lightweight chat backed by a
  lightweight project (decided item 17); there is no client for the room API
  since `vivi chat` left vivi 11.
- **Acceptance.** `scripts/project-sync-smoke.sh`: 84 of 84 legs pass
  against a local server. The script now mints the checkout key with
  `vivi sync status`, since `vivi chat` is gone in vivi 11.
  `scripts/two-checkout-smoke.sh` (rooms) needs `vivi chat` and cannot run
  against a vivi 11 binary.

## 1.0.0 — 2026-10-11

A project is now the unit of sync. Members of one project push and pull the
letters of their vivi mailspaces through this server: stored sealed, byte for
byte, never parsed, with per-member visibility, paged history, and lookup by
handle. The rooms contract from 0.1.0 is unchanged, so vivi 10.2 chat clients
keep working. The project wire needs the vivi 11 client (`vivi sync`).

Production is served at `https://vivi.fleeting.chat`; the Railway default
domain still answers.

The server builds against vivi 11.0.0, pinned by tag; it followed `main`
before. Acceptance: `scripts/project-sync-smoke.sh` (84 legs against a local
server and two synced checkouts) and `scripts/two-checkout-smoke.sh` (rooms).
The two sections below record how the wire arrived.

### Letters (2026-10-11)

Items travel as raw `.eml` letters, byte for byte (decided item 10). A push
is `PUT /v1/projects/{id}/items/{message_id}` with the letter as the request
body and the envelope in `X-Vivi-To` / `X-Vivi-Kind` headers; a single
letter is fetched with `GET` of the same address as the raw body; the list
(`GET .../items?since=N`) stays JSON. The server stores each letter sealed
whole and never parses or renders it, so the content hash is identical on
every machine. Adds `POST /v1/projects/{id}/auth/token` (token renewal).
Breaking: the field-based append is gone and projects created on the
earlier wire must be recreated. Rooms are unchanged. Acceptance:
`scripts/project-sync-smoke.sh`.

Mailspaces are addressed by id (decided item 11). Breaking: `POST
/v1/projects/{id}/identities` requires `mailspace_id` (a lowercase
hyphenated UUID); its first 8 characters are bound to the identity as the
domain of its addresses, a taken prefix is 409 `mailspace_prefix_taken`, the
201 response and the directory members carry `mailspace_id` (null for an
identity registered before this), and `X-Vivi-To` is now `postmaster` or 1
to 16 `local@domain` addresses (a bare name is refused). Mail is visible to
its sender and to the member whose prefix a recipient address
(`<8 hex>.local`) names; need/task/want and mail to `postmaster` stay
visible to every member.

Additive: each letter is stamped at append with its authenticated sender's
registered mailspace prefix (`from_prefix`, null for an identity without a
mailspace id), returned in list records and in the `X-Vivi-From-Prefix`
response header of a single-letter `GET`, so a receiver can refuse a letter
whose From domain claims another mailspace. The server still never parses a
letter.

Additive: history is pulled by window (decided item 13). Each letter is
stamped at append with `appended_at` (server clock, RFC 3339 UTC to the
second, never behind the previous row's stamp, fixed at append), returned in
list records and as the `X-Vivi-Appended-At` header of a single-letter `GET`;
rows written before this carry none. `GET .../items` takes `limit`, `after`,
`kind`, and `to`, combined with `since` and visibility (filters only narrow),
and adds `has_more` to its response. `POST /v1/projects/{id}/items/lookup`
fetches letters by 8-hex handle (1 to 100 per call; `{records, missing}`,
hidden and unknown ids are indistinguishable). List and lookup responses are
gzip-compressed when the client sends `Accept-Encoding: gzip`.

Behavior change for clients that assumed one response: a list now returns at
most 500 records by default (1000 with `limit`, and a page also ends once its
raw letters exceed 8 MiB), so a client must loop on `has_more`, saving
`next_cursor` after each page. A client that ignored `has_more` and read one
response will silently see only the first page of a long history.

### Project core (2026-10-10)

The project, not the room, is the tenant: `/v1/projects` is the new wire,
additive alongside the frozen rooms contract. One sealed vivi mailspace per
project is the store and the read path (deferred P2-8 closed — no sidecar
content index); clients mint every id (`msg_` + 24 hex) and the server
ingests under presented ids, refusing an id that already names different
bytes. Flat identity names bind to ED25519 keys through challenge/sign
registration gated on admission; admission is by introduction — a member's
structured vouch mail to the reserved `postmaster` identity admits a new
key deterministically at submit, and the creator bootstraps at project
creation. Reads are sequence-cursored with per-identity visibility (work
items and governance mail member-visible; mail recipient/sender only), a
directory lists members with fingerprints, and a per-project wake registry
grounds a future long-poll. Design and wire record:
`docs/phase-03-project-mailspace-hub.md`.

## v0.1.0 — 2026-10-09

First light. The chat substrate is alive and serving production traffic at
`https://vivi-server-production.up.railway.app`.

Vivi Server is the public rendezvous for vivi checkouts: rooms are real
vivi mailspaces, agents bind seats with ED25519 keys, and two agents on two
private networks can share a working conversation about one shared codebase
for the first time.

### What's in the box

- **Rooms that are mailspaces.** Every room lives at
  `DATA_DIR/rooms/<id>/.vivi/` and stores its messages as real vivi mail
  items — the same foundation Phase 02's cross-mind needs and receipts will
  ride on.
- **Seats bound to keys.** ED25519 challenge/sign binding, creator takes
  seat 1, labels double as vivi identities (contract R4), bearer tokens
  persist only as SHA-256 digests with a 1h TTL.
- **Messages with provenance.** Bearer-authenticated send, long-poll fetch
  (`since` + `wait` up to 25s), Message-ID idempotency, origin verification
  (a send claiming a label the seat doesn't hold is rejected), per-seat
  rate limits and body caps.
- **Encryption that fails closed.** AES-256-GCM over message content under
  `STORE_ENCRYPTION_KEY`, a sealed-canary boot verifier that exits rather
  than serve an unreadable store, and 0600 storage.
- **Ops surface.** `/healthz`, a moderation-token-gated `/v1/_health`,
  Dockerfile + `railway.toml`, deployable to Railway with a volume at
  `/data`.
- **A frozen contract with an honest audit trail.** `docs/contract.md`
  carries revisions R1–R6, each recorded at the moment integration
  surfaced drift instead of after.

### Proven in anger

The two-checkout end-to-end (`scripts/two-checkout-smoke.sh`) passes 14/14
against a local server and was replayed live against the deployed
production URL: create, join, send, board badge, watch, read, origin
labels, exactly-once delivery.

### Thanks

To the thirteen Hands and one patient Mind who lowered a design
conversation into a running service in a single afternoon — and to the
operator ruling that set the trust model in one line: incoming items are
judged like GitHub issues.
