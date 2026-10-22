# Changelog

## 1.8.0 — 2026-10-22

Built on vivi 19.0.0, and the write rate limit becomes a deployment setting.

- **vivi 19.0.0.** The `vivi` dependency moves from v18.4.0 to v19.0.0. No
  source change was needed. vivi 19 writes `pump.db` schema 8, which a server
  built on vivi 18.x cannot open for pump health, `pump_control` or
  `pump_gate`; upgrade the server together with the host's pump.
- **`SEND_RATE_PER_MINUTE`.** The per-identity write rate limit is now a
  deployment setting. Unset keeps the public default of 30 per minute; a
  positive integer sets the limit; `0` turns the limit off (for a
  tailnet-only host whose writers are all trusted). Any other value refuses to
  boot, naming the variable. Rooms, the projects API and MCP writes (including
  `pump_control` and `pump_gate set`) share the one configured limiter, the
  refusal text reports the configured number (`over N sends/min/identity`,
  `over N writes/min/identity`), and the boot log states the effective limit.

## 1.7.0 — 2026-10-22

Operator control over MCP: create a hosted project from nothing, and pause,
resume or re-gate its pump, without shell access to the host. Built on vivi
18.4.0.

- **Host MCP endpoint and `create_project`.** `POST /v1/host/mcp` is a
  Streamable HTTP MCP endpoint for the host, outside any one project. Its
  Bearer is the new `HOST_OPERATOR_TOKEN` (constant-time compare); with the
  variable unset the endpoint answers 404, and a project identity token or API
  key is refused (403). The one tool, `create_project`, calls
  `vivi::project_create::create` with the roots from `HOST_PROJECTS_ROOT`,
  `HOST_LANES_ROOT`, `HOST_GIT_URL_PREFIX` and `HOST_DEFAULT_POOL` (a missing
  one is a configuration error naming the variable). Arguments: `name`, and
  optionally `gate`, `branch`, `pool`, `initial_commit`, `pump`, `serve`. The
  server reconciles the host registry at once, so the new project is served
  at `/v1/projects/{id}/mcp` without a restart or waiting for the watcher, and
  mints an operator API key (identity `operator`, minted by the host's own
  member) returned once in the answer. Creations run one at a time, and
  `hosted::reconcile` passes are now serialized.
- **MCP `pump_control`.** Operator-only pump control on a hosted project:
  `action` is `pause`, `resume`, `stop`, or `start` (`start` is `resume`, as
  in the CLI), through `vivi::pump::control::set_project_mode`. The answer is
  the resulting control state. A member token, a key another member minted, and
  a key acting as a role that is not of kind `operator` are refused.
- **MCP `pump_gate`.** Operator-only show and set of a hosted project's pump
  gate command (`[pump.gate] command` in the mailspace config), through
  `vivi::pump::gate_config`: `action` is `show` or `set` (with `command`). The
  answer is `{command}`, `null` when unset or blank; a blank `set` is refused
  by the library. Same authorization as `pump_control` (one shared check), and
  `set` is rate limited as a write. Pools, roles and other config are not
  exposed: a member who could repoint a pool's `remote_url` could send code
  elsewhere.
- **vivi 18.4.0.** The `vivi` dependency moves from v18.2.0 to v18.4.0
  (config inheritance, not-ready pumps, the gate setter, `project create`).
  No source change was needed for 18.3.0. The pump health verdict now
  includes the library's `not ready: ...` reasons for a project missing
  required pump settings.

## 1.6.0 — 2026-10-21

Pump health: a read-only health verdict over a hosted project's pump, so a
monitor can check it over the network without shell access to the host.
Built on vivi 18.2.0, which reads the pump store schema 7 that vivi 18.1.2
pumps write (vivi 18.1.0 refused it).

- **`GET /v1/projects/{id}/pump/health?since=&stale_after=`.** Any
  authenticated member (identity token or API key). The body is
  `vivi::pump::health` as JSON: `verdict` (`ok`, `degraded`, `stalled`,
  `unknown`) with `reasons`, `as_of`, `control`, `writer` (`state`, `source`,
  `pid`), `last_tick`, `status_age_secs`, unit counts, recent merges, blocked
  units and parked needs with reasons, complete-but-open needs, recent
  exceptions, and `changed_since` when `since` (a previous `as_of`) is given.
  A server-owned project answers `404 not_hosted`; an unknown query key or a
  non-integer value is `400`.
- **MCP `pump`.** The same JSON as a tool, for any caller on a hosted project
  (`since`, `stale_after`).
- **Read-only.** The library opens `pump.db` with SQLite's read-only flags in
  one read transaction, never migrates it, and finds the pump process from
  `/proc/locks` without touching the pump lock. There is no route that
  pauses, stops, requeues, withdraws or replans.

## 1.5.0 — 2026-10-21

Hosted projects: the server serves the host registry's projects from their own mailspaces (the hub's project mailspace and the server that wraps it are one mailspace). Built on vivi 18.1.0. Railway retired; self-hosted binary releases only.

### hosted MCP

On a hosted project the mailspace is the host's own, so MCP acts on it through
the `vivi` library exactly as the CLI would, as a mapped vivi identity. The
routing is per call: an **API key** on a hosted project; an identity-token
caller, and every server-owned project, keep the transport tools unchanged.

- **Acting identity.** The key's `identity`, else `HOSTED_DEFAULT_IDENTITY`
  (default `operator`). It must be a role of the hosted mailspace, or the call
  fails naming the identity.
- **Writes** call the CLI's entry points: `send` (`Mailspace::send`, roles
  `needs`/`tasks`/`wants`/`inbox`; `chat` is refused), `reply`
  (`Mailspace::reply`), `done`/`reopen` (`move_item_outcome`; the library's
  own authority rules and errors), `memo save` (`save_memo`). New tools `mark`
  and `unmark` (`set_marks`, on a need or want the acting identity holds).
- **Reads** are live: `overview`, `list` (kind, status, `to`, `from`, `marks`,
  `for`), `show`, `thread`, `graph` (the backlog graph), and the new `board`
  (`Mailspace::board` as JSON, host root omitted). `list` reads the acting
  identity's folders as `vivi need list --for <identity>` does; `for` names
  another role or `all`.
- Differences from the transport shapes: items carry `role` (folder), `date`
  (not `appended_at`), `marks` (always) and `last_event`; `show`/`thread` keep
  `status` for need/task/want only; `send`/`reply` answer with `handle`
  (recipient's copy), `sent`, `delivered`, `remote`; `done`/`reopen` answer
  with the library's `event`/`reported`/`joined_needs`.

### hosted projects

- **Serve the host registry.** At boot and whenever the registry file
  (`vivi::registry`, `serve = true` roots) changes, the server hosts each
  served root directly from that project's own `.vivi/`: no second mailspace,
  no seeding. The project id is `[sync] project`, else the id derived from the
  mailspace UUID; the mailspace's own sync key is registered as the creator
  member. Server data stays under `DATA_DIR/projects/<id>/`; `project.json`
  gains `mailspace_root`. A root that leaves the registry stops being served
  and loses nothing. vivi pin `v18.1.0`.
- **The index follows the mailspace (S4).** Each hosted project's
  `transport.sqlite` is kept current with its mailspace without copying
  letters: vivi's own push selection runs through an in-process transport
  that only checks the blob hash and inserts the transport row, then binds
  the letter in the mailspace so the next pass skips it. What travels is the
  mailspace's own `[sync] send` (the hub's host config sets `addressed`).
  Passes run every 5 s and before a list, lookup, or fetch read when the
  mailspace's change watermark moved; they skip while another pass runs or
  the host's `vivi sync` holds the sync lock. See phase-03 "Storage".
- **Member appends file into the mailspace (S5).** An append to a hosted
  project is filed with `vivi::sync::pull::file_records` (as a client pull
  files a record: verified, filed under the addressed role's folder, bound,
  reply-linked, events folded), then recorded in the index; the 403
  `project_read_only` refusal is gone. The per-member rate limit still
  applies; the indexer has none.

### epoch and key identity

- **Per-project log epoch.** Every list response (`GET .../items`, and the
  chat poll) carries a top-level `epoch`: a random u32 (1 to 4294967295)
  minted once per project and stored in `project.json`. A project that
  predates epochs gets one on first read. The vivi client binds pulled
  letters under it and treats a change as a server reset.
- **Identity on API keys.** `POST .../keys` takes an optional `identity` (a
  vivi identity name, `[A-Za-z0-9._-]`, 1-64). It is stored with the key,
  listed, and returned at verify beside `created_by`; the MCP dispatcher
  receives it as `acting_as` but no tool uses it yet. Key files without the
  field still load.

### item events

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
  client's, as kind `mail`, From `operator@<prefix>.local`: the member that
  owns the prefix counts an own-domain event only from the item's author or
  a `mind`/`operator` role, and a registration name is neither. Reopening an
  absorbed item is refused.
- **The fold reads every letter.** `list`, `overview`, `graph`, `show`,
  `thread` and `done`/`reopen` folded only the first page of visible
  letters (500, or 8 MiB), so an event past it was invisible to MCP and a
  new event's stamp and `X-Vivi-Event-Prev` were wrong. They now walk the
  index by cursor to the end, folding events as they go and keeping none.
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
