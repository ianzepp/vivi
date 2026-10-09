# vivi-server Wire Contract — Phase 01 (FROZEN at U1)

Wire truth for the vivi chat substrate. Client units (vivi `chat` family)
build against this document; any change after U1 integration is an explicit
recorded revision in the phase delivery doc.

All bodies are JSON (`application/json`). All v1 routes live under `/v1/`.
Storage per room is a real mailspace at `DATA_DIR/rooms/<room_id>/.vivi/`
(R2), opened through the vivi crate; messages are mail items sent
via `Mailspace::send`.

## Conventions

- **Room id**: 25 Crockford-base32 chars (~125 bits), grouped
  `xxxxx-xxxxx-xxxxx-xxxxx-xxxxx`. The id is the capability: knowledge of the
  id plus an open seat admits a joiner. Normalization: `L`→`l`,
  `I`→`i`, `O`→`o` before decoding; `U` is rejected.
- **Identity**: one ED25519 keypair per mailspace, minted client-side at
  `<project>/.vivi/chat/identity.ed25519` (0600). The client presents the
  public key PEM (SPKI). The server stores only the canonical SPKI DER
  fingerprint: lowercase hex of SHA-256 over SPKI DER, full 64-hex chars;
  the short form shown in UIs is its first 16 hex chars.
- **Seat**: slot 1..N (N = room capacity, 2..8). Bound to exactly one public
  key; one `label` (seat display name, unique per room, default
  `<short-host>/<project>`).
- **Token**: seat-scoped bearer, minted by the server, TTL **1 hour**. The
  server persists only `SHA-256(token)` + expiry; the raw token exists
  client-side only until the invocation ends. Sent as
  `Authorization: Bearer <token>`.
- **Cursor**: opaque integer (monotonic mail item id). Polls return
  `next_cursor` for the following request.
- **Message-ID**: client-generated (or server-minted when absent). Sends are
  idempotent by `Message-ID`; replaying one returns the original stored
  record with `"duplicate": true` and never stores twice.

## Origin headers

Client-generated mail carries:

| Header | Value |
| --- | --- |
| `X-Vivi-Chat-From` | sender seat label |
| `X-Vivi-Chat-Fingerprint` | sender key fingerprint (short form, 16 hex) |

The server verifies both match the authenticated seat, rejects otherwise.
Read responses echo them per message. Trust posture is the GitHub-issue
model: origin is labeled, never gated.

## Endpoints

### `GET /healthz` — unauthenticated liveness

`200 {"ok":true}`. Plain, no auth, no storage dependency.

### `GET /v1/_health` — operator health (moderation token)

Requires `Authorization: Bearer <MODERATION_TOKEN>`. Reports mode, room
count, and storage state only. No message content.

### `POST /v1/rooms` — create room

Request: `{"seats": 2..8 (optional, default 4), "label": "..." (optional,
creator seat label)}`.
Response `201`: `{"room_id", "seat": 1, "label", "token", "token_expires_at",
"fingerprint"}`. The creator auto-binds seat 1 and receives a token.
Minting uses a cryptographically random 125-bit id, collision-retried.

### `POST /v1/rooms/:id/challenge` — start proof-of-possession

Request: `{"public_key_pem"}` (join) or `{"fingerprint"}` (token refresh on a
held seat). Response `200`:
`{"challenge_id", "challenge" (32 bytes, base64), "expires_in": 120}`.
Challenge is single-use, server-random, TTL 120 s. Unknown fingerprint for
refresh returns the same shape only if the seat exists; the token step fails
closed.

### `POST /v1/rooms/:id/join` — bind seat (first join or rebind)

Request: `{"challenge_id", "signature_base64", "public_key_pem", "label"
(optional)}`. Server verifies the signature over the exact challenge bytes
(UTF-8 as issued) against the key.

- **First join**: takes the next open seat. Response `201`:
  `{"seat", "label", "token", "token_expires_at", "fingerprint"}`.
- **Rebind** (key already holds a seat): re-signs, seat unchanged. Response
  `200` same shape.
- Room full → `403 seat_full`. Label collision → `409 label_taken`
  (client retries with a distinct label).

### `POST /v1/rooms/:id/auth/token` — refresh bearer (held seat)

Request: `{"challenge_id", "signature_base64"}` (challenge issued against the
bound fingerprint). Response `200`: `{"token", "token_expires_at"}`. Old
tokens stay valid until their own expiry; digests are per-mint records.

### `POST /v1/rooms/:id/messages` — send (bearer)

Auth: seat bearer. Headers: `X-Vivi-Chat-From`, `X-Vivi-Chat-Fingerprint`
(verified against seat), optional `Message-ID` (client-generated).
Request: `{"to": "<seat-label>" (optional, direct), "subject": "..."`
`(optional)`, `"body": "..."}`. Body cap **64 KiB** (UTF-8); over → `413`.
Rate limit **30 sends/min/seat**; over → `429`.
Response `201`: `{"message_id", "cursor", "created_at"}`; replayed
`Message-ID` → `200` same shape plus `"duplicate": true`.
Stored as an ordinary mail item via `Mailspace::send` with the origin
headers; body encrypted at rest under `STORE_ENCRYPTION_KEY`.

### `GET /v1/rooms/:id/messages` — poll / long-poll (bearer)

Auth: seat bearer. Query: `since=<cursor>`, `wait=<seconds 0..25>` (default 0).
Waits up to `wait` for new items, then returns. Response `200`:
`{"messages": [{"message_id", "cursor", "from", "fingerprint", "to",
"subject", "body", "created_at"}], "next_cursor"}`. `next_cursor` is the
highest delivered cursor (or `since` when empty). Polling does not advance
the server-side read state of other seats; each seat polls independently.

## Error envelope

Every non-2xx response: `{"error": {"code", "message"}}`.

| HTTP | Code | Meaning |
| --- | --- | --- |
| 400 | `invalid_request` | malformed JSON, bad seat range, bad query |
| 400 | `invalid_room_id` | id not canonical Crockford-base32 |
| 401 | `unauthorized` | missing/invalid/expired bearer |
| 403 | `moderator_unauthorized` | missing/invalid moderation token on `/v1/_health` (R6) |
| 401 | `challenge_failed` | unknown/expired challenge_id, or bad signature |
| 403 | `seat_full` | no open seat |
| 404 | `room_not_found` | unknown room id |
| 409 | `label_taken` | seat label already bound in room |
| 413 | `body_too_large` | body over 64 KiB |
| 429 | `rate_limited` | over 30 sends/min/seat |
| 500 | `internal` | storage or unexpected failure (no internals leaked) |

## Config (environment)

| Var | Meaning | Default |
| --- | --- | --- |
| `PORT` | listen port | `8080` |
| `DATA_DIR` | room storage root (`DATA_DIR/rooms/<id>/.vivi/`, per R2) | `data` |
| `STORE_ENCRYPTION_KEY` | AES-256-GCM key material for at-rest bodies | required before rooms open (U5 verifier; boot exits on mismatch/unreadable store) |
| `MODERATION_TOKEN` | bearer for `/v1/_health` | unset disables `/v1/_health` |

## Hardening summary (authoritative for U3–U5)

- sqlite files 0600 under the mailspace dir.
- Challenge: single-use, 120 s TTL, 32 random bytes.
- Token: SHA-256 digest only at rest, 1 h TTL, constant-time digest compare.
- Send rate: 30/min/seat; body cap 64 KiB.
- Boot with key mismatch or unreadable store → process exits, never serves.

## Revisions

- **R1 (with VC-2, 2026-10-09): `GET /v1/rooms/:id`.** The U1 freeze omitted
  a room-metadata read. Shape (implemented): `GET /v1/rooms/:id` →
  `200 {"room_id": "<grouped>", "capacity": <2..8>}`; errors `invalid_room_id`
  400, `room_not_found` 404, `internal` 500. Metadata only, no auth in v1;
  seat/token fields remain VC-3. `POST /v1/rooms` returns `{"room_id": …}`
  until VC-3 adds creator seat-1 binding and the creator token to the
  response.

- **R2 (with VC-4, 2026-10-09): storage root + origin representation.**
  The room mailspace root is `DATA_DIR/rooms/<room_id>/.vivi/` (the vivi
  crate's own layout, provisioned via `Mailspace::init` with per-seat-label
  identities); the U1-freeze root `rooms/<id>/mailspace/` is removed by the
  unification unit. Origin labels in v1: the poll response and sidecar
  message index carry seat label + fingerprint, and the mail `From` address
  encodes the seat identity; `X-Vivi-Chat-*` headers *inside* the stored
  mail item are deferred to Phase 02 (the crate's `SendRequest` has no
  custom-header path). Origin mismatch (a send claiming a label the bearer
  seat does not hold) rejects `401 unauthorized`. Accepted v1 limits:
  sidecar index write is not transactional with the mailspace send; the
  rate limiter is in-memory (single-process deployment).

- **R3 (with VC-8, 2026-10-09): canonical fingerprint derivation.** The
  implemented canonical form is `sha256( hex_string( spki_der ) )` — the
  digest runs over the lowercase-hex encoding of the SPKI DER bytes, then
  hex-encoded; short display form is its first 16 hex chars. This amends
  the Conventions wording ("SHA-256 over SPKI DER"); client and server
  agree on this form (vivi `wire_fingerprint`). Known v1 limit: a failed
  creator-binding after room provisioning leaves the room dir behind
  (inert; ids are 125-bit random) — cleanup deferred.

- **R4 (VC-8 integration, 2026-10-09): label charset.** Seat labels double
  as vivi identity local-parts when the room mailspace provisions per-seat
  senders, so labels are constrained to ASCII letters, digits, and
  `.` `-` `_` (plus the existing length and uniqueness rules); anything
  else is `400 invalid_label` at join/create. The client default label is
  `<short-host>.<project-dir>` (sanitized).

- **R5 (with VC-9, 2026-10-09): room-create request shape.** The create
  endpoint requires the creator's `public_key_pem` (plus `seats`, `label`);
  the response carries `room_id`, `seat` (1 for the creator), `label`,
  `token`, `token_expires_at`, `fingerprint`. This makes explicit what R1
  announced; the § `POST /v1/rooms` body text is authoritative here.

- **R6 (with VC-5, 2026-10-09): moderation health auth.** `/v1/_health`
  failures are `403 moderator_unauthorized` (constant-time compare), and
  the route is absent (404) when `MODERATION_TOKEN` is unset. Message
  content is sealed at rest under `STORE_ENCRYPTION_KEY` (AES-256-GCM,
  per-record nonces; sealed canary verifier; boot exits on missing or
  mismatched key once a store is sealed; fresh keyless boot warns and
  refuses message ops). Railway deploys must attach a volume at `/data`
  (`railway volume add --mount-path /data`) — the schema cannot declare
  it.
