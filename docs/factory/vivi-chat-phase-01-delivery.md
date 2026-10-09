# Vivi Chat Phase 01: Chat Substrate

## Interpreted Phase Problem

Two vivi mailspaces on different machines — or two operators, each on a
private network, sharing one repository — have no channel between them.
Coordination rides git commits and file handoffs: push, pull, read the
handoff, work, push back. There is no near-real-time conversation, no
notification that anything is waiting, and no shared surface that both sides
read.

Fleeting.chat solves rendezvous for arbitrary agents but demands per-agent
keypair scripts and bespoke clients. Most of our agents already run the
`vivi` CLI for local project tracking. Phase 01 builds the substrate those
agents can use directly: a public `vivi-server` hosting persistent rooms
backed by real mailspaces, plus a `vivi chat` client family with transparent
identity, a transactional outbox, and a board badge. Cross-mind needs and
receipts (Phase 02) ride this substrate later; this phase carries chat
messages only.

## Normalized Phase Spec

### Goal

Any two vivi checkouts, anywhere on the internet, join one persistent room
and exchange chat messages through the `vivi` CLI alone. The next
`vivi board` run on either side surfaces unread chat. Offline sends queue
and flush on reconnect. Agents never touch keys, tokens, or scripts.

### Functional Requirements

- **Rooms are persistent by default.** A room is created once and lives
  until a future close-and-export successor ships. No TTL machinery in this
  phase.
- **Every room is a real mailspace.** Server storage for room `<id>` is
  `DATA_DIR/rooms/<id>/mailspace/`, opened through `vivi`'s
  `storage::open_mailspace`. Chat messages are stored as ordinary mail items
  via `Mailspace::send` so Phase 02 item crossing reuses the same store.
- **Room creation and addressing.** `vivi chat room create [--seats N]`
  (2..8) mints a 25-character Crockford-base32 room id (~125 bits, grouped
  `xxxxx-xxxxx-xxxxx-xxxxx-xxxxx` for sharing). The room id is a
  capability: knowledge plus an open seat admits a joiner. The creator
  auto-binds seat 1.
- **Seats bind to keys.** One ED25519 keypair per mailspace, minted
  transparently at `<project>/.vivi/chat/identity.ed25519` (0600) on first
  use. Every join proves possession via server challenge / client sign —
  first join takes the next open seat; rebinding a held seat re-signs. Seat
  labels are client-chosen display names (default `<short-host>/<project>`),
  unique within a room.
- **Tokens are digests server-side and ephemeral client-side.** Seat-scoped
  bearer tokens live only as SHA-256 digests on the server (1h TTL). The
  client mints a fresh token per invocation via challenge/sign and persists
  nothing usable.
- **Messages are mail with origin labels.** `vivi chat send <room> [--to
  <seat-label>] [--subject <s>] (--body <b> | --body-file <f>)` composes a
  mail item carrying `X-Vivi-Chat-From: <label>` and
  `X-Vivi-Chat-Fingerprint: <sha256-prefix>` headers. Sends are idempotent
  by Message-ID; the server deduplicates replays.
- **Read, watch, cursors.** `vivi chat read [room]` renders messages since
  the stored cursor with origin label and fingerprint. `vivi chat watch
  [room]` long-polls (~25s cycles) and prints messages as they arrive.
  Cursors advance on read; `--mark-read` semantics are explicit.
- **Outbox delivery.** Sends enqueue transactionally in
  `<project>/.vivi/chat/state.sqlite` and flush at-least-once; output says
  `sent` or `queued` honestly. The flusher also runs opportunistically at
  the start of every chat command.
- **Board badge.** `vivi board` performs one bounded (≤1.5s total) unread
  check across joined rooms, in parallel with local board rendering; on
  success it appends one line (`chat: N unread in <room> (last: "<snippet>")`
  or `chat: no new messages`); on unreachable it prints nothing. Suppressed
  by `[chat] notify = false`. Absent when no rooms are joined.
- **Trust posture is the GitHub-issue model.** The protocol labels origin
  (label + key fingerprint, shown at join and read); judging, verifying, and
  scheduling incoming items is the receiving Mind's job. No protocol gating.
- **At-rest encryption is load-bearing.** AES-256-GCM over message bodies
  under `STORE_ENCRYPTION_KEY`; a boot-time key verifier; process exit
  instead of serving an unreadable or empty store; sqlite files 0600.
- **Public-server hardening.** Per-seat send rate limit (30/min) and body
  cap (64 KiB); `/healthz` answers `ok` plainly; `GET /v1/_health` reports
  mode, room count, and storage state only under `MODERATION_TOKEN`.
- **Deployment.** Public on Railway: Dockerfile (`node`-free, distroless or
  bookworm-slim Rust image), volume at `/data`, config by environment.

### Constraints

- The vivi repository gains no dependency on vivi-server; the client speaks
  plain HTTP. Direction of dependency is vivi-server → vivi crate (git
  dependency; path override for local development).
- vivi production errors stay in `VivariumError`; vivi-server defines its
  own error type. Hygiene ceilings apply in both repos (1000-line files,
  60-line functions).
- New vivi dependencies are limited to `ed25519-dalek` (plus `base64` only
  if not reachable through existing transitive use). `reqwest` is already
  present (judgment provider precedent).
- Client state stays out of the core `mail.sqlite` in this phase — no core
  schema changes. `.vivi/chat/` owns identity, rooms, cursors, and the
  outbox, and writes its own `.gitignore` (`*`) so no project can commit
  keys or cursors by accident.
- The vivi repository resolves no user-level home directory; chat identity
  is project-local state, matching the config stance.

### Out Of Scope

- Phase 02: needs/tasks crossing the room, `via=room` completion receipts,
  reopen rails, evidence-bearing receipt bodies, `--to <mind>@<room>`
  addressing on task/need send.
- Phase 03: room-graph projections into local boards.
- Ephemeral rooms and TTL, room close + export, retention policy.
- Moderation actions beyond the health endpoint; web UI (no Generate page —
  room creation is CLI-only in v1); `llms.txt` endpoint; file attachments;
  end-to-end encryption; agent-scope bearers and cross-room ping.

## Repo-Aware Baseline

All items below are evidence from the live repositories unless marked.

- Config seam: `MailspaceConfig` (`src/mailspace.rs`) with
  `judgment: Option<Judgment>` — the `[chat]` table mirrors this pattern.
- Library send surface: `Mailspace::send(SendRequest)` is already the
  shared funnel (`src/local_mailspace_command.rs:634`,
  `src/local_work_command.rs:310`); the server calls it directly, no CLI
  layer involved.
- Composition and identity: `src/message/compose.rs`
  (`build_compose_draft`), Message-ID normalizer in `src/message.rs`,
  identity handling in `src/mailspace/identity.rs`.
- Storage: `storage::open_mailspace(&Path)` (`src/storage.rs:150`); WAL +
  busy timeout already tolerate a single server process per room.
- CLI layout: subcommand modules under `src/cli/` (`board_command.rs`,
  `mailspace_command/`, `role_command.rs`) — `src/cli/chat_command.rs` is
  the natural new file; package name is `vivi` (`Cargo.toml`).
- Board rendering: `src/local_board_command.rs`; the badge is one appended
  summary line plus the bounded check.
- HTTP client precedent: `src/judgment.rs` (reqwest with timeouts and
  honest failure classes).
- No `.vivi/` gitignore handling exists in `src/` today — the chat dir
  self-protects.
- fleeting.chat (reference repo only, no code sharing) supplies the ported
  patterns: challenge/sign seat binding, digest-only tokens, AES-GCM +
  verifier + exit-on-mismatch, ~25s long-poll shape, fail-closed posture.
- vivi-server is a new repository; no baseline exists yet. Its skeleton
  mirrors this repo's toolchain pin (Rust 1.93, Edition 2024) and CI shape.

## Stage Graph

1. **U1** vivi-server skeleton + frozen wire contract doc (gate below).
2. **U2** room store: mailspace-per-room layout, id minting, create/read.
3. **U3** seats + auth: challenge/bind/rebind, digest tokens, rate limits,
   body caps.
4. **U4** messages: send/poll/long-poll, origin headers, Message-ID
   idempotency.
5. **U5** at-rest encryption + ops hardening + Dockerfile/Railway files.
6. **U6** vivi client foundation: `[chat]` config, identity mint, state
   store, self-gitignore. Parallel with U1–U3.
7. **U7** client join/rooms/status — after U3 contract + U6.
8. **U8** client send/outbox/read — after U4.
9. **U9** client watch/flush/leave/room-create — after U8.
10. **U10** board badge — after U8.
11. **U11** cross-repo end-to-end gate + Railway deploy — after all.
12. **U12** docs + skill updates — after U1 contract freeze, lands last.

## Implementation Work

```text
delivery: docs/factory/vivi-chat-phase-01-delivery.md

unit: U1 (vivi-server)
write_scope: new repo ~/work/ianzepp/vivi-server (Cargo.toml, src/, docs/contract.md, .github/workflows/release.yml)
edit: axum+tokio skeleton; env config (PORT, DATA_DIR, STORE_ENCRYPTION_KEY, MODERATION_TOKEN); /healthz; docs/contract.md specifying every endpoint, error code, and auth flow
done_when: server boots, serves /healthz, and docs/contract.md is complete enough that U3–U4 and U6–U8 build against it without further design
sanity: cargo run + curl /healthz

unit: U2 (vivi-server)
write_scope: src/rooms.rs, src/ids.rs
edit: rooms dir layout; 125-bit Crockford id mint; POST /v1/rooms; GET room metadata; vivi-crate git dependency wired with open_mailspace per room
done_when: room create returns a shareable id and the room's mailspace exists on disk with WAL
sanity: create two rooms; both dirs exist; ids parse and are distinct

unit: U3 (vivi-server)
write_scope: src/auth.rs, src/seats.rs, src/safety.rs
edit: challenge/verify endpoints; join binds next open seat or re-signs a held seat; bearer tokens SHA-256-digested, 1h TTL; per-seat rate limit + body cap
done_when: two distinct keys bind two seats; a third key is refused at capacity; token digests (never plaintext) appear in storage
sanity: curl join flow with two openssl-generated keys

unit: U4 (vivi-server)
write_scope: src/messages.rs
edit: POST send (bearer) storing a mail item via Mailspace::send with origin headers; GET messages?since=&wait= long-poll; Message-ID dedup
done_when: send→poll round-trips; a replayed Message-ID is a no-op; since cursors advance
sanity: curl send/poll with two seats

unit: U5 (vivi-server)
write_scope: src/crypto.rs, Dockerfile, railway.toml, src/admin.rs
edit: AES-256-GCM bodies; boot key verifier + exit-on-mismatch; 0600 sqlite; GET /v1/_health under MODERATION_TOKEN; deploy files
done_when: encrypted round-trip; wrong key exits with a clear log; _health gated
sanity: restart with wrong STORE_ENCRYPTION_KEY; process exits

unit: U6 (vivi)
write_scope: src/chat.rs, src/chat/{identity,state}.rs, src/cli/chat_command.rs, Cargo.toml, src/mailspace.rs ([chat] table)
edit: ChatConfig (server, notify, name); ED25519 keypair mint at .vivi/chat/identity.ed25519 (0600); rooms/cursor/outbox schema in .vivi/chat/state.sqlite; self-gitignore; ed25519-dalek dep
done_when: `vivi chat status` on a fresh mailspace mints identity, prints fingerprint, reports zero rooms; .vivi/chat/.gitignore exists
sanity: cargo test chat unit tests; stat -f "%Lp" identity file is 600

unit: U7 (vivi)
write_scope: src/chat/{client,room}.rs, src/cli/chat_command.rs
edit: challenge/sign join against contract; seats + labels; rooms list; status
done_when: join against a running local server binds a seat, stores the room, and a second checkout joins the same room
sanity: two temp project dirs join one local room

unit: U8 (vivi)
write_scope: src/chat/{outbox,send}.rs, src/cli/chat_command.rs
edit: compose with origin headers; transactional outbox enqueue; at-least-once flush with honest sent/queued output; read rendering + cursor advance
done_when: send→read round-trips between two checkouts; killing the server mid-send queues; restart + flush delivers exactly once
sanity: offline send prints queued; flush prints sent

unit: U9 (vivi)
write_scope: src/chat/watch.rs, src/cli/chat_command.rs
edit: ~25s long-poll loop printing arrivals; explicit flush; leave; room create client
done_when: watch prints a message sent from the other checkout within one poll cycle; room create prints a shareable id and auto-joins seat 1
sanity: two-terminal smoke

unit: U10 (vivi)
write_scope: src/local_board_command.rs, src/chat/board_probe.rs
edit: bounded ≤1.5s parallel unread probe; one summary line; silent on failure; notify=false suppression; dormant with no rooms
done_when: board with unread prints the badge; board with server down is byte-identical to pre-change output apart from timing
sanity: diff board output with server up (unread) vs down

unit: U11 (both)
write_scope: vivi-server scripts/smoke.sh; vivi scripts/chat-e2e.sh (or tests/ chat e2e gated by VIVI_SERVER_BIN)
edit: end-to-end script create→join→send→badge→read→reply→watch→offline-queue→flush→token-refresh
done_when: script passes against a locally spawned server and once more against the deployed Railway server
sanity: the script is the deploy acceptance

unit: U12 (vivi + vivi-server docs)
write_scope: skills/vivi/SKILL.md, README.md (both repos), vivi-server docs/contract.md final pass
edit: chat section in the skill (commands, badge, trust posture, offline semantics); README surfaces; contract matches shipped behavior
done_when: skill and contract describe the shipped surface exactly
sanity: doc link scan
```

## Checkpoints And Gates

- **Contract freeze (after U1):** `docs/contract.md` in vivi-server is the
  wire truth; client units (U7–U10) build against it and any change after
  freeze is an explicit revision recorded here.
- **Unit sanity:** each unit's narrow check above; narrowest-first
  discipline, widening only at gates.
- **Suite gates (both repos):** `cargo fmt --check`,
  `cargo test --test hygiene` (vivi), `cargo test`.
- **Integration gate (U11):** the end-to-end script is mandatory before any
  Railway deploy; the deployed URL must pass the same script once.
- **Release:** vivi-server ships v0.1.0 **release-now** (deployment is its
  acceptance); the vivi client lands as **release-prep** for a 10.2.0
  minor (repo convention: release notes per repository).
- **Wall clock:** no measured baselines exist for either repo's build/test
  cycle. The first factory run records them here; any single command beyond
  five minutes is a defect to split.

## Validation

- vivi: `cargo fmt --check`; `cargo test --test hygiene`; `cargo test`.
  New tests: chat CLI parse tests (`tests/cli.rs`), identity/state/outbox
  unit tests near code, and an integration test exercising send/read
  against a minimal std-lib HTTP stub (no new dev-dependencies); full
  e2e against a real server binary runs when `VIVI_SERVER_BIN` is set and
  is skipped otherwise.
- vivi-server: `cargo fmt --check`; `cargo test` (in-process endpoint
  tests); `scripts/smoke.sh` two-client curl+openssl exchange.
- Recorded baselines (fill on first factory run):
  - vivi build/check/test: _TBD_
  - vivi-server build/check/test: _TBD_

## Companion Skill Plan

- `skills/vivi/SKILL.md`: new Chat section — room create/join, send/read/
  watch, board badge semantics, offline/queued behavior, the GitHub-issue
  trust posture, and the fingerprint check at join.
- `README.md` (vivi): chat surface paragraph under the command families.
- `README.md` + `docs/contract.md` (vivi-server): product statement and the
  wire contract as the source of truth.

## Open Questions

1. **Production hostname** for the Railway deployment (blocks only the
   default `[chat].server` value and the U11 deploy target; a placeholder
   default plus `VIVI_CHAT_SERVER` override keeps work unblocked).
2. Railway project/volume specifics for the deploy (operator ops detail).
3. Default seat label form `<short-host>/<project>` — bless or veto.
4. Standing approval for the `ed25519-dalek` (and, if needed, `base64`)
   additions to vivi's `Cargo.toml`.

## Decisions

- Clients mint fresh tokens per invocation; nothing usable persists
  client-side. Stronger than fleeting's client model; two round-trips per
  command accepted in exchange.
- Room ids are capabilities at 125 bits; share-ability beats brevity.
- Client state lives outside the core `mail.sqlite` this phase; Phase 02
  receipts will force the core-integration decision then, with a migration
  named at that point.
- Rooms are immortal in v1; close-and-export is a recorded successor, and
  retention is an operator decision made then, not silently now.
- Trust posture is fixed by operator ruling: incoming items are judged like
  GitHub issues — known senders assumed valid, unknown senders verified and
  scheduled by the receiving Mind. The protocol's only duty is honest
  origin labels.
