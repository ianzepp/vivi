# Changelog

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
