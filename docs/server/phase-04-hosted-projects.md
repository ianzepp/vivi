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
  (`set_marks`, on a need or want the acting identity holds), and `depend`
  (`set_dependencies`: `add` and `remove` handles on a need, task, or want
  the acting identity may close; each change is a `dependency-added` /
  `dependency-removed` letter that reaches every member, and the answer lists
  the item's prerequisites as they stand). Writes are rate limited per acting
  identity.
- **`for`.** `list` reads the acting identity's folders, as
  `vivi need list --for <identity>` does; its `for` argument names another
  role or `all`. `board` takes `for` the same way.
- **`pump_control`.** Operator-only: `action` is `pause`, `resume`, `stop`, or
  `start` (the CLI's `start` is `resume`), through
  `vivi::pump::control::set_project_mode`; the answer is the control state
  (`mode`, `shutdown_requested`). The key must be minted by the host's own
  member (the creator whose mailspace id is the hosted mailspace's) and act as
  a role of kind `operator`; a member token or any other key is refused.
- **`pump_rungs`.** Operator-only, with `pump_control`'s authorization (one
  shared check, `show` included): `action` is `show` or `set`, with an optional
  `target` (a pump target id; without it the workspace table, `[pump.rungs]`).
  `set` takes a `rung` (`task`, `need`, `main` or `release`) and a non-blank
  `command`, and goes through `vivi::pump::config::RungsConfig::set`
  (format-preserving, under the mailspace config lock; `[pump.rungs]`, or
  `[pump.targets.<id>.rungs]` with `target`). `show` ignores `rung` and
  `command`. Both answer the table as it stands afterwards, parsed:
  `{target, task, need, main, release, timeout, pools}`, with `null` for an
  unset command; with `target` it is that target's effective table (its own
  rung command, else the workspace one). Errors: a missing `rung` or `command`
  on `set`, an unknown rung, a blank command (the library refuses these), and
  an unknown target (``unknown pump target `<id>` ``). `set` is rate limited
  as a write. The tool edits rung commands only: `timeout` and `pools` are
  shown but not settable here, and a set rung with no rung `pools` leaves the
  config invalid until the host sets `pools` (rung jobs cannot dispatch). This
  and `pool_bands` are the only config the server edits: pools, roles, remotes
  and free-form config stay with the host, because a member who could repoint a
  pool's `remote_url` could send code elsewhere.
- **`pool_bands`.** Operator-only, with `pump_control`'s authorization (one
  shared check, `show` included): `action` is `show`, `set` or `unset`, with
  `pool`, and `band` (`P0` to `P5`) for set and unset, `model` and optional
  `reasoning` (default `xhigh`) for set. Edits `[pools.<pool>.bands]` in the
  host's `.vivi/mailspace.toml` through `vivi::mailspace::config_edit`
  (`set_band`, `unset_band`; format-preserving, under the mailspace config
  lock); `set` replaces the band's entry with one choice. `set` is refused for
  a model not in the pool's `harness.models` (the error names the list), an
  unknown pool (the error names the pools) or a malformed band or reasoning.
  Every action answers `{pool, bands, harness_models}`. `set` and `unset` are
  rate limited as writes. Only that one pool's bands table is reachable:
  bands select among models the host already allowed and cannot repoint a
  remote or a harness, so the rule that pools, roles and other config stay
  with the host (see `pump_rungs`) holds.
- **`pump_send`, `pump_list`, `pump_show`.** Operator-only, with
  `pump_control`'s authorization (one shared check): the pump record verbs
  `vivi pump send|list|show`, through `Mailspace::pump_send`, `pump_list` and
  `pump_show`. `pump_send` takes `need` (required) and optional `body` and
  `from`, files one open record the pump reads on its next tick, answers the
  record, and is rate limited as a write; the library refuses an unknown or
  non-need handle, a need not addressed to the pump identity, and a need that
  already has an open record. The body is literal text: the CLI's `-` and
  `@path` forms are not accepted, since they would read the server's files.
  `pump_list` takes `need`, `status` (`open` by default, `done`, `all`) and
  `limit` (default 50, at most 100) and answers `{records, total, truncated}`,
  oldest first, the newest `limit` when more match. `pump_show` takes `handle`
  and answers the record with its `body`.
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
- **Call.** `create_project {name, branch?, pool?, initial_commit?, rungs?,
  pump?, serve?}` runs `vivi::project_create::create` on a blocking thread
  (git repository, mailspace with the starter pump config, host registry
  entry). It rolls back on failure and refuses a name that exists. `branch`
  is the new repository's default branch (default `main`). `rungs` is an
  object of rung commands `task`, `need`, `main`, `release` (non-blank
  strings) plus optional `timeout` (whole seconds, at least 1) and `pools` (an
  array of pool names); any other key is refused. It is parsed and validated
  before anything is created, then written into `[pump.rungs]` after the
  create (commands through `RungsConfig::set`, `timeout` and `pools` through
  `config_edit`) and the answer's `readiness` is recomputed. A write failure
  after the create does not roll the project back: the answer carries
  `rungs_error` naming what was not written. There is no `gate` argument.
- **Served at once.** The registry watcher polls every 3 s, so a project
  registered with `serve = true` is served within a few seconds even without
  this tool. `create_project` does not wait: it runs `hosted::reconcile_registry`
  itself, so the project answers at `/v1/projects/{id}/mcp` before the tool
  returns. `serve = false` registers the project but leaves it unserved.
- **Answer.** The `ProjectCreated` fields (`project_root`, `git_url`,
  `repo_path`, `mailspace_id`, `initial_commit`, `readiness`) plus `served`,
  `project_id`, `mcp_path`, `member`, and `operator_key`. The key is minted
  in-process for the host's own member with identity `operator`, the shape
  `pump_control` and `pump_rungs` require. It is shown once; the server keeps
  only a digest, and never logs it. If serving failed, `served` is false and
  `serve_error` says why. `rungs_error` is present only when seeding `rungs`
  failed after the project was created.
