# vivi-server Wire Contract

Wire truth for vivi-server. Phase 01 rooms (U1, seat/challenge) are
**superseded in 1.3.0** by [Lightweight chat rooms](#lightweight-chat-rooms-130).
The project letter wire and [Project MCP](#project-mcp-120) stay. Revisions
after a freeze are recorded here.

All bodies are JSON (`application/json`) unless a route says otherwise.
All v1 routes live under `/v1/`.

## Lightweight chat rooms (1.3.0)

A room is a **chat-mode project**: same mailspace and transport index as a
durable project (`DATA_DIR/projects/<id>/`), with `mode=chat`, a TTL, and
keyless admission. The room id is the project id (25 Crockford-base32,
grouped `xxxxx-xxxxx-xxxxx-xxxxx-xxxxx`) and the shareable capability.

Chat letters use `kind=chat` and are member-visible (the same transport
rule as need/task/want). Ordinary `kind=mail` stays sender/recipient-only.

### Auth

No key. `POST /v1/rooms` and `POST /v1/rooms/{id}/join` take `{name}`
(1–64 chars, mail local-part charset). The server mints a guest identity
(mailspace UUID + prefix) and a Bearer that lasts until the room expires.
Stored hashed. Confined to chat projects. A guest cannot mint API keys.
Durable projects keep challenge/sign.

Member cap **64**. Rate limit **30 sends/min/identity** by default (a deployment may set `SEND_RATE_PER_MINUTE`; `0` is no limit). Body cap **64 KiB**.

### TTL

Default **24 h**, min **60 s**, max **7 d**. Any member may close the
room. After close or expiry: join, send, and MCP are `410`
(`room_closed` / `room_expired`). GET meta still reports `closed` /
`expired` until `/v1/_health` sweeps the tree. Named exception to
phase-03 item 4: a chat project's letters are not retained past expiry.

### Endpoints

#### `POST /v1/rooms`

Request: `{"name", "ttl_seconds"?}`. Response `201`:
`{"room_id", "project_id", "mode":"chat", "ttl_seconds", "expires_at",
"closed", "expired", "members", "mcp", "name", "token",
"token_expires_at"}`. `project_id` equals `room_id`. `mcp` is
`/v1/projects/{id}/mcp`.

#### `POST /v1/rooms/{id}/join`

Request: `{"name"}`. Response `201`: same shape as create (token for the
joiner). `409 name_taken`. `403 room_full`. `410` if over.

#### `GET /v1/rooms/{id}`

No auth. `200` metadata (no message content). `400 invalid_room_id`.
`404 room_not_found`.

#### `POST /v1/rooms/{id}/messages`

Bearer. Request: `{"body", "subject"?, "in_reply_to"?}`. Subject
defaults to `chat`. Response `201`:
`{"message_id", "handle", "sequence", "kind":"chat", "duplicate"}`.

#### `GET /v1/rooms/{id}/messages`

Bearer. Query: `since=<sequence>`, `wait=<seconds 0..25>`. Response `200`:
`{"messages":[{"message_id","handle","sequence","from","subject","body",
"created_at"}], "next_cursor"}`. Only `kind=chat`.

#### `POST /v1/rooms/{id}/close`

Bearer. Response `200` metadata with `closed: true`.

#### `POST /v1/rooms/{id}/mcp`

Same Streamable HTTP MCP as `POST /v1/projects/{id}/mcp`. `send` /
`list` accept `kind=chat`; `to` is optional on chat send.

### Errors (additive)

| HTTP | Code | Meaning |
| --- | --- | --- |
| 403 | `room_full` | 64 members already |
| 409 | `name_taken` | name already in this chat |
| 410 | `room_expired` | TTL elapsed |
| 410 | `room_closed` | a member closed the chat |

---

## Phase 01 rooms (superseded)

The remainder of this section is the frozen U1 seat/challenge contract,
kept as the revision record. It is not served in 1.3.0.

Storage per room *was* a mailspace at `DATA_DIR/rooms/<room_id>/.vivi/`
(R2), opened through the vivi crate; messages were mail items sent
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
Rate limit **30 sends/min/seat** by default (configurable per deployment, `0` = none); over → `429`.
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
| 429 | `rate_limited` | over N sends/min/identity (N is the configured limit, default 30) |
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
- Send rate: 30/min/seat by default, configurable per deployment (`SEND_RATE_PER_MINUTE`, `0` = none); body cap 64 KiB.
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

- **R7, R8 (phase 02):** additive send/poll fields (`kind`, `in_reply_to`,
  `refs`) and the traveling-id rule for kinded sends. Recorded in
  [`phase-02-identity-and-sync.md`](phase-02-identity-and-sync.md), the
  phase 02 delivery home.

## Project MCP (1.2.0)

Additive alongside the rooms contract and the project letter wire in
[`phase-03-project-mailspace-hub.md`](phase-03-project-mailspace-hub.md).
One Streamable HTTP MCP endpoint **per project**: the URL is the tenant,
the Bearer is the caller.

### `POST /v1/projects/{id}/mcp` — Streamable HTTP MCP (bearer)

JSON-RPC 2.0. `Content-Type: application/json`. Response is
`application/json` unless `Accept` is only `text/event-stream`, in which
case the same JSON-RPC object is wrapped as one `event: message` SSE
frame. Notifications (`notifications/*`) return `202` with an empty body.
Supported protocol versions: `2024-11-05`, `2025-03-26`, `2025-11-25`.

Auth: `Authorization: Bearer` — a live identity token **or** a live
project API key. Both resolve to the minting / registered identity. A
missing or bad bearer is `401 unauthorized`. An unknown project is
`404 project_not_found`. Method errors are JSON-RPC (`-32601`, …), not
HTTP 4xx.

Methods: `initialize`, `ping`, `tools/list`, `tools/call`. The tool list:

| Tool | Arguments | Effect |
| --- | --- | --- |
| `overview` | — | Caller identity, open counts, ready handles, recent items |
| `list` | `kind?` need\|task\|want\|mail\|memo, `status?` open\|done\|all, `limit?` | Summaries. Default: open work |
| `show` | `handle` | One letter or memo |
| `thread` | `handle`, `limit?` | Reply thread |
| `send` | `kind`, `to`, `subject`, `body`, `depends_on?` | Server composes and mints the id; `From` is the caller |
| `reply` | `handle`, `body` | Reply as the caller |
| `done` | `handle`, `note?` | Stamped event letter, From the caller's `operator` role, if the caller's mailspace authored the item; else a report |
| `reopen` | `handle`, `note?` | Same authority as `done`; an absorbed item is refused |
| `memo` | `action` list\|show\|save, `handle?`, `subject?`, `body?` | Server-local memos (do not sync) |
| `graph` | `view?` ready\|status | Ready / blocked / done from `X-Vivi-Depends-On` and events |

Writes share the per-identity send budget (30/min by default; a deployment may configure it, `0` = none), and a refusal reads `over N writes/min/identity`. Tool failures are
`isError: true` with a text body, HTTP 200. `kind=chat` is accepted on
`send` and `list` (1.3.0); `to` may be omitted on a chat send. A closed
or expired chat project is `410` before JSON-RPC.

### API keys

Minting and revocation require an **identity token** (proof of
possession), not another API key.

- `POST /v1/projects/{id}/keys` `{label}` (1–64 chars) → `201`
  `{key_id, key, label, created_by, created_at}`. The raw `key` is
  returned once (`vivi_` + random). At most 32 live keys per project.
- `GET /v1/projects/{id}/keys` (bearer) → `{keys: [{key_id, label,
  created_by, created_at, revoked_at?}]}`. No secrets.
- `DELETE /v1/projects/{id}/keys/{key_id}` (identity token of the
  minting member) → `204`. Unknown or someone else's key is
  `404 key_not_found`.

Keys are stored as SHA-256 digests, 0600, next to the project. A revoked
or unknown key is the same `401` as a bad identity token. A live key is
accepted on every project bearer route, including the letter wire.

## Item events (1.4.0)

Closing, reopening, absorbing, marking and binding a need, task or want
travel as **event letters**: small replies carrying `X-Vivi-Event`. The
ruling the server folds by: *when two machines disagree about a need state,
the last update or event created against that need is controlling.*

### Version gate: `item_events`

`GET /v1/projects/{id}/items` carries a top-level `item_events` integer
beside `records`, `next_cursor` and `has_more`, on **every** response —
an empty page past the end of the log included, so `?since=<cursor>&limit=1`
reads it. `1` means the server understands event letters as below. It is
advisory: the current client sends every event letter with envelope kind
`mail`, which a server that predates this files as plain mail, and does not
read the field.

### On the wire

An event letter is any letter whose header block carries `X-Vivi-Event`,
whatever kind its envelope names. The client sends one as kind `mail`
(`X-Vivi-Kind: mail`, no `X-Vivi-Kind` header on the envelope), so an old
client or server files it as mail and never as work. An older client sent it
under the item's own kind; that still works and is folded the same way.

- **Visible to every member.** A letter carrying `X-Vivi-Event` is served
  to every member of the project, not only its sender and the prefixes it is
  addressed to: visibility is decided by the header, not by the kind, so a
  member that holds the item learns of its events. Only the header name
  counts (not `X-Vivi-Event-Of`, not the text in a body). The marker is
  recorded on the transport row at append; an index written before it is
  upgraded on first open. An event letter's body and addresses are therefore
  member-visible: keep private text out of one.

### What the server does with an event letter

- **Never work.** A record carrying `X-Vivi-Event` is not listed, counted in
  `overview`, routed as `ready`, a `graph` node, or settled by `done` /
  `reopen`, whatever envelope kind it travels under. `show` and `thread`
  report it as `kind: "event"`, `status: "event"`. Server-side it files in
  the project mailspace as mail, never under `needs`/`tasks`/`wants`.
- **Who counts.** Any member's event. The letter must name its item both as
  `In-Reply-To` and `X-Vivi-Event-Of`, and its `From` domain must be the
  `<prefix>.local` the server stamped for the sender at append (a bare
  `local`, a legacy name domain, or a prefix the sender does not hold never
  counts). Kinds: `done`, `reopened`, `absorbed` (`X-Vivi-Event-State:
  done|open`), `marked`/`unmarked` (`X-Vivi-Event-Slug`), `bound`
  (`X-Vivi-Event-Unit`, repeatable). An unknown kind, or a kind missing its
  payload, is skipped. A client also writes `dependency-added` /
  `dependency-removed` (`X-Vivi-Event-Dep`, repeatable) to change an item's
  prerequisites after the send; this server files those letters and serves
  them to every member as event letters, and does not fold them into its own
  `graph` projection, which reads `X-Vivi-Depends-On` headers only. A hosted
  project's `graph` folds them, through the library. The pump identity's
  events count by the same rule: its letter is From `<pump identity>@<prefix>.local`
  where the prefix is the pumping member's stamped prefix (the host's own on a
  hosted project), never by role name.
- **Order.** The key is `(instant, causal depth, Message-ID)`, compared
  lexicographically; the largest is newest. The instant is the event's
  `X-Vivi-Event-At` (RFC 3339, microseconds), else its `Date` in whole
  seconds — never `appended_at`. The causal depth of a `done`/`reopened`
  event is 0 when no counted state event is named in its
  `X-Vivi-Event-Prev`, else one more than the deepest one that is. A later
  instant beats a deeper chain.
- **Future bound.** An event whose instant is more than ten minutes ahead
  of the server clock *at fold time* is not counted at all, so a far-future
  stamp cannot lock an item. The test is made each time the letters are
  read, not at append: the same letters count once the clock catches up,
  as they do on a client.
- **Absorbed is final.** The earliest `absorbed` event (by instant, then
  `Message-ID`) names the state; later events of any kind move nothing, and
  a sealed item takes no later marks or binds.
- **Marks and binds.** Per slug, the newest `marked`/`unmarked` decides, by
  `(instant, Message-ID)`: the client reads no `X-Vivi-Event-Prev` on a
  property event, so its depth is always 0 and this is the same key. Per
  unit, the newest `bound` decides which item it belongs to, by the same
  key. Both are shown on a work item's summary (`marks`, `units`) when
  present, and `absorbed: true` on a sealed one.

- **Every letter, not a page.** The MCP tools fold the event letters of
  *every* letter the caller may see, however many: the index is walked by
  cursor in chunks of 500 until it is exhausted, never the first page a
  list serves (500 letters, 8 MiB). `list` (work kinds), `show` and the
  settle path of `done`/`reopen` read only work items and event letters;
  `overview`, `graph`, `thread` and `list kind=mail` also read the caller's
  plain mail. A project's events are folded as they are read and dropped, so
  memory holds the listed letters and not the events folded into them.

### MCP `done` / `reopen`

The authoring mailspace (the caller's prefix is the item's `From` domain)
writes an event letter the way the client does: envelope kind `mail`,
`X-Vivi-Kind: mail`, `X-Vivi-Event`, `X-Vivi-Event-Of`, `In-Reply-To`,
`X-Vivi-Event-At` (the clock, never at or below the newest counted
`done`/`reopened` event of the item plus one microsecond; the clock again if
that stamp would not parse back), one `X-Vivi-Event-Prev` per head, addressed
to the item's `From` first and then its `To`/`Cc` minus the sender.

**MCP state changes act as the member's operator role.** The event is
written `From: operator@<prefix>.local`, never `<name>@<prefix>.local`. The
member that owns the prefix counts an own-domain event only when its local
part is the item's author or a role of kind `mind` or `operator` in that
mailspace; a registration name is none of those, so a letter From the name
would be counted by the server and by every other member and ignored by the
member that holds the item. `operator` is the conventional role every
mailspace has. The caller's prefix must still own the item (above); `send`,
`reply`, `memo` and a report reply stay From the caller's name.

Every member is served the event by its header. Any other caller files a
plain report reply and writes no event. `reopen` of an absorbed item is an error; so is
`done` of an item absorbed open.

## Pump health (1.6.0; body follows vivi 21)

### `GET /v1/projects/{id}/pump/health` — hosted pump verdict (bearer)

Read-only. Any authenticated member of the project: an identity token or an
API key. Query, both optional, whole seconds: `since` (a previous answer's
`as_of`) and `stale_after` (override the stale threshold). Any other key, or a
non-integer value, is `400 invalid_request`. A project the server owns end to
end has no pump: `404 not_hosted`.

`200` body (`vivi::pump::health::PumpHealth`):

| Field | Meaning |
| --- | --- |
| `verdict` | `ok`, `degraded`, `stalled` or `unknown` |
| `reasons` | One line per finding; empty for `ok` |
| `as_of` | Epoch seconds the answer was judged at; pass it back as `since` |
| `control`, `shutdown_requested` | The pump's stored control mode |
| `writer` | `{state: running\|not_running\|unknown, source: proc_locks\|tick_freshness\|none, pid}` |
| `tick_secs`, `stale_after_secs` | Configured tick and the threshold used (default `max(30 × tick, 600)`) |
| `last_tick` | Newest tick receipt (`id`, `started_at`, `ended_at`, `duration_secs`, `outcome`, `age_secs`), or `null` |
| `status_age_secs` | Age of `status.txt`, or `null` |
| `schema` | The `pump.db` schema this server reads |
| `exceptions` | `{at, kind, unit, text}` at or after `since`, else in the last hour; fifty at most, newest kept |
| `changed_since` | Only with `since`: `{since, exceptions}`, the count of exception rows strictly after `since` |

Verdict rules, first match wins:

1. `unknown`: no `[pump]` table, no `pump.db`, or a schema this server does
   not read (the reason names it).
2. `stalled`: control `running` and either the writer runs with no tick inside
   `stale_after`, or there is no writer and `status.txt` is older than
   `stale_after`.
3. `degraded`: control paused, stopped or unreadable; writer state unknown;
   or no writer but a fresh `status.txt` while control says running.
4. `ok`.

Not-ready fold, applied after the rules above (vivi 21, `add_not_ready`): a
project missing config the pump needs adds a `not ready: ...` reason. If it
never ticked, the verdict is `unknown` and that reason replaces the others. If
it did tick, the verdict stays as judged, the reason comes first, and `ok` is
lowered to `degraded`.

MCP `pump` answers the same JSON with the same two arguments.
