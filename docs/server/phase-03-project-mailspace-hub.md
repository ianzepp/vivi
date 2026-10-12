# Phase 03 — Project mailspace hub

Status: **CORE BUILT 2026-10-10, CLIENT BUILT 2026-10-11.** The server
(project store, identity registration, admission by introduction,
governance mail, visibility scoping, directory, letters, mailspace
addresses, history windows, lookup by handle) and the vivi client (`vivi
sync`, including `pull --since`, `fetch`, `pull --refs`, `resync`, and
`prune`) interoperate end to end; the 84-leg
`scripts/project-sync-smoke.sh` passes locally.
The wire as built is recorded at the end of this document. Transparent
mode is decided and not built. The postmaster LLM loop remains a future
phase. The room-based wire contract in [`contract.md`](contract.md) is
unchanged and the rooms API stays alongside (migration is the open want
`b4d07a2a`).
**Correction 2026-10-12.** The Storage section below, written with the
letter wire on 2026-10-11, removed the server-side mailspace ("A project no
longer uses a vivi mailspace on the server"). That was a misreading of
decided item 10, which fixes what travels (the raw `.eml`, byte for byte)
and says nothing about the server dropping its mailspace. It is reversed by
items 14 to 16: the project is a mailspace on the server again, and the
sealed-blob store is to be replaced. Until that is built, the sealed-blob
store is what runs.
Recorded 2026-10-10 from a design conversation with the repo owner;
admission by introduction and the reserved `postmaster` identity were
decided in a follow-up conversation the same day. Each item is marked
**Decided**, **Proposed** (an idea the owner has not confirmed), or
**Open** (deliberately undecided). Build work is tracked as needs and
wants in the project mailspace (`.vivi/`, roles `mind` and `hand`); this
document is the design record and the rulings.

## Why this phase exists

Phases 01 and 02 built a chat substrate. A *room* is the tenant: its API is a
send/poll log with a cursor, it has seats bound to keys, and clients render
polled records into their own mailspaces. On the server a room is stored as a
vivi mailspace, but that mailspace is only ever written; polls read a sidecar
`messages.json`. The README's "rooms are real mailspaces" is true of storage and
misleading about behavior.

The intended product is different. The Vivi server is the Vivi equivalent of an
IMAP server and an SMTP server combined, for a **project**:

- Several clients each have a local checkout of a common project, each with its
  own local mailspace.
- Client A creates a need locally, with a handle minted locally, and assigns it
  to a role that lives at client B (`--from` / `--to`).
- The need is pushed to the server and stored there. The next time B syncs, it
  pulls a copy into its local mailspace.
- With ten clients on one project, every client's needs and wants sync up, and
  every member syncs down the common superset. Vivi handles appear in the text of
  needs, wants, tasks, and mail, so they must mean the same thing for everyone
  who reads that text.

The Phase 02 workflow (a need crossing from A to B, closed on B, answered with a
receipt) already runs end to end. What changes is the architecture beneath it:
who the tenant is, what travels, and where the truth lives.

## The model

**Decided**

1. **The tenant is the project.** The server holds one mailspace per project.
   The room, as a tenant, goes away. The room was created to contain chat, and
   chat is just mail.
2. **Chat is ordinary mail** (`kind=mail`), threaded with the reply headers vivi
   already has. A group message is a mail with several recipients. Nothing needs
   a shared folder.
3. **Identities are flat names, unique within the project**, such as
   `mind-on-pharos`. The client chooses the name when it registers. It is bound
   to the key that registered it and published in the project directory as a
   member or role. The key, not the name, identifies the client.
   - ~~The domain part of an email address has no meaning on the wire.~~
     Superseded by item 11: the domain part of a letter's address is the
     sender's mailspace prefix, and the server routes on it.
4. **The server is the source of truth.** Clients are replicas. A new or
   damaged checkout can resync everything from the server.
5. **Clients mint every id.** The server never creates a need, a want, a task, a
   mail, or an id. It ingests items under the id they arrive with (the traveling
   ids of Phase 02) and refuses an id that already names different bytes.
6. **The protocol is append-only.** Sync is a union of items, so concurrent
   writers cannot overwrite each other. The server assigns each appended item a
   sequence number so a client can ask for everything after N. That number is a
   transport cursor only and is never part of an item's identity.
7. **Visibility is by access control, not cryptography.**
   - Work items (needs, wants, tasks) are visible to every member of the project.
   - Mail is visible only to the connecting client: the server exposes only the
     inbox of the client that is authenticated.
   - The server operator can read everything. Content is encrypted at rest with
     a server-held key, which protects the disk and does not make anything
     private from the server. There is no end-to-end encryption.
   - Handles in a mail resolve for its recipient because every member holds every
     work item.
8. **Admission is by introduction.** A public key becomes admissible when a
   member of the project says so: the member's signed statement carries the
   candidate key (and, proposed below, the identity name it may register),
   and the server treats that key as admissible from then on. Knowing the
   project id grants nothing; access requires a signature from a key a member
   vouched for. The new developer's key reaches the vouching member out of
   band first.
9. **`postmaster` is a reserved identity.** It exists in every project,
   cannot be registered by a client, and is the administrative addressee of
   the mailspace. A server-side LLM that interprets mail addressed to it is a
   future phase (see below); nothing about the loop is needed to build this
   phase.
10. **Items travel as letters (decided 2026-10-11, repo owner).** Every
    vivi type — mail, task, need, want — is an `.eml` file, and the `.eml`
    is the unit of sync. The origin's file goes to the server byte for byte,
    is stored there, and is synced down byte for byte to every member. A
    letter is immutable once created; its hash (`sha256` of the bytes, which
    is the `content_id` every store already uses) is therefore the same on
    every machine, and that is the only way immutability can be enforced.
    The server never parses, renders, or rewrites a letter. See
    [Letter wire](#letter-wire-decided-2026-10-11).
11. **Mailspaces are addressed by id (decided 2026-10-11, repo owner).**
    Every mailspace has a UUID, minted once on the machine where it lives
    and stored in that mailspace's uncommitted `mailspace.toml` (`id`).
    The first 8 characters of the UUID are its **prefix**, and the prefix
    is the domain part of every address the mailspace mints:
    `mind@3f9a1c7e.local` is the `mind` role of the mailspace whose id begins
    `3f9a1c7e`. Consequences:
    - The address is in the letter's bytes, so sender and recipient are part
      of the hashed, immutable object. Routing no longer depends on a
      config table outside the letter.
    - The UUID is the global identity. The prefix only has to be unique
      among the mailspaces of one project (1,000 mailspaces collide with
      probability about 0.012%), because a prefix is bound at registration
      and the first claim wins. A collision is refused at join, before
      anything is pushed.
    - One wire identity is one checkout is one mailspace. Roles are the
      local part of the address, not separate identities.
    - Same-domain addresses are local delivery and never need the server.
    - The prefix is a label, never proof: authentication is the key. The
      UUID is decoupled from the key so an address survives re-key. The
      server stamps each letter with its authenticated sender's registered
      prefix, and a receiver refuses a `From` address whose prefix
      disagrees, so an address inside a letter can be trusted.
    - `.vivi/` configuration is machine-local, not source. A mailspace is
      not a repository: one workspace-level `.vivi` has one id, and a
      developer's separate `.vivi` is a different mailspace with a
      different address. A committed or copied `mailspace.toml` would
      duplicate an id; the registration refusal is the backstop.
    - Letters minted before this change keep their old domain
      (`name@<mailspace-name>.local`). They are immutable. They travel, but
      their addresses are not routable, so a reply to one needs an explicit
      address.
    See [Mailspace addresses](#mailspace-addresses-decided-2026-10-11).
12. **Transparent mode (decided 2026-10-11, repo owner).** A mailspace can
    be configured to keep no local store. Its commands read live from the
    server and write letters straight to it, so a remote pool slot or
    auditor reads its tasks and replies with no download step and no
    handshake. A slot is a virtual extension of the mind's mailspace: it
    carries the mind's mailspace id, so work addressed to a seat of the
    mind (`hand-7@<mind prefix>.local`) is already addressed to it, and
    the mind can create and push that work before any slot exists. Three
    rulings: the store is always stateless; commands that change only
    local state are refused; the interim credential is the member's own
    key. Local-first is unchanged. **Superseded 2026-10-12 by item 15**:
    pool slots reach the server through MCP, not a stateless local store.
    See [Transparent mode](#transparent-mode-decided-2026-10-11).
13. **History is pulled by window, and letters are fetched by handle
    (decided 2026-10-11, repo owner).** The common case is a hand that
    pulls a week of history for coverage, not a full clone and not a bulk
    seed. The server stamps every letter with its append time
    (`appended_at`, the server clock, monotone with the sequence), so a
    pull can start at a time instead of a cursor, and the list takes
    `limit`, `after`, `kind`, and `to`. A mailspace may hold a suffix of
    history; the pull records a **floor** saying where. Deepening an
    older range is a later unit; `resync` replays everything. A handle (8
    hex characters of a message id) is matched by the server against ids,
    scoped by visibility, so a hand can fetch the letters its content
    references and does not hold. `vivi sync prune` evicts old bound
    letters locally under the chat-prune policy, restorable byte for
    byte. Bulk push (seeding a large existing mailspace at 30 letters per
    minute) is deferred and is not a current requirement. See
    [Windows, handles, and prune](#windows-handles-and-prune-decided-2026-10-11).

14. **The project is a mailspace on the server (decided 2026-10-12, repo
    owner).** The server holds one vivi mailspace per project under
    `DATA_DIR/projects/<id>/`, and that mailspace is the store and the read
    path. This restores the original item 1 and reverses the Storage
    section's 2026-10-11 removal. The wire is unchanged: letters still PUT
    and GET as raw `.eml`, byte for byte, with the same `content_sha256`
    check, because a mailspace can hold a letter verbatim (the client
    already ingests pulled bytes verbatim). The reason is MCP (item 15): a
    server that answers "list my open needs" or "show the thread for this
    handle" needs a real store, and a sealed blob plus a JSON index cannot
    answer either.
15. **Server MCP, with `vivi mcp` as its stdio bridge (decided 2026-10-12,
    repo owner).** The server exposes one MCP tool surface over HTTP, for
    LLMs that have MCP but no vivi binary. It mirrors, to a reasonable
    extent, the vivi command tree: fetching needs, wants, tasks and mail,
    threads, reply, send, and reports. `vivi mcp` is a thin stdio bridge to
    that same endpoint: it holds the member key, signs the challenge,
    renews the token, and forwards tool calls, so a pool slot needs only
    the binary, a mailspace id, and a key, and has no local mailspace and
    no sync. This supersedes transparent mode (item 12), which is not
    built and is not to be built. Consequences:
    - Reads are live queries on the server's mailspace; the recipient
      filter, handle lookup and long-poll prerequisites of item 12 are
      served by it and are not separate work.
    - Because the HTTP endpoint serves callers that cannot sign or mint,
      the server composes the letter and mints its id for an authenticated
      member. This is a scoped exception to item 5, like `postmaster`
      replies. The letter's `From` carries that member's registered prefix.
    - The credential is the member's bearer token. No new pass type.
    - Permissions are enforced on the server: a hand may file a report; a
      close travels only from the mind's identity. A report is not an
      acceptance.
16. **Plain text on an encrypted volume; the index database answers
    queries (decided 2026-10-12, repo owner).** The server mailspace holds
    letters and indexes in the clear. Protection at rest is the encrypted
    volume, not per-letter sealing; the server cipher still guards keys and
    tokens. Sealed blobs can be layered on later if that becomes a
    requirement. The design rule: search, listing, and filtering run
    against the mailspace's indexed database, never by scanning `.eml`
    files. The `.eml` is kept for byte-exact GET and hash verification and
    is read only to serve a letter.

## What carries over from phases 01 and 02

| Piece | Why it survives |
| --- | --- |
| ED25519 challenge/sign, key-bound identity | Becomes registration of an identity name |
| Sealed storage at rest and the boot key verifier | Unchanged |
| Traveling ids and presented-id ingest (P2-1, P2-2) | Exactly what "clients mint every id" needs |
| `kind` headers, filing by kind, backlog nodes | The unit of sync is now the item itself |
| Idempotent outbox, at-least-once delivery | Reusable for remote `vivi mail send` |
| `remote_bindings` (mailbox, uid, uidvalidity, stale) | IMAP-shaped table; today only chat uses it |
| `import_mailspace` (dedupes messages, events, links, blobs) | A primitive for moving items between mailspaces |
| Staging and production services, deploy pipeline, smoke harness | Same shape, new legs |
| Per-identity delivery through `Mailspace::send` | The server already writes it; nothing reads it |

## What is replaced

- The `/v1/rooms/*` API, room ids, seats, labels, and per-room cursors.
- The sidecar `messages.json` as the read path. The mailspace becomes the store
  (the deferred P2-8, now central).
- Server-minted message ids (`m{cursor}`).
- The `vivi chat` commands as a separate concept. Remote delivery becomes part of
  `vivi mail`, which is local-only today ("no external side effects").
- The client rendering of a polled chat record into an `.eml`. The server holds
  the real item.

## Client-side questions (resolved 2026-10-11)

- Local addresses were `<identity>@<mailspace-name>.local`, so every
  checkout of a project shared one `mind@<project>.local`. Resolved by
  item 11: the domain is the mailspace's own id prefix, and a remote role
  is addressed directly with no local stand-in.
- Replying to a remote sender routes through the server. Built: a reply is
  addressed to the original `From`, which is routable.
- The local folders are unchanged; chat is plain mail.

## Transparent mode (decided 2026-10-11)

Decided by the repo owner in conversation on 2026-10-11. Not built, and
**superseded on 2026-10-12 by decided item 15** (server MCP and the `vivi
mcp` stdio bridge). Kept as the design record and for the reasoning on
pool kinds and seats; its rulings no longer bind.

**Why.** The mind keeps needs in a priority order and, when it works one,
writes a task for a hand. That hand is often a remote slot that does not exist
yet, and may not for a while. The mind should not have to wait for a slot,
provision it, learn an address, come back, address the task, push it, and send
the slot back to sync it. A slot, or an auditor handed a set of handles, is an
execution location of the mind's mailspace, not a mailspace of its own.

**What it is.** A mailspace configuration that keeps no local store. The
provisional key is `[sync] transparent = true`. Each command is one session:

1. Make a temporary directory and copy the mailspace config and key into it.
2. Authenticate (challenge, sign, bearer).
3. Pull what the command needs, with the same verification and additive
   filing as `vivi sync pull`.
4. Run the command against that store, unchanged.
5. Push every letter the command wrote.
6. Delete the directory.

Letters keep their bytes and ids, so a handle (the first 8 hex of the message
id) is the mind's handle, and the mind receives the slot's replies the
ordinary way: filing is by address and additive, so a letter authored by
another place acting as the mailspace lands in the right role's inbox.

**How the work reaches the slot.** The slot carries the mind's mailspace id.
Work addressed to a seat of the mind (`hand-7@<mind prefix>.local`) is
already addressed to it. The mind creates and pushes that work whenever it
likes, before any slot exists. Parallel slots address distinct seats, since
two readers of `hand` would see the same task. Dispatch places two files on
the slot, the mailspace config (id, `[sync]`, the transparent flag, the seat
role) and the key, and the instruction is the handle or handles to work. An
auditor is the same: a seat plus handles, no dedicated mailspace.

**Rulings**

- **Stateless always.** A transparent command never reads or keeps a persisted
  store, even when the host's directory persists. A persisted replica carries
  local-only state (closed needs, marks, the backlog graph) that never syncs,
  diverges from the mind's silently, and carries one seat's leftovers into the
  next. A content-addressed letter cache is safe later, because a letter is
  immutable and never goes stale; it is an optimization, not part of this
  decision.
- **Reads are live, writes are letters.** A command whose only effect is local
  state is refused with a clear message: closing or reopening a need, marks,
  graph, role edits, memos, rules, and the mind-side surfaces `board`, `boot`
  and `step`. `need done --note` sends its note as a letter and closes
  nothing. A first-class done is the appended-event item under Proposed.
- **A write is a push.** A command that writes a letter pushes it before it
  reports success and exits non-zero if the push fails. There is no outbox:
  an ephemeral host has no disk to queue to.
- **The interim credential is the member's own key,** provisioned to the slot.
  The owner's ruling is that the baseline is a mind using ordinary subagents
  in the same workspace, which already see everything in the mailspace
  through the vivi tool, so no scoped credential is required. Known costs,
  accepted: the member's send budget (30 per minute) is shared by every slot
  and the mind, and the key lingers on a warm host until something removes it.

**Pool kinds**

| Pool | What persists | Behavior |
| --- | --- | --- |
| Destroyed on exit (for example a Fly machine) | Nothing | Stateless per command. A write that fails to push fails the command. |
| Warm directory (for example SSH to a directory) | The directory, possibly the config and key | Same stateless store. The key should be removed at slot end; a crashed slot leaves it. |
| Local worktree | Everything | No transparency needed. Discovery walks up to the mind's own `.vivi`, so the hand is on the same mailspace. |

**Server prerequisites.** None is needed for correctness. All are needed for
scale or latency:

- **A recipient filter on the list:** `GET /items?since=N&to=<address or
  prefix>`. Without it every command pulls everything the member can see,
  including every work item of the project.
- **A handle lookup.** The single-letter `GET` takes the full `msg_` id and a
  handle is the first 8 hex of it, so a thin client needs a route that
  resolves a handle (for example `?handle=<8 hex>`) without scanning.
- **Long-poll on the list** (`?wait=`, registering the waiter before reading
  so an append between read and wait is not missed), so the originating
  mind's `pool wait` can block on a reply instead of polling.

**Open**

- The send budget: raise the member's bucket, or key it per seat, once
  parallel slots are real.
- Seat role provisioning: write the role into the slot's config, or let
  filing create the account on first use (it files under an unknown role with
  a warning today).
- The command set of the first cut: list, show, thread, reply, send, and
  `done --note` are the intended core.

## Proposed

- Closing a need is an appended event. Every client derives open or closed by
  folding events in the server's append order, so everyone converges. A receipt
  carrying evidence can still be a normal mail, but it is not the mechanism.
- Mail to an unpublished identity is rejected at submit time, with a hard error to
  the sender. (Not built: the server accepts a recipient prefix that has not
  registered, so a mailspace can be addressed before it joins. Revisit if
  typos prove costly.)
- Identity names use the restricted character set roles use today (letters,
  digits, `.`, `-`, `_`, with a length cap). Names are immutable once registered;
  a rename is a new identity.
- A directory call lists a project's members and roles. (Built:
  `GET .../directory`, with each member's `mailspace_id`.)
- A connecting client sees the inboxes of every identity bound to its key.
- Any admitted member may vouch for a new key, so trust is transitive one hop
  at a time; the who-admitted-whom chain in the log is the accountability
  record.
- The vouch pins the identity name along with the key ("this key is
  `alice-external`"), so the human judgment at admission covers who the person
  is.
- The vouching member verifies the key fingerprint with the new developer
  through a second channel (voice works), because a key swapped in transit
  gets the wrong party admitted.
- The project creator's key is admitted at creation — the first key has no
  introducer.
- Revocation and re-keying share the vouching privilege and are themselves
  append events.
- The vouch travels on the wire as structured mail to `postmaster`, processed
  deterministically at submit: a fixed format, a privilege check, the
  admission appended, and an ack in the submit response. A dedicated CLI
  verb, if wanted, is client-side sugar that composes and sends the same
  mail. Free-form prose waits for the postmaster loop.
- Governance mail (admissions, revocations, re-keys) is visible to every
  member — a scoped exception to mail's recipient-only visibility — because
  the vouching chain is the accountability record.

- **A delegated pass for slots (proposed 2026-10-11, deferred).** The member
  key mints, offline, a short-lived EdDSA-signed JWT that the server
  accepts directly as a bearer after verifying it against the member's
  registered key. Claims: `iss` (member name), `aud` (project id), `sub`
  (seat), `roles` (the roles it may send as and read mail for), an optional
  `read` list of handles, `exp` (hours), and `jti`. It is single hop and
  non-renewable, so expiry replaces revocation. A scoped pass also needs
  the sender role in the envelope, checked by the server against `roles`
  and by receivers against the letter's `From`. Deferred because transparent
  mode accepts the member key (decided item 12); the pass adds a per-seat
  send budget, audit, and least privilege when those are wanted. Pin one
  algorithm, require `exp` and `aud`, and never log the token.

## Future phase: the postmaster loop

Deferred by the owner on 2026-10-10. When it is built, the guardrails settled
in the design conversation apply:

- The model extracts a typed operation (`admit key K as name N`, `revoke K`,
  `re-key N to K'`); authorization and execution stay in deterministic code.
- Quoted and forwarded content is data. Operation parameters come from the
  authenticated sender's own words and attachments; a public key found inside
  a forwarded quote is inert.
- The structured deterministic path remains the ground truth; CI and the
  smoke script never depend on an LLM, an API key, or a budget.
- Postmaster replies mean the server authors mail and mints ids — a scoped,
  attributed exception to decided item 5. The confirmation gate on
  irreversible operations is the main reason to allow it.
- Interpretation runs as an async loop over the append-only inbox: an outage
  or a budget cap means mail waits, and execution is idempotent per item id.
- Open when the phase starts: model and provider, in-process versus a sidecar
  process, cost ceilings, and which operations are confirmation-gated.

## Open

- **The vouch's shape.** Structured mail to `postmaster` versus a dedicated
  `vivi` command. The recommended layering is recorded in Proposed: mail on
  the wire, command as sugar.
- Key lifecycle details: lost keys and re-keying (a new key admitted by
  another member, and whether the name and history follow it), name release,
  and name squatting if a vouch ever names nothing. Admission settles the
  front door; recovery stays open.
- Retention and deletion on a server that is the source of truth.
- How an assigned work item is represented to members who are not the assignee
  (the item plus assignee metadata, or per-recipient delivered copies).
  (Resolved: the letter's own addresses are the assignee, and a work item
  for another mailspace's role is held under that address.)
- What supplies the sequence cursor (for example the per-store event log vivi
  already keeps) and how concurrent appends are serialized per project.
  (Resolved: the project index's counter under one process-wide append
  mutex.)
- Whether the wire stays HTTP or speaks IMAP/SMTP. (The item encoding is
  decided: the raw `.eml`, decided item 10. HTTP is the working assumption.)
- Migration of the rooms and messages already on the staging and production
  services, and how long the rooms API stays alongside the new one.
- The MCP tool list of the first cut and its role permissions (item 15).
- Where the transport fields live in the server mailspace. Verbatim ingest
  is public API in `vivi` (`Storage::ingest_if_absent`, `ingest_message`,
  and `Mailspace::deliver_raw`), but the client's filing (`sync::filer`) is
  `pub(crate)` and tied to client pull types. The server holds every
  member's letters, so it needs one row per letter and queries by
  recipient prefix and kind, not per-role delivery. `sequence` maps onto
  the existing `remote_bindings` uid; `appended_at` and `from_prefix` have
  no column and need a decision (an ingest event, a table beside the
  mailspace, or a small public addition to `vivi`). A spike settles this
  before the store is built.

## Effect on pending work

- **Poll efficiency** is settled by this design: a per-project append-only log
  read by sequence, with a per-project wake on append. A sandbox measurement on
  2026-10-09 showed the sidecar scan costing 33 ms per empty poll at 10,000
  records and a send costing 51 ms; no work happens before this phase.
- **Room cleanup** (a delete endpoint) is moot.
- **Direct-message privacy** is resolved by per-client mail scoping.
- **P2-8** becomes the core of the phase.
- The vivi client's one-room-per-mailspace and `watch` discussion is moot: a
  local mailspace maps to one project.

## Wire contract as built (2026-10-10)

> **Superseded in part.** The `POST /v1/projects/{id}/items` body shape and
> the `GET .../items` record shape below are replaced by the
> [Letter wire](#letter-wire-decided-2026-10-11). Project creation,
> challenge, registration, directory, admission, and the vouch rule are
> unchanged.

Endpoints under `/v1/projects`, additive alongside the frozen rooms
contract. Authentication is per-identity ED25519 challenge/sign with digest
bearer tokens, mirroring the rooms flow. All item content is sealed at rest
with the server key; the project mailspace under `DATA_DIR/projects/<id>/`
is the store and the read path (P2-8 closed); `items.json` beside it holds
only the sequence counter and replay digests — transport state, never
content.

- `POST /v1/projects` `{public_key_pem, creator_name}` → 201
  `{project_id, admission: {name, fingerprint}}`. The creator's key is the
  founding admission (bootstrap).
- `POST /v1/projects/{id}/identities/challenge` `{public_key_pem}` →
  `{challenge_id, challenge, expires_in}`, keyed to the project and key
  fingerprint.
- `POST /v1/projects/{id}/identities` `{name, public_key_pem,
  challenge_id, signature_base64}` → 201 `{name, token,
  token_expires_at, fingerprint}`. The name must be admitted for exactly
  the presented key (canonical SPKI match), else 409 `admission_required`.
- `GET /v1/projects/{id}/directory` (bearer) → `{members: [{name,
  fingerprint}]}`, sorted by name.
- `POST /v1/projects/{id}/items` (bearer) with a required `Message-ID`
  header of `msg_` + 24 lowercase hex — clients mint every id (decided
  item 5); there is no server-minted form. Body `{to, subject, body,
  kind, in_reply_to, refs}`; `from` is always the authenticated identity.
  Kinds `need`/`task`/`want` file by kind; `mail` or absent is plain.
  Body cap 64 KiB (413); 30 sends/min per project and member (429). → 201
  `{message_id, sequence}`; a replay of the same id and bytes → 200 with
  `duplicate: true`. An item addressed `to: postmaster` is a governance
  vouch with body `{"vouch": {"name", "public_key_pem"}}`, processed
  deterministically at submit: the admission is written first, then the
  item ingests; a malformed vouch or unknown voucher is 400 with nothing
  stored; the same name for a different key is 409.
- `GET /v1/projects/{id}/items?since=N` (bearer) → `{records:
  [{sequence, message_id, from, to, kind, in_reply_to, refs, body,
  raw_eml}], next_cursor}`. Visibility per decided item 7: work items and
  postmaster governance mail are member-visible; other mail is visible
  only to its recipient and sender. Sequences are transport cursors and
  never part of item identity; filtered items do not advance
  `next_cursor`.

### Carried forward from the build

- ~~Identity tokens have no refresh endpoint~~ Built 2026-10-11:
  `POST /v1/projects/{id}/auth/token` (see Token renewal below).
- The append rate limiter folds the identity name into the limiter's u32
  seat slot (`DefaultHasher`); a theoretical name collision shares one
  bucket. True string keying wants a `safety.rs` variant. The cap is 30
  sends per minute per member, so a first push of a large mailspace takes
  several runs (the client stops cleanly and resumes).
- Revocation and re-key events share the vouch privilege by design but are
  not built; the registry admits only.
- Per-project wake exists in the store (`ProjectWake`) with no long-poll
  endpoint wired yet; the items read is a plain poll. A long-poll must
  register its waiter before reading so an append between read and wait is
  not missed (a miss costs latency, never data: reads are cursored).
- ~~The smoke script has no project-API legs~~ Built 2026-10-11:
  `scripts/project-sync-smoke.sh` is the end-to-end acceptance for the
  letter wire (two checkouts, real server, real binaries).
- URL project ids are used raw by the challenge and identity paths rather
  than canonicalized. Harmless on a case-sensitive filesystem and not a
  signature bypass, but it should be canonicalized.
- Appends and reads scan the whole `items.json` index linearly.
- Bulk append is deferred (decided item 13): seeding a large existing
  mailspace is one `PUT` per letter at 30 letters per minute per member,
  about eleven hours for 20,000 letters. A JSON batch action, resumable by
  id and counted per call, is the shape if it is wanted.

**Shape rule.** A letter itself, PUT or GET, is the raw `.eml` with its
envelope in HTTP headers. Every action *around* letters (listing now,
archiving and the like later) is JSON.

### Append

`PUT /v1/projects/{id}/items/{message_id}` (bearer). `{message_id}` is the
client-minted id, `msg_` + 24 lowercase hex (decided item 5). The request
body **is the letter**: the exact `.eml` bytes, `Content-Type:
message/rfc822`, no wrapping and no encoding. The envelope fields the
sender declares ride as request headers (these are HTTP headers, not
headers of the letter):

- `X-Vivi-To: <recipients>` (optional): `postmaster`, or a comma-separated
  list of addresses (grammar under
  [Mailspace addresses](#mailspace-addresses-decided-2026-10-11)).
- `X-Vivi-Kind: need|task|want|mail` (optional)

- `from` is always the authenticated identity; it is not client input.
- The body is capped at 64 KiB (413 `body_too_large`); an empty body is 400.
- `PUT` is idempotent by id, which is the point: replaying the same request
  is safe.
- 201 `{message_id, sequence, content_sha256}` for a fresh letter.
- 200 with `duplicate: true` for a replay: same id and same bytes, or the
  same bytes presented under a different id **by the same sender** (the
  response names the original id and sequence). The second case is
  sender-scoped so a member who holds the exact bytes of mail they cannot
  see learns nothing about it; another member presenting those bytes gets
  an ordinary fresh append under their own id.
- 409 `conflict`: the id already names different bytes.
- 400 `invalid_request`: bad id, empty body, bad `X-Vivi-Kind`, or an
  invalid `X-Vivi-To`.
- Rate limit and the vouch rule are as built. For `X-Vivi-To: postmaster`
  the letter's body (the bytes after the first blank line, UTF-8) must be
  the JSON `{"vouch": {"name", "public_key_pem"}}`; the admission is
  written first, then the letter is stored.
- A client verifies that the response's `content_sha256` equals the
  `sha256` of the bytes it sent.

### Fetch one letter

`GET /v1/projects/{id}/items/{message_id}` (bearer) returns the letter as
the raw body, the same format `PUT` accepts:

```
200 OK
Content-Type: message/rfc822
X-Vivi-Message-Id: msg_...
X-Vivi-From: alice
X-Vivi-To: hand@3f9a1c7e.local  (omitted when none)
X-Vivi-Kind: need               (omitted when none, meaning mail)
X-Vivi-Sequence: 7
X-Vivi-Content-Sha256: <sha256 of the body>

<the exact .eml bytes>
```

- Visibility is the same scoping as the list. A letter the caller may not
  see is 404 `item_not_found`, identical to an unknown id, so the route
  cannot be used to probe for existence.
- A malformed id is 400 `invalid_request`; an unknown project is 404
  `project_not_found`; a missing or bad bearer is 401.

### Read (list, JSON)

`GET /v1/projects/{id}/items?since=N` (bearer) →

```json
{"records": [{"sequence": 7, "message_id": "msg_...", "from": "alice",
              "to": "hand@3f9a1c7e.local", "kind": "need",
              "content_sha256": "...",
              "raw_eml": "<base64, the exact bytes appended>"}],
 "next_cursor": 7}
```

- Visibility and cursor behavior are as built: work items and governance
  mail are member-visible, other mail is visible to its sender and
  recipient, and filtered items do not advance `next_cursor`.
- `raw_eml` is byte-identical to what was appended. A client verifies
  `sha256(raw_eml) == content_sha256` and refuses a record that fails.
- The list is JSON with base64 letters because it is a batch of envelopes;
  only the single-letter push is a raw body.

### Mailspace addresses (decided 2026-10-11)

Item 11 made concrete.

**Registration.** `POST /v1/projects/{id}/identities` gains a required
`mailspace_id`: a canonical UUID, lowercase and hyphenated (36 characters).
A missing or malformed id is 400 `invalid_request`. The server derives the
prefix (the first 8 characters). If another registered identity in the
project already holds that prefix, the registration is refused 409
`mailspace_prefix_taken` and nothing is registered; the admission stays
usable for a registration with a different id. An already registered name
remains 409 `conflict`. The 201 response gains `mailspace_id`. An identity
registers exactly one mailspace id for life.

**Directory.** `GET /v1/projects/{id}/directory` members are
`{name, fingerprint, mailspace_id}`. `mailspace_id` is null for an identity
registered before this change; such a member has no prefix and cannot be
addressed.

**Envelope recipients.** `X-Vivi-To` is `postmaster` or a comma-separated
list of 1 to 16 addresses `local@domain`: local part `[A-Za-z0-9._-]{1,64}`,
domain `[A-Za-z0-9.-]{1,64}`, the whole header at most 1 KiB. The sender's
client fills it from the letter's own `To` and `Cc` addresses (lowercased,
deduplicated, sorted); the server never reads the letter. The server checks
grammar only. A domain of exactly `<8 lowercase hex>.local` names a
mailspace prefix. Any other domain is accepted and routes to no one. A
prefix need not be registered when the letter is appended.

**Mail visibility.** Mail (no kind) is visible to the member who sent it and
to the member whose registered prefix equals the prefix of any recipient
address. Need, task, and want letters and mail to `postmaster` stay visible
to every member. Single-letter `GET` follows the same rule and answers 404
otherwise. The index keeps `to` as the raw header string; no index schema
change.

**Sender prefix.** When a letter is appended the server records the
authenticated sender's registered prefix beside it (null for an identity
registered before this change). The list returns it as `from_prefix` and
the single-letter `GET` as `X-Vivi-From-Prefix`. It is fixed at append and
never recomputed. The server still never reads the letter. A receiving
client refuses any letter whose `From` address domain is exactly
`<8 hex>.local` with a prefix other than `from_prefix` (a missing
`from_prefix` counts as a mismatch). Legacy name domains and external
domains are exempt. This is what makes a `From` inside a letter an
authenticated claim rather than free text: no member can write another
mailspace's address.

**Envelope check at the receiver.** On pull, unless the envelope `to` is
absent or `postmaster`, the client compares the envelope's addresses with
the letter's own `To` and `Cc` and refuses a record that disagrees. The
server stays unparsing, and a lying envelope is caught at the edge.

### Windows, handles, and prune (decided 2026-10-11)

Item 13 made concrete. Bulk push is not part of this.

**Append time.** Each letter is stamped at append with `appended_at`, the
server clock as RFC 3339 UTC to the second. The stamp is monotone with the
sequence (the server stores `max(previous stamp, now)` under the append
mutex, so a clock step cannot reorder it) and is fixed when the letter is
appended. A replay returns the original row with its original stamp. The list
returns it as `appended_at` and the single-letter `GET` as
`X-Vivi-Appended-At`. A row written before this field existed has none; the
index loads without it, and a `after` window excludes such rows.

**List parameters.** `GET /v1/projects/{id}/items` accepts, all optional and
combined with AND (a filter narrows what visibility allows and never widens
it):

- `since=N`: sequences after N (as built).
- `limit=K`: at most K records. Default 500, maximum 1000, and a page also
  stops adding records once their raw letters exceed 8 MiB (at least one
  record is always returned).
- `after=<RFC 3339 time>`: only letters whose `appended_at` is at or after the
  time.
- `kind=need,task,want,mail`: a comma list of kinds; `mail` is plain mail
  (no kind).
- `to=<address or prefix>`: letters whose envelope recipients include that
  exact address, or any address whose domain is `<prefix>.local`. Case
  insensitive.

The response gains `has_more` (true when more records match after
`next_cursor`). `next_cursor` is the sequence of the last record returned,
or `since` when none matched; filtered-out and invisible rows never advance
it. A page is bounded by `limit`, so a client loops until `has_more` is
false and saves its cursor after each page.

**Lookup by handle.** A handle is the 8 hex characters after `msg_` in a
message id. `POST /v1/projects/{id}/items/lookup` with
`{"handles": ["4b8275cc", ...]}` (1 to 100 handles, each 8 hex characters,
case insensitive; anything else is 400 `invalid_request`) returns
`{"records": [...], "missing": ["...", ...]}`. Records have exactly the list
record shape, ordered by sequence, and are scoped by the same visibility
rule as the list. A handle that names several ids returns all of them. A
handle that matches nothing the caller may see is in `missing`, identical
whether the id is unknown or hidden, so the route is not an existence probe.
Lookup never moves a cursor.

**Compression.** List and lookup responses are gzip-compressed when the
client sends `Accept-Encoding: gzip`, where an existing dependency's feature
makes that possible. It is an optimization, not a contract.

**Client.**

- `vivi sync pull --since <age|date>`: on a mailspace with no pull cursor for
  the project, start at that window (`7d`, `12h`, `90m`, an RFC 3339 time, or
  a `YYYY-MM-DD` date, midnight UTC) and continue to the head. The state then
  records the **floor**: the sequence and `appended_at` of the first record
  received, or the requested time when the window was empty. `vivi sync
  status` prints `history from <time> (sequence N)` or `full history`. On a
  mailspace that already has a cursor, `--since` is refused with a message
  that names the floor and says `vivi sync resync` replays everything.
  Deepening an older range is a later unit.
- `vivi sync resync`: reset the cursor and floor to zero and pull, through
  the same verification and ingest as `pull`. Evicted letters come back
  byte-identically under the same ids and local state is left as it was.
  Interrupted, it resumes from the saved cursor.
- `vivi sync fetch <handle>...`: look the handles up (in chunks of 100) and
  file what comes back through the same verification and filing as pull.
  Prints counts of filed, already held, and missing. The cursor and floor are
  untouched. Library form: a function a later transparent mode can call to
  resolve a handle it does not hold.
- `vivi sync pull --refs [--depth N]`: after the pull, scan the letters this
  invocation filed for references the mailspace does not hold, look those up,
  and file them. A reference is a value of an `X-Vivi-Depends-On` header or a
  whole-word 8-hex token in the subject or body. Depth defaults to 1 and is
  at most 3; each further hop scans only what the previous hop filed. At most
  500 tokens are looked up per invocation. A token that matches nothing is
  ignored.
- `vivi sync prune --older-than <age> [--dry-run]`: evict, from the local
  store only, letters bound to the server by the same policy as `vivi chat
  prune`: never open needs, tasks, or wants (however old), never starred or
  absorbed items, never local-origin letters (anything with no binding), and
  only letters dated before the cutoff. The row and its local state stay, the
  blob goes, and re-ingest reports the row as restored. It needs neither the
  server nor a key and never runs on its own. Restore with `resync`, `fetch`,
  or `pull --refs`.

**Measurement.** A synthetic 20,000-letter project built at store level
(letters of about 3 KiB, a mix of kinds, several recipients) times a full
list, a 7-day `after` window, a `kind` filter, a `to` filter, and a lookup of
100 handles, before and after paging, and the client's pull of 5,000 letters
through the in-process fake server. The numbers are reported, not asserted.

### Storage

**As running today (to be replaced).** Letters are sealed whole at rest
under the server key (AES-256-GCM, as VC-5) in per-project blobs named by
`content_sha256`, with `items.json` holding envelope rows (`sequence`,
`message_id`, `content_sha256`, `from`, `to`, `kind`, `appended_at`,
`from_prefix`). A project does not use a vivi mailspace on the server. This
was a misreading of item 10 (see the correction at the top).

**Target (decided items 14 and 16).** One vivi mailspace per project under
`DATA_DIR/projects/<id>/`, letters in the clear on an encrypted volume,
ingested verbatim by address with the same filing rules as the client's
pull, and queried through the mailspace's indexed database. The transport
fields (`sequence`, `appended_at`, `from_prefix`, the envelope `to`, and
`content_sha256`) are stored beside each letter and are never recomputed.
The wire in this document does not change.

**Migration (decided 2026-10-12): none.** Nothing on staging or production
is worth keeping. The switch is a clean break: projects created on the
sealed-blob store are recreated, and no replay is written.

### Token renewal

`POST /v1/projects/{id}/auth/token` `{name, challenge_id, signature_base64}`
→ 200 `{token, token_expires_at}`. The challenge comes from the existing
`/identities/challenge` and is keyed to the project and key fingerprint; the
signature must verify under the key bound to `name`. Any mismatch is 401.
Tokens last one hour and a registered name cannot re-register, so without
this endpoint an identity is locked out after an hour.

### Client

- Config is the `[sync]` table in `mailspace.toml`: `server`, `project`
  (the server-side project id), and `name` (this checkout's registered
  identity). `recipient` and `[sync.names]` are gone: the address carries
  both. The mailspace id is the top-level `id` in `mailspace.toml`, minted
  once when absent and never rewritten.
- `vivi sync join` registers `name`, the key, and the mailspace id.
  `vivi sync directory` lists the project's members and their address
  domains so a remote role can be addressed.
- `vivi sync` pushes every local letter that has no remote binding for the
  project, then pulls letters since the stored cursor. Push sends the stored
  file as-is. Pull verifies the hash and the envelope and ingests the bytes
  verbatim under the envelope id; an id or content already held is skipped.
- Envelope derivation on push: `kind` is the letter's work kind (`need`,
  `task`, `want`) or `mail`; `to` is the letter's own `To` and `Cc`
  addresses.
- Compose: a recipient `role@<prefix>.local` whose prefix is not this
  mailspace's is a remote recipient. The letter gets a sent copy and no
  delivered copy, and goes on the next push. A reply is addressed to the
  original `From`, so replying to a remote sender routes with no extra
  configuration.
- Pull verification, before anything is filed: the bytes hash to
  `content_sha256`; the envelope recipients equal the letter's own To/Cc;
  a prefix-shaped `From` domain equals the record's `from_prefix`.
- Pull filing, for each record not already held. Filing is by address and
  additive; it never depends on who authored the letter, because a letter can
  come back from another place that acts as this mailspace (a restore, a
  remote slot):
  1. One row per `To`/`Cc` address whose domain is this mailspace's, under
     that local role, with the role folder chosen by kind as for any
     delivered letter. A role this mailspace does not have is filed under
     the name anyway and reported, so adding the role later shows it.
  2. If the `From` address's domain is this mailspace's, also one `sent`
     row under the `From` role, exactly what a local send would have
     written.
  3. A letter with neither (a need, task, or want for another mailspace's
     role) is held under its first `To` address verbatim, so every member
     holds every work item without it landing in a local role's queue.
- Not synced: memos, drafts, trash, and local state (closing a need,
  marks). Closing a need as an appended record is a later unit.
- Out of this layer, by design: long-poll, and a reply to a letter minted
  before item 11 (its address is not routable; address the reply
  explicitly).
