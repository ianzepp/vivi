# Phase 04: hosted projects

This records what is built. Source of truth is the code: `src/hosted.rs`,
`src/projects/hosted_index.rs`, `src/mcp/live.rs`, and the tests named below.
The end-to-end check is `scripts/hosted-smoke.sh`.

## The idea

The hub's project mailspace and the server wrapping it are the same
mailspace. A **hosted project** is a vivi mailspace that already exists on the
server's machine (made and driven by the `vivi` CLI and its pump), and the
server serves it directly from its own `.vivi/`. There is no second mailspace
and nothing is copied or seeded. Phase 03's server-owned projects (a mailspace
under `DATA_DIR/projects/<id>/.vivi/`) are unchanged and still exist; a hosted
project is the other kind, told apart by `mailspace_root` in `project.json`.

## Registry-served projects

The server serves the roots that the host registry marks `serve = true`
(`vivi pump service add <root> --serve`; the pump supervisor reads the same
registry, so `--no-pump --serve` hosts a project without a pump).

- At boot, and whenever the registry file changes (polled every
  `hosted::POLL_INTERVAL`, 3 s), `hosted::reconcile` makes the served set match
  the registry.
- A root that cannot be served is refused with a log line and nothing else:
  its mailspace does not open, its `[sync] project` is not a canonical id, the
  id is already hosted from another root, the id names a server-owned
  project, or the creator member cannot be admitted.
- A root that leaves the registry stops being served (requests answer 404 as
  for an unknown project) and loses nothing: the host mailspace is untouched,
  and the server's data directory for it stays, so serving it again resumes.
  A root that moves keeps its project data and epoch.
- Server data stays where every project's lives: `DATA_DIR/projects/<id>/`
  (`project.json`, `transport.sqlite`, `identities.json`, `admissions.json`,
  `api_keys.json`, and `sync/state.json`). The server never creates, hardens
  or otherwise lays out anything under the host root, and no `.vivi` appears
  under the data directory.

## Id resolution and the creator member

The project id is `[sync] project` from the mailspace's `mailspace.toml`
(normalized: 25 Crockford digits, grouped in fives), else the id derived from
the mailspace UUID (its first 125 bits in the same encoding). The HTTP routes
accept either form.

The mailspace's own sync key (what `vivi sync key` prints, kept in
`.vivi/chat/identity.ed25519`) is admitted and registered as the project's
**creator member**, with the mailspace id and its address prefix, named by
`[sync] name`, else the mailspace name (sanitized; `postmaster` becomes
`hub`). This is what a client's `vivi sync create` then `join` would do, done
by the server on the host's behalf. It is idempotent. It is refused when the
same mailspace id is already registered under a different key, or the name is
taken by another key or mailspace id.

Because the host's key is the creator's, the host's own `vivi sync api-key
mint` works against the server it serves with no further setup.

## The indexer

The member-facing transport routes (`GET .../items`, lookup, fetch) read
`transport.sqlite`. For a hosted project that index is kept current with the
mailspace without copying any letter.

- **What is indexed.** A pass runs vivi's own push selection
  (`vivi::sync::push::push`) through an in-process transport whose append only
  checks that the blob store holds the letter's hash and inserts the
  transport row. Push then binds the letter in the mailspace under the project
  id, so later passes skip it. Selection is the mailspace's own `[sync] send`;
  the server never overrides it. The hub's host config sets
  `send = "addressed"`, so only letters addressed off the host are indexed.
  The default (`all`) would index every local letter.
- **When.** Every `hosted::INDEX_INTERVAL` (5 s) per project, and before a
  list, items, lookup or fetch read when the mailspace's change watermark
  moved since the last pass. A pass is skipped when another pass for the
  project is running, when the watermark has not moved, or when the host's CLI
  holds `.vivi/sync/sync.lock` (taken without waiting). The pass has no rate
  limit.
- **Cost model.** A pass whose watermark moved lists every message row and
  classifies each letter not bound to the project. It grows with the size of
  the mailspace, not with the number of new letters. Measured in release on a
  host mailspace of 5,000 sends (about 7,500 message rows), from the
  `CHANGELOG` and `tests/measure_index.rs` (run
  `cargo test --release --test measure_index -- --ignored --nocapture`): the
  first pass, indexing 2,500 letters, 0.76 s; a pass with the watermark
  unmoved, 0.3 ms; a pass for one new letter, 0.13 s.
- **Locking.** The pass holds the per-project pass mutex and the sync lock
  (both try-lock) for its run, and the append mutex only for each row insert.
  The HTTP append path never takes the sync lock.

## Member appends

An append to a hosted project (`PUT .../items/{id}`) is filed into the host
mailspace with `vivi::sync::pull::file_records`, exactly as a client pull files
a wire record: verified, filed under the addressed role's folder, bound to the
project at the new sequence, reply-linked, item events folded. The pull-style
state lives in `DATA_DIR/projects/<id>/sync/state.json`. The transport row
follows, so the indexer never picks the letter up again. A record the filer
refuses (a From claiming another prefix, an envelope disagreeing with the
letter's To and Cc, an id naming different content) is an append error and
writes nothing to the index. The per-member rate limit (30 sends per minute
per member) applies; the indexer has none. The old `403 project_read_only`
refusal is gone.

## MCP on a hosted project

`POST /v1/projects/{id}/mcp` routes per call. An **API key** on a hosted
project acts on the host mailspace through the `vivi` library as a vivi
identity. An identity-token caller, and every server-owned project, keep the
transport tools unchanged (a member token reads the index, never the live
mailspace).

- **Identity mapping.** The key's `identity` (set when the key is minted:
  `POST /v1/projects/{id}/keys` `{label, identity?}`, by a creator or member
  identity token; the host's `vivi sync api-key mint` mints a key without
  one), else `HOSTED_DEFAULT_IDENTITY` (default `operator`). It must be a role
  of the hosted mailspace, or the call fails naming the identity. A key cannot
  mint keys; a project holds at most 32 live keys.
- **Writes** call the CLI's entry points, so authority rules and errors are the
  library's: `send` (`Mailspace::send`; roles `needs`, `tasks`, `wants`,
  `inbox`; `chat` is refused), `reply`, `done` and `reopen`
  (`move_item_outcome`), `memo` (`save_memo`), and `mark` / `unmark`
  (`set_marks`, on a need or want the acting identity holds). Writes are rate
  limited per acting identity.
- **`for`.** `list` reads the acting identity's folders, as
  `vivi need list --for <identity>` does; its `for` argument names another
  role or `all`. `board` takes `for` the same way.
- **`pump_control`.** Operator-only: `action` is `pause`, `resume`, `stop`, or
  `start` (the CLI's `start` is `resume`), through
  `vivi::pump::control::set_project_mode`; the answer is the control state
  (`mode`, `shutdown_requested`). The key must be minted by the host's own
  member (the creator whose mailspace id is the hosted mailspace's) and act as
  a role of kind `operator`; a member token or any other key is refused.
- **`pump_gate`.** Operator-only, with `pump_control`'s authorization (one
  shared check): `action` is `show` or `set`; `set` takes a `command` and
  writes `[pump.gate] command` in the host's `.vivi/mailspace.toml` through
  `vivi::pump::gate_config::set_command` (format-preserving, under the
  mailspace config lock); a blank command is refused by the library. Both
  answer `{command}` (`null` when unset or blank). `set` is rate limited as a
  write. This is the only config the server edits: pools, roles and free-form
  config are deliberately not exposed, because a member who could repoint a
  pool's `remote_url` could send code elsewhere.
- **Live reads.** `overview`, `list` (kind, status, `to`, `from`, `marks`,
  `for`; default 50, at most 100), `show`, `thread`, `graph` (the backlog
  graph) and `board` (`Mailspace::board` as JSON, host root omitted) query the
  mailspace store at call time. Each call opens the mailspace afresh, so a
  need created by the CLI or the pump is visible immediately and no index pass
  is involved.
- **Shapes** differ from the transport tools: items carry `role` (folder),
  `date` (not `appended_at`), `marks` and `last_event`; `send` and `reply`
  answer with `handle` (the recipient's copy), `sent`, `delivered`, `remote`;
  `done` and `reopen` answer with the library's `event`, `reported` and
  `joined_needs`.

Tests: `tests/mcp_hosted.rs` (each tool against the library), `tests/hosted.rs`
(registry reconciliation), `tests/hosted_index.rs` (the indexer and appends).

## Epoch

Every project has a log epoch: a random u32 (1 to 4294967295) minted once and
stored in `project.json`, sent as `epoch` at the top of each list response (and
the chat poll). A hosted project gets one when it is first served and keeps it
across a root move or a registry removal and re-add. The vivi client binds
pulled letters under it and treats a change as a server reset.

## Limits and v2 items

Built as above and deliberately small. Known limits:

- **Select grows with the mailspace.** The pass lists every message row; there
  is no incremental select, so a very large mailspace pays for every pass that
  has work. A watermark-keyed incremental select is a v2 item (it needs a vivi
  change).
- **No rebuild after data-dir loss.** The binding of a letter to the project
  lives in the mailspace; the index row lives in `transport.sqlite`. Losing the
  server's data directory loses the index, the members, the keys and the epoch,
  and the mailspace still believes its letters are indexed, so they are not
  re-indexed. A rebuild (clear the project binding and re-run selection, with
  the members re-admitted) is a v2 item. Back up `DATA_DIR`.
- **`appended_at` is the index time.** The transport row's timestamp is when
  the indexer saw the letter (monotone per project), not when the letter was
  composed; the letter's own `Date` header is unchanged.
- **Sender is the creator.** An indexed letter is attributed to the creator
  member, as the host's own mailspace is the one member that sends it.
- **One host, one writer.** Several processes share the host's files (the CLI,
  the pump, the server); the indexer yields to the sync lock, and MCP writes
  use the library's own locking. Nothing else coordinates them.

## Creating a project over MCP

`POST /v1/host/mcp` is the host's own MCP endpoint (Streamable HTTP, the same
JSON-RPC plumbing as the project endpoint). It has one tool, `create_project`.

- **Auth.** The Bearer must equal `HOST_OPERATOR_TOKEN` (constant-time
  compare). There is no host identity beyond that token. With the variable
  unset the route answers 404 and does nothing; a missing Bearer is 401; any
  other Bearer, including a project member token or a project API key, is 403.
- **Config.** `HOST_PROJECTS_ROOT`, `HOST_LANES_ROOT`, `HOST_GIT_URL_PREFIX`,
  and optionally `HOST_DEFAULT_POOL`. A missing required one makes the tool
  answer with an error naming the variable.
- **Call.** `create_project {name, gate?, branch?, pool?, initial_commit?,
  pump?, serve?}` runs `vivi::project_create::create` on a blocking thread
  (git repository, mailspace with the starter pump config, host registry
  entry). It rolls back on failure and refuses a name that exists.
- **Served at once.** The registry watcher polls every 3 s, so a project
  registered with `serve = true` is served within a few seconds even without
  this tool. `create_project` does not wait: it runs `hosted::reconcile_registry`
  itself, so the project answers at `/v1/projects/{id}/mcp` before the tool
  returns. `serve = false` registers the project but leaves it unserved.
- **Answer.** The `ProjectCreated` fields (`project_root`, `git_url`,
  `repo_path`, `mailspace_id`, `initial_commit`, `readiness`) plus `served`,
  `project_id`, `mcp_path`, `member`, and `operator_key`. The key is minted
  in-process for the host's own member with identity `operator`, the shape
  `pump_control` and `pump_gate` require. It is shown once; the server keeps
  only a digest, and never logs it. If serving failed, `served` is false and
  `serve_error` says why.
