# vivi-server Wire Contract

Wire truth for vivi-server. Phase 01 rooms (U1, seat/challenge) are
**superseded in 1.3.0** by [Lightweight chat rooms](#lightweight-chat-rooms-130).
The project letter wire stays; [Project MCP](#project-mcp-120) is
**superseded in wave 3** by [Server MCP](#server-mcp-wave-3). Revisions
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
`/v1/rooms/{id}/mcp`.

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

The room's own Streamable HTTP MCP, unchanged by the server endpoint:
the URL names the room, the tools take no `project` argument, and the
Bearer is an identity token or a per-project key (the mirror a
self-service server key leaves behind). `send` / `list` accept
`kind=chat`; `to` is optional on chat send.

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

## Project letter append

Durable member push is `PUT /v1/projects/{id}/items/{message_id}` on a
project the server stores. The body is the raw letter. A hosted project
files through the host and is not covered here. The rest of the wire is
in [`phase-03-project-mailspace-hub.md`](phase-03-project-mailspace-hub.md)
under Append.

### Letter format (v1)

A v1 letter is TOML front matter between two `+++` fences, then the
Markdown prose:

```
+++
done_when = ["the smoke passes"]
target = "repo"
+++

Prose follows the front matter.
```

Every letter the server composes carries `X-Vivi-Format: 1`: the need,
want, task, mail and chat letters of MCP `send` and `reply` (hosted and
durable; prose mail included), the pump record a hosted project's
`vivi pump send` files, the `done`/`reopen` event letters, and memos.
Content facts ride front matter, never headers: the close metadata of a
`done` event (`verdict`, `branch` and `commit` together, a `repos` map
of repo name to tip) is the event letter's front matter, and memo
`tags` are the memo's front matter (a memo with no tags stores its
prose bare, with no `+++` fence). No server letter carries the headers
W3-B4 moved (`X-Vivi-Verdict`, `-Repo`, `-Branch`, `-Commit`, `-Tag`,
`-Rule-Scope`, `-Supersedes`); a format-1 letter that carries one is
refused, composed or appended.

On a work send (`kind` need, task or want) the `send` tool takes vivi's
sugar arguments and renders them into the front matter: `done_when`,
`target`, `write_scope`, `serves`, `explores`, `question`, `depends_on`
(strings or lists of strings). A body that opens with `+++` is the whole
letter: the sugar arguments are refused beside it — name those keys in
the front matter instead. `mail` and `chat` take no sugar arguments.

A letter that carries `X-Vivi-Format: 1` must parse as a v1 letter. A
letter that claims format 1 and fails to parse, or names an unknown
format, is refused `422` `letter_format`. The body is
`{"error": {"code": "letter_format", "message": "<the parser's message>"}}`
and nothing is stored. A letter with no `X-Vivi-Format` header is
legacy and is refused the same way, with the message `the server
accepts only format-1 letters: this one has no X-Vivi-Format header`;
a legacy letter already stored stays filed and contributes no
dependencies to `graph`. A replay of an already-stored letter keeps its
duplicate or conflict outcome and is not judged again.

## Server MCP (wave 3)

One Streamable HTTP MCP endpoint for the whole server, `POST /v1/mcp`.
It supersedes the per-project endpoint of 1.2.0: `POST
/v1/projects/{id}/mcp` and `POST /v1/host/mcp` are gone — both answer
`404`. Chat rooms keep their own `POST /v1/rooms/{id}/mcp`
([above](#post-v1roomsidmcp)). Additive alongside the rooms contract and
the project letter wire in
[`phase-03-project-mailspace-hub.md`](phase-03-project-mailspace-hub.md).

### `POST /v1/mcp` — Streamable HTTP MCP (server key)

JSON-RPC 2.0. `Content-Type: application/json`. Response is
`application/json` unless `Accept` is only `text/event-stream`, in which
case the same JSON-RPC object is wrapped as one `event: message` SSE
frame. Notifications (`notifications/*`) return `202` with an empty body.
Supported protocol versions: `2024-11-05`, `2025-03-26`, `2025-11-25`.
Method errors are JSON-RPC (`-32601`, …), not HTTP 4xx.

Auth: `Authorization: Bearer`, one of:

- **A server key** (`vivi_` + random) from the one key store at
  `DATA_DIR/keys.json`. It resolves to its principal and that
  principal's grants. Keys are SHA-256 digests at rest, 0600; the raw
  secret exists once, at mint.
- **The host operator token** (`HOST_OPERATOR_TOKEN`): the built-in
  host principal `host` (constant-time compare), which reaches only the
  host tools. With the variable unset the principal does not exist and
  only server keys authenticate.
- Anything else is `401 unauthorized`. An identity token is told
  `identity tokens are not accepted on /v1/mcp; this endpoint needs a
  server key`; an unknown `vivi_` secret gets the generic invalid
  bearer. A missing bearer is `401` before that.

Methods: `initialize`, `ping`, `tools/list`, `tools/call`.

### The `project` argument

Every project tool takes a required `project` argument: a project id,
or the name of a hosted project the key is granted on (the root
directory name, else the mailspace config `name`; durable and chat
projects resolve by id only). Resolution is strict among the key's own
grants — an exact project id first, else a name match — and the tool
always dispatches on the resolved grant's project id, never on the raw
argument. A missing or non-string `project` is refused `project is
required`. An unknown project and a project the key holds no grant on
are the same refusal, so a key cannot probe which projects exist; a
name that two of the key's grants resolve to is refused. The grant's
member must still be a registered identity on the project, and a closed
or expired chat project reads as ungranted.

### Tools

The project tools are the 1.2.0 list plus the hosted additions
(`depend`, `pump`, `pump_control`, `pump_rungs`, `pool_bands`,
`config_migrate`, `config_set`, `config_unset`, `pool_allow_set`,
`target_add`, `target_remove`, `role_add`, `role_set`, `charter_set`,
`pump_action`, `pump_reset`, `pump_send`, `pump_list`, `pump_show`, and
the reads `pump_log`, `flow_show`, `config_show`, `config_validate`,
`pool_list`, `pool_show`, `role_list`, `role_show`, `charter_show`,
`target_list`); each also takes the required `project` argument on top
of the arguments shown.

| Tool | Arguments | Effect |
| --- | --- | --- |
| `overview` | — | Caller identity, open counts, ready handles, recent items |
| `list` | `kind?` need\|task\|want\|mail\|memo, `status?` open\|done\|all, `limit?` | Summaries. Default: open work |
| `show` | `handle` | One letter or memo |
| `thread` | `handle`, `limit?` | Reply thread |
| `send` | `kind`, `to`, `subject`, `body`, vivi sugar arguments | Server composes and mints the id; `From` is the caller (see [Letter format](#letter-format-v1)) |
| `reply` | `handle`, `body` | Reply as the caller |
| `done` | `handle`, `note?`, close metadata `verdict?`, `branch?`+`commit?`, `repos?` | Stamped event letter, From the caller's `operator` role, if the caller's mailspace authored the item; else a report. Close metadata rides the event letter's front matter |
| `reopen` | `handle`, `note?` | Same authority as `done`; an absorbed item is refused |
| `memo` | `action` list\|show\|save, `handle?`, `subject?`, `body?`, `tags?` | Server-local memos (do not sync); `tags` ride the memo's front matter |
| `graph` | `view?` ready\|status | Ready / blocked / done from `depends_on` in the letter's front matter and from events. A legacy letter (no `X-Vivi-Format`) contributes no dependencies |
| `projects` | — | List what this key may reach |

`projects` answers one `{project_id, name?, mode, member, identity,
level}` per live grant (`mode` is `hosted`, `chat` or `durable`; `name`
is the hosted project's name; `identity` the vivi identity the key acts
as, when one was named). A host principal answers `{host: true}`.

#### Config, pool and target writes

Hosted projects only, and only for a key that passes the operator rule
(the host's own member, acting as a role of kind `operator`). Each write
calls the same library function as the CLI and goes through the config
write guard: refused while that project's pump runs (lock held and mode
running; a paused pump is writable), a backup first, and a write that
adds a new config error is rolled back. A refused or accepted write is
one audit line. A dry run is not.

| Tool | Arguments | Effect |
| --- | --- | --- |
| `config_migrate` | `write?` (default false) | `pump::config::migrate`. `write: true` is wrapped in `guard_file` and is a write; otherwise a dry run and a read. Returns `{plan, diff, dry_run, wrote, charters}` |
| `config_set` | `key`, `value` (TOML text) | `config_edit::set_key`. Returns `{key, value, kind}` |
| `config_unset` | `key` | `config_edit::unset_key`. Returns `{key, unset: true}` |
| `pool_allow_set` | `pool`, `models` (array of strings) | Writes `pools.<pool>.harness.models` (the allow list `pool_bands` checks) through `config_edit::set_pool_key`. A non-array or a non-string element is refused before a write |
| `target_add` | `id`, `path`, `remote_url?`, `remote?`, `main?`, `github_repo?`, `rungs?` (`NAME=CMD`), `merge?` (`KEY=VALUE`) | `pump::config::targets::add`. Returns `{id, path, main, rungs}` |
| `target_remove` | `id` | `pump::config::targets::remove`. Returns `{id, removed: true}` |

`pump_rungs` `set` is the same guard (`RungsConfig::set`).

#### Role and charter writes

The same operator rule and the same guard as the config writes above.
`role_add`, `role_set` and `charter_set` call `Mailspace::add_role`,
`set_role` and `set_charter` with the arguments the CLI's
`vivi role add`, `vivi role set` and `vivi role charter set` take.

| Tool | Arguments | Effect |
| --- | --- | --- |
| `role_add` | `role`, `kind?`, `harness?`, `provider?`, `model?`, `thinking?`, `status?`, `labels?`, `cadence?`, `pool?`, `read_only?`, `models?` | `Mailspace::add_role` under `guard_file`, then the CLI's adoption of letters held under the new name (with the CLI's default: the pump's hand role, `[pump] to`, gets kind `hand` when `kind` is absent). A duplicate name is refused. Returns `{added, adopted, role}` |
| `role_set` | `role`, plus each of `kind`, `harness`, `provider`, `model`, `thinking`, `cadence`, `pool`, `read_only`, `models` with a `clear_<field>` form, and `status`, `labels`, `clear_labels` | `Mailspace::set_role` under `guard_file`. At least one field must be named (the CLI's rule). The CLI's `--pid`/`--host` (a live seat's runtime binding, the one unguarded `role set` write) is not exposed. Returns the `role_show` view |
| `charter_set` | `role`, `charter` (the body text; the CLI's `@path` and `--body-file` forms are not accepted) | `Mailspace::set_charter` guarding the charter file the CLI guards. Returns the `charter_show` envelope plus `bytes` |

#### Pump actions and reset

Hosted projects only, operator only, same rule as `pump_control`. Each
call is one audit line. `pump_action` posts for the pump's next tick
through the library function the CLI calls, and is not refused while the
pump runs (those functions do not check the lock). `pump_reset` is
`pump::reset::archive`: refused while the pump lock is held, running or
paused, with the library's message, and nothing moves on a refusal.

| Tool | Arguments | Effect |
| --- | --- | --- |
| `pump_action` | `action` withdraw\|requeue\|replan\|answer\|unlock; `needs?` (withdraw), `steps?` or `job?` (requeue; not both), `dry_run?` (requeue only, still a write), `need?` (replan, unlock), `step?`+`word?` (answer), `entry?`, `reason?`, `note?` | `withdraw` closes each need `withdrawn` and posts on its open record. `requeue` posts one action per flow step, or releases one held job. `replan`, `answer`, and `unlock` post on the open record. A library refusal is the tool error |
| `pump_reset` | — | Archives the pump home beside itself. Returns `{archive_dir, moved, mode, open_records}`: `moved` is the home-relative names that moved, `mode` is the fresh database's control mode |

#### Reads

Hosted projects only, operator only — the same rule as the writes above
— and read-only throughout: no config edit, no record, no letter, so
none is audited or rate limited. Each calls the same vivi library
function its CLI command calls and answers the CLI's `--json` body.
The live free-slot probe is refused on both tools that take `resolved`:
it runs the vivi binary's `pool status`, which the server is not, so
`free` is never answered here.

| Tool | Arguments | Effect |
| --- | --- | --- |
| `pump_log` | `file` log\|exceptions, `lines?` (1–500, default 50), `filter?` | Read-only tail of the pump's `log.txt` or `exceptions.txt`, as `vivi pump log` does: the last `lines` lines that contain `filter` when given, counted after filtering. Returns `{file, lines}` |
| `flow_show` | `kind` build\|research\|release\|repair, `need?` | `vivi flow show --json`: the shipped flow's Mermaid source and version, plus that need's runs and their steps when `need` is given (`runs` is null when the project has no `pump.db`) |
| `config_show` | `resolved?` (default false) | `vivi config show --json`: the merged config overview (roles, flow, pools), plus `origins` when a merged value was inherited. `resolved: true` is refused (above) |
| `config_validate` | — | `vivi config validate --json`: the issue array (severity `error`, `warning` or `not_ready`) exactly as the CLI prints it; readiness is read off the severities, not asserted |
| `pool_list` | — | `vivi pool list --json`: one row per configured pool — name, provider, slots, the bands table (model and reasoning per band) and harness models; `free` stays null (the live probe is the vivi binary's) |
| `pool_show` | `name`, `resolved?` (default false) | `vivi pool show <name> --json`: the pool's row — provider, slots, bands and harness models. `resolved: true` is refused like `config_show`'s, `free` is always null, and an unknown pool is refused |
| `role_list` | — | `vivi role list --json`: one view per role — address, aliases, kind, status, harness, provider, model, cadence, pool, models, and whether a charter is set |
| `role_show` | `role` (name or alias) | `vivi role show <role> --json`: one role's full view; an unknown role is refused |
| `charter_show` | `role` (name or alias) | `vivi role charter show <role> --json`: the charter body (an empty string when it has none), with its name, address and `has_charter` |
| `target_list` | — | `vivi pump target list`: the configured pump targets, each `{id, path, main, rungs}` |

#### Host tools

Nine tools need a **host principal** — the operator token or a key
minted `host: true`:

| Tool | Arguments | Effect |
| --- | --- | --- |
| `create_project` | `name`, `branch?`, `pool?`, `rungs?` | Create a hosted project: bare repository, lanes, git URL and pool remote from the `HOST_*` configuration; the operator server key it shows once is the project's `operator` key |
| `lane_create` | `name`, `target`, `git_url_prefix?`, `seed?` | Projects half only of `vivi project lane add`. Attaches the existing bare repository `<HOST_LANES_ROOT>/<target>.git` as `[pump.targets.<target>]` on `<HOST_PROJECTS_ROOT>/<name>`: clones `<project>/<target>` and adds the target through the guarded config write. Does not create anything under the lanes root. Success returns `{name, target: {target_id, checkout_path, git_url}, remaining: {half: "lanes", root, command}, readiness: {ready, missing}}`. `remaining.command` is the exact `vivi project lane add <project-root> <target> --lanes-root <lanes> [--git-url-prefix <prefix>] [--seed]` for the lanes owner (this call did not perform that half). `git_url_prefix` overrides `HOST_GIT_URL_PREFIX`; with neither set the clone uses the lane's local path. `seed` is not performed here; it is included in the command when true. A missing bare repository is refused (`isError`) with `{error, name, remaining, readiness}` and the same command. A second attach of the same target is the library's refusal |
| `key_mint` | `principal`, `grants?`, `host?`, `label?` | Mint a server key for `principal` (created when absent; grants not already held are added). `host: true` mints a host principal, which carries no grants and whose `principal` defaults to `host`. The raw `key` is shown once, `vivi_` + random; at most 32 live keys per principal |
| `key_list` | — | Principals with their keys (digests only) and grants |
| `key_revoke` | `key_id` | Revoke one key, on any principal; already-revoked is success |
| `grant_add` | `principal`, `grant` | Add one grant to an existing principal (idempotent) |
| `grant_remove` | `principal`, `project_id` | Remove the principal's grant on one project |
| `server_version` | — | This server's own name and version — the same pair `initialize` answers. Read-only |
| `project_list` | — | The hosted projects this server serves from the host registry: one `{project_id, name?, mode}` each (`name` is the project root's directory name, else the mailspace config `name`; `mode` is `hosted`). A project that cannot be read is skipped. Read-only |

A non-host key that calls one is refused
`<tool> is a host tool; this key is not a host principal`. A key/grant
mint refuses, among others: `principal is required (or host: true for a
host key)`, `a grant must name a project id and a member (1-64
characters), an optional identity, and a level of member or operator`,
`a host principal carries no project grants`, `that principal exists
with a different host flag`, `that principal already has 32 live keys`,
`no such principal or key`, and `<principal> holds no grant on
<project_id>`.

### Grants and levels

A grant is `{project_id, member, identity?, level}`: `level` is
`member` or `operator`, and `identity` names the vivi identity the key
acts as on that project (absent: the member identity itself). The level
is stored metadata; the operator RULES are re-checked per call — an
operator-gated tool (the pump controls, `pool_bands`, `pump_*`,
operator-only actions) still requires today's operator rule to hold for
this key on this project, exactly as the per-project keys did, and a
`member` grant is refused those tools. The grant wall on `/v1/mcp` is
checked before every dispatch; no tool call ever sees a project the key
is not granted on.

### Writes: audit and rate limit

One audit line is appended per **write** tool call, whatever came of it
(`ok`, `refused`, `error`), and per key/grant change; reads are not
logged. The self-service key routes write the same shape themselves:
`self_service_mint` on a successful `POST /v1/projects/{id}/keys` and
`self_service_revoke` on a successful `DELETE /v1/projects/{id}/keys/
{key_id}`, exactly one line each. The log is `DATA_DIR/audit.jsonl`: one
JSON object per line, 0600, append-only, no fsync, no read API yet. A
line is, in wire order: `at` (RFC 3339), `key_id`, `principal`,
`project_id` (null for host tools), `tool`, `outcome`
(`ok`|`refused`|`error`), and `summary` (a
short description, capped at 200 chars).

Writes are rate limited **per (project, key)** — `host` is the bucket
for host tools — through the same limiter the other routes use
(`SEND_RATE_PER_MINUTE`, default 30, `0` = none). A refusal reads
`over N writes/min for this key`.

Tool failures are `isError: true` with a text body, HTTP 200.

### Self-service keys

Minting and revocation require an **identity token** (proof of
possession), not another key.

- `POST /v1/projects/{id}/keys` `{label}` (1–64 chars), optional
  `identity`, on an identity token → `201`. This now mints a **server
  key**: principal `<member>@<project_id>`, or
  `<member>.<role>@<project_id>` when an acting identity is named (so a
  second key with a different identity does not resolve the first
  grant), carrying one grant on this project — level `operator` when
  today's operator rule holds at mint time, else `member`. The answer
  is `{key_id, key, label, created_by, created_at, grants}` (plus
  `identity` when one was named); the raw `key` is returned once
  (`vivi_` + random). At most 32 live keys per principal. A derived
  principal that already exists under a different member/identity
  derivation is refused `409 principal_conflict` naming both
  derivations (member names allow `.`): the second derivation to arrive
  never joins the first's grants.
- `GET /v1/projects/{id}/keys` (identity token or key) → `{keys:
  [{key_id, label, created_by, created_at, identity?, revoked_at?}]}`
  from the per-project mirror. No secrets.
- `DELETE /v1/projects/{id}/keys/{key_id}` (identity token of the
  minting member) → `204`; the server-store twin dies with it. Unknown
  or someone else's key is `404 key_not_found`.

The same secret is mirrored into the per-project registry
(`DATA_DIR/projects/<id>/api_keys.json`) at mint, so rooms auth and the
letter wire keep accepting it; the mirror is the only writer of new
per-project keys. A mirror dies with what it mirrors: `key_revoke` on
`/v1/mcp` revokes the entry in every project the key's principal is
granted on, and `grant_remove` revokes that principal's entries in that
one project's file (other projects' mirrors stand). A revoked or
unknown key is the same `401` as a bad
identity token on those routes. A live key is accepted on every project
bearer route, including the letter wire.

#### One-time legacy retirement

The first boot after the upgrade renames every
`DATA_DIR/projects/<id>/api_keys.json` to `api_keys.json.retired` —
pre-upgrade keys stay dead — and records `DATA_DIR/api_keys.retirement`.
Later boots leave live files alone: a self-service mint re-creates
`api_keys.json` for the mirror, and a restart must not retire it.

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
  `graph` projection, which reads `depends_on` from the letter's front matter only. A legacy letter (no `X-Vivi-Format` header) contributes no dependencies, and so does a letter that claims a format but does not parse; the graph does not error on that stored history. A hosted
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
