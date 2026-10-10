# Phase 03 — Project mailspace hub

Status: **DRAFT**, recorded 2026-10-10 from a design conversation with the repo
owner. Nothing here is built or scheduled. Admission by introduction and the
reserved `postmaster` identity were decided in a follow-up conversation the
same day. Each item is marked **Decided**, **Proposed** (an idea the owner
has not confirmed), or **Open** (deliberately undecided). Build work from
this draft is tracked as needs and wants in the project mailspace
(`.vivi/`, roles `mind` and `hand`); this document is the design record
and the rulings, and the backlog holds the live state. Until this phase
is built, the room-based wire contract in [`contract.md`](contract.md) is
the shipped behavior.

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
   - The domain part of an email address has no meaning on the wire. Vivi
     supports it because vivi can also send external email, but in a
     project-scoped view `mind@pharos` and `mind-on-pharos` are the same thing.
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

## Notes for the client side (not decided)

- Local addresses are `<identity>@<mailspace-name>.local` today, so every checkout
  of one project has its own `mind@<project>.local`. A local role must map to the
  registered wire name, and an item from a remote identity needs a local address
  form (the chat client uses `<label>@<room>.vivi-room`).
- Replying to a remote sender must route through the server. This is the failure
  vivi recorded as its open item 3 (a pulled item's sender is not a local role).
- The local folders are a fixed set (inbox, sent, drafts, trash, archive, tasks,
  needs, wants, memos, done); no new folder is needed if chat is plain mail.

## Proposed

- Closing a need is an appended event. Every client derives open or closed by
  folding events in the server's append order, so everyone converges. A receipt
  carrying evidence can still be a normal mail, but it is not the mechanism.
- Mail to an unpublished identity is rejected at submit time, with a hard error to
  the sender.
- Identity names use the restricted character set roles use today (letters,
  digits, `.`, `-`, `_`, with a length cap). Names are immutable once registered;
  a rename is a new identity.
- A directory call lists a project's members and roles.
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
- What supplies the sequence cursor (for example the per-store event log vivi
  already keeps) and how concurrent appends are serialized per project.
- Whether the wire stays HTTP or speaks IMAP/SMTP, and whether an item travels as
  its raw `.eml` or inside an envelope.
- Migration of the rooms and messages already on the staging and production
  services, and how long the rooms API stays alongside the new one.

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
