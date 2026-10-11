# Changelog

## Unreleased — letters (2026-10-11)

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

## Unreleased — phase 03 core (2026-10-10)

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
