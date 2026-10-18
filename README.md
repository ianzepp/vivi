# Vivi

Vivi is a local-first project mailspace for LLM agents. It keeps the
coordination state of a project (tasks, needs, wants, mail, memos, rules,
roles, goals and executable work graphs) in a `.vivi/` directory inside the
project, and exposes it through the `vivi` command line.

Agents working in one repository need somewhere to record what must happen,
what they need from each other and what they learned, in a form that survives
restarts, context compaction and hand-offs between different models. Vivi is
that place. A mailspace needs no server and works offline on one machine.
Optional parts build on it: `vivi sync` replicates letters through a project
server, `vivi pool` runs tasks on remote machines, and `vivi pump` is an
unattended dispatcher built on pools.

Vivi does not run your agents. The agents (a "Mind" that routes work, "Hands"
that do it) are yours; Vivi is their shared memory and record. The exceptions
are `vivi pool dispatch` and `vivi pump`, which start work on machines you
configured.

Vivi is one of three sibling repositories:

| Repo | Owns |
| --- | --- |
| [`vivi`](https://github.com/ianzepp/vivi) (this repo) | Project mailspaces, roles, goals, work graphs, pools, the pump, the `vivi` binary, and the `vivi`, `tugboat` and `speedboat` agent skills |
| [`vivi-mail`](https://github.com/ianzepp/vivi-mail) | The email half: IMAP, SMTP, the local email archive |
| [`vivi-pty`](https://github.com/ianzepp/vivi-pty) | The project-scoped PTY runtime adapter |

They share no code and no data.

## Install

Release binaries are attached to this repository's
[GitHub releases](https://github.com/ianzepp/vivi/releases), the only
distribution channel. The installer picks the archive for your platform and
falls back to `cargo install` from the release tag when none exists:

```sh
curl -fsSL https://raw.githubusercontent.com/ianzepp/vivi/main/install.sh | bash
```

It reads `VIVI_VERSION` (release tag, default the latest), `VIVI_INSTALL_DIR`
(default `~/.local/bin`), `VIVI_REPO` (default `ianzepp/vivi`) and
`VIVI_BIN_NAME` (default `vivi`). Release archives are built for
`aarch64-apple-darwin`, `x86_64-apple-darwin` and `x86_64-unknown-linux-gnu`.

From source (`rust-toolchain.toml` pins Rust 1.93):

```sh
git clone https://github.com/ianzepp/vivi.git
cd vivi
cargo build --release      # target/release/vivi
cargo install --path .     # or install it onto your PATH
vivi --version
```

## Quick start

Everything here runs offline, in any directory.

```sh
cd /path/to/your/project
vivi mailspace init                           # creates .vivi/
vivi role add mind --kind mind                # the seat that routes and accepts work
vivi role add hand-1 --kind hand --harness subagent

# A need (the requirement) and a task under it. A "done_when: ..." line in a
# body gives `vivi step` something to check; vivi warns when a need has none.
vivi need send --from mind --to hand-1 --subject "Add a greeting" \
  --body $'Print hello.\n\ndone_when: running it prints hello'
vivi task send --from mind --to hand-1 --subject "Write hello.sh" \
  --body $'Print hello.\n\ndone_when: running it prints hello'
vivi board                                    # actionable work per role
vivi boot                                     # the whole project frame in one read

# Each send prints the handle it created. Bind the task to the need, so the
# need completes when the task is accepted.
vivi need bind <need-handle> <task-handle>

vivi task done <task-handle> --for hand-1     # a worker's "done" is a report
vivi step                                     # lists it as awaiting_acceptance
vivi task done <task-handle> --for mind       # the author (or a mind) accepts
```

After the last command the task is done, the bound need completes ("all bound
units accepted"), and `vivi step` reports no exceptions.

Handles are the first eight hex characters of a message id; any unambiguous
prefix works. `--project <root>` names the project from anywhere and is accepted
before or after the subcommand; without it Vivi walks up from the current
directory to the nearest `.vivi/mailspace.toml`. Only `mailspace init` creates
`.vivi/`: every other command fails with "No Vivi mailspace found" rather than
make one. `-v/--verbose` turns on logging.

## Core concepts

**Mailspace.** The `.vivi/` directory of one project: a config file, one SQLite
database and content-addressed message blobs. It ignores itself (Vivi writes
`.vivi/.gitignore` containing `*`), so nothing in it is committed. The
mailspace has an id (a UUID minted on first open); its first eight characters
are the domain of every address it mints, for example `hand-1@65d72e54.local`.

**Items.** Work and notes are messages with lifecycles and stable handles:

| Item | Meaning | Verbs |
| --- | --- | --- |
| `mail` | Plain local message | send, reply, list, show, thread, absorb |
| `task` | A unit of work for one role | send, from, done, reopen |
| `need` | A prioritised requirement, usually the parent of tasks | send, bind, done, reopen, mark |
| `want` | A later idea; never dispatched until promoted | send, set-priority, promote, done/drop, mark |
| `memo` | Durable per-role memory | save, list, search, show, delete |
| `rule` | A project rule every seat can read | save, list, supersede, retire |

Tasks, needs and wants accept `--depends-on <handle>` (repeatable). `absorb`
seals a record so it can no longer change.

**Who may finish work.** A worker's `done` is a report. The item's author, or a
role of kind `mind` or `operator`, accepts it; acceptance completes the work,
unlocks dependents and is recorded as an event letter that other checkouts apply
when they sync. `vivi step` lists reported work as `awaiting_acceptance`.

**Roles.** A role is a named seat with a mailbox and metadata: `kind` (`hand`,
`head`, `mind`, `operator`, `steward`, or freeform), `status`, `labels`,
`harness`, `provider`, `model`, `thinking`, a `pid`/`host` binding the live
process sets for itself, a `cadence` (the maximum silence between its outbound
signals) and a `charter` (its standing prompt, stored at
`.vivi/charters/<name>.md`). `vivi role status <name>` computes liveness fresh
on each call and reports schedule health (`none`, `never`, `ok`, `due`,
`overdue`) from the cadence; a role bound to another host reports `remote`
instead of being probed locally.

**Goals.** `vivi goal add --path <file>` registers the path of a goal document.
Vivi stores the pointer, not the file; `board` and `boot` list registered goals.

**Work graphs.** Every task, need and want send mints an open node in the
mailspace's `backlog` graph, and `--depends-on` becomes a prerequisite edge.
Lifecycle moves keep node state in step. `vivi graph ready` prints the
frontier (ready, blocked, active, awaiting, reported, gates), and `vivi graph
blocked` lists each blocked node with the prerequisites still unfinished.
`vivi step` turns
the backlog into a manifest of dispatches and exceptions and, with
`--apply <handle>`, completes a settled item. `graph import` and `graph apply`
create or revise further graphs from a Mermaid `flowchart`. Vivi decides which
nodes are eligible; the Mind (or the pump) dispatches. Depth:
[`skills/vivi/references/work-graph-flow.md`](skills/vivi/references/work-graph-flow.md).

**Sync and the server.** With a `[sync]` table, `vivi sync push` sends local
needs, tasks, wants and plain mail (never memos, drafts or trash) to a project
server, and `vivi sync pull` files what other members pushed under the roles
their To and Cc addresses name. Identity is an Ed25519 key in
`.vivi/chat/identity.ed25519`; new members are admitted by a `vouch`. Because
done, reopen, absorb, marks and binds travel as event letters, an item ends in
the same state on every checkout.

**Pools.** `vivi pool` manages preseeded machines ("slots") that each run one
task and push a branch. Providers: `fly` (the default), `docker`, `ssh`, `local`
and `runpod`. `pool dispatch <task>` uses the task body as the brief, claims a
free slot, starts the configured agent harness and returns at once; `pool wait`
and `pool reconcile` collect results. A role's band (`P0` to `P5`, from its
`model` field or a `band:` line in the task body) selects the model from the
pool's `bands` table.

**Pump.** `vivi pump` is an unattended Mind written as a state machine; see
[The pump](#the-pump).

## Command overview

`vivi --help` is the live list and `vivi <command> --help` the flag reference.
The `vivi` skill (see [Agent skills](#agent-skills)) documents usage patterns.

| Command | What it does |
| --- | --- |
| `board` | Actionable tasks, needs and wants per role. `--for`, `--since`, `--process`, `--graph`, `--json` |
| `boot` | The whole project frame in one read-only read: seats, loops, unabsorbed mail, handles with verdicts, goals, and the backlog sliced for dispatch |
| `mailspace` | `init`, `status`, `description`, `archive` and `archive export`, `watch`, `import`, `identity add\|list\|rename` |
| `role` | `list`, `show`, `add`, `set`, `rename`, `status`, `charter show\|set` |
| `mail` | `send`, `reply`, `deliver` (an `.eml` file), `list`, `show`, `thread`, `watch`, `absorb`, `dump` |
| `task` | `send`, `from` (derive from a handle), `list`, `show`, `done`, `reopen`, `watch`, `absorb`, `dump` |
| `need` | `send`, `list`, `show`, `done`, `reopen`, `bind`, `mark`, `unmark`, `watch`, `absorb`, `dump` |
| `want` | `send`, `list`, `show`, `set-priority`, `promote`, `done`, `drop`, `mark`, `unmark`, `watch`, `absorb`, `dump` |
| `memo` | `save`, `list`, `search`, `show`, `delete`, `absorb`, `dump` |
| `rule` | `save`, `list`, `show`, `search`, `supersede`, `retire`, `absorb` |
| `goal` | `add`, `list`, `show`, `drop` |
| `graph` | `import`, `apply`, `show`, `export`, `ready`, `audit [--repair]`, `connect`, `complete`, `activate`, `node add`, `edge add` |
| `step` | Adjudicate the backlog graph into a dispatch/exception manifest; `--apply <handle>` completes a settled item |
| `trace` | The cross-role communication tree around a handle (not work-graph topology) |
| `cycle` | `cycle intake --for <role>`: cycle intake for a role, with an optional cursor file |
| `sync` | Needs a server: `push`, `pull`, `resync`, `fetch`, `join`, `create`, `directory`, `vouch`, `api-key mint\|list\|revoke`. Local: `status`, `key`, `prune`, `keep-local` |
| `mcp` | Needs a server: a stdio bridge to the project server's MCP endpoint |
| `pool` | `create`, `status`, `destroy`, `scale`, `dispatch`, `wait`, `reconcile`, `doctor` |
| `pump` | `run`, `once`, `status` |
| `skills` | `list`, `show <name> [--file <path>]`, `install <dir> [--force]` for the skills embedded in the binary; needs no mailspace |

Commands that list, show or dump take `--json`. A large dump to stdout needs
`--confirm-large` (or `--output <file>`). The `watch` commands (`mail`, `task`,
`need`, `want` and `mailspace watch`) block until a matching local event
arrives; they take `--timeout`, `--once` and a caller-owned `--cursor-file`.

An address with a different eight-hex prefix (`mind@9f8e7d6c.local`) names a
role in another mailspace. The letter is stored as sent, locally, and
`vivi sync` carries it; without `[sync]` Vivi prints a note that it will not
leave this checkout.

## Configuration and files

Vivi's own configuration is one file per project, under `<project>/.vivi/`.

| Path | Contents |
| --- | --- |
| `mailspace.toml` | The config file. Writes are atomic and held under `mailspace.toml.lock` |
| `mailspace.id` | A copy of the mailspace id, used to restore it if the config loses it |
| `mail.sqlite` | The store: items, events, goals, links, work graphs |
| `blobs/` | Content-addressed message payloads (`.eml`) |
| `charters/<role>.md` | Role charters |
| `chat/identity.ed25519` | The 32-byte sync identity seed, created on demand (for example by `vivi sync key`) |
| `sync/` | Sync cursor, history floor and lock (`state.json`, `sync.lock`) |
| `pump/` | The pump home (default; see below) |
| `judgment-corpus.jsonl` | Shadow-screen answers, appended by `vivi step --apply` when `[judgment]` is set |

Deleting `.vivi/` deletes that project's coordination state, local mail and the
replica of synced letters. Do not copy `mailspace.toml` between machines: two
mailspaces with one id collide at `vivi sync join`.

### `mailspace.toml`

| Key | Meaning |
| --- | --- |
| `id`, `name`, `description` | Identity of the mailspace. `id` is minted for you |
| `archive` | Path of a Git repository that receives absorbed records (`vivi mailspace archive --set <path>`) |
| `[[identities]]` | The role roster: `name`, `kind`, `status`, `labels`, `harness`, `provider`, `model`, `thinking`, `pid`, `host`, `cadence`, `aliases`. `vivi role add` writes it |
| `[[probes]]` | Boot probes: `name` and `command`. `command` is an executable path (relative to the mailspace root), run directly with no arguments and no shell, that prints one JSON object with `facts`, `sections` and `verdicts` on stdout. A missing, failing or non-JSON probe is reported as skipped and never fails `boot` |
| `[absorb]` | `quiet_hours` (default 24, `0` turns the wait off): how long an item's newest event must stand before a role that may close it can absorb it |
| `[judgment]` | Optional shadow screens on `vivi step --apply`: `provider` (only `typesafe`), `endpoint`, `model`, `timeout_ms` (default 4000) and `key_cmd` (a shell command whose stdout is the API key; required when `provider` is set). Provider answers never gate a completion |
| `[sync]` | `server` (base URL), `project` (the server-side project id), `name` (this checkout's member name), `send` (`all`, the default, or `addressed`) |
| `[pump]`, `[pump.*]` | The pump; see below |
| `[pools.<name>]` | One table per remote lane pool; see below |

### Pools

A pool table is complete on its own: nothing is inherited. Keys: `provider`
(`fly` by default, `docker`, `ssh`, `local`, `runpod`), `repo` (`owner/name`),
`slots`, `image`, `setup`, `seed`, `requires`, `facts`, `build_dir`, `depth`,
`restore`, and the sub-tables `bands` (`P0` to `P5`, each a list of
`{ model, reasoning }`), `github` (`app_id`, `private_key`), `harness` (`name`,
`run`, `models`, `env`, `rules`), `committer` (`name`, `email`), `bridge`
(`enabled`) and one table per provider (`fly`, `docker`, `ssh`, `local`,
`runpod`). A `local` pool with `[pools.<name>.local] worktree_of = "<repo>"`
makes each slot a Git worktree of a repository already on the machine, with no
network. Credentials are not kept in the config: the Fly token comes from
`FLY_API_TOKEN` or `FLY_ACCESS_TOKEN` (or `fly auth token`), and a harness
receives only the environment variables its `env` list names.

`vivi pool doctor` checks pools, providers and repositories and names the fix
for each finding. `doctor --test` creates throwaway slots (billable on Fly; on
RunPod it rents a GPU pod and also needs `--allow-spend`).

A pool that needs no network, and that `vivi pool doctor` accepts:

```toml
[pools.local]
provider = "local"
slots = 1

[pools.local.local]
worktree_of = "/path/to/a/git/repo"

[pools.local.harness]
name = "echo"
run = "echo {model} {brief}"
models = ["m1"]

[pools.local.committer]
name = "Vivi"
email = "vivi@example.invalid"
```

The full key reference, the other providers, requirements and facts, read-only
dispatch and the MCP bridge are in the `vivi` skill (`vivi skills show vivi`).

## The pump

`vivi pump` works through a backlog without an operator in the loop. It is
project-neutral: the project supplies its commands and names in the `[pump]`
tables of `mailspace.toml`, and the engine holds no project rules.

Each tick the pump:

1. **Plans.** Finds open needs addressed to its `reply_from` role (or held for
   it): those from the senders in `[pump.board] from`, or any such need
   carrying the approval mark, `pump` by default. A need addressed to another
   recipient is not its work, and a reply on an existing thread is never
   planned. It writes the need thread and a prompt under its home, and starts a
   planner (`[pump.planner] run`, prompt on stdin, at most `planners_max` at a
   time). The planner writes `plans/<need>.json`: a verdict and a list of units,
   each with `id`, `brief` and `write_scope`, and optionally `title`, `after`
   (units it waits for), `proof` (commands) and `tools` (the unit's kind).
2. **Dispatches.** Sends each ready unit as a task and hands it to a pool with
   `vivi pool dispatch` to the pool of the unit's kind (`[pump.dispatch.kinds]`;
   `default_kind` when `tools` is empty), on a model from the configured
   `lanes`, to a branch `unit/<id>-a<attempt>`. Units whose write scopes overlap
   do not run at the same time, nor do units touching a path listed in the
   reserved file (paths another session holds). A unit naming an unknown kind
   is held and reported. A failed attempt is retried
   up to `max_attempts`, then the unit is marked stuck and written to
   `exceptions.txt`.
3. **Merges.** Merges returned unit branches into an integration branch
   (`branch`, on `remote`) in batches of at most `merge.batch`, runs the format
   command and shared checks and then each unit's own proof. A failing unit
   proof isolates that unit; a failing shared check on a batch of several units
   sets the whole batch aside on `pump/quarantine/<id>` (the units become
   `quarantined`) and opens one repair unit, ranked first, that starts from
   that branch.
4. **Gates.** After `every_merges` merges, or after `every_seconds` with new
   merges, a read-only gate worker (`[pump.gate] command`, on one of
   `gate.pools`) measures the tree and reports failing test names between
   `FAILING-BEGIN` and `FAILING-END` markers. With `results_dir` set, an
   external runner on any machine can supply the result instead by writing
   `<tip>.json` there; the pool gate is the fallback after
   `results_wait_seconds`. Names in `ignore_file` never count. The first
   readable gate becomes the baseline; failures that are new against it are
   owned by a gate-fix planner, and merging never stops for a red gate.
5. **Promotes.** A gate with no new failure and no owned failure still red
   fast-forwards `main` to the gated tip, so `main` never gets redder. It never
   forces, and skips the push when `main` has moved. `promote = false` turns it
   off.

`vivi pump run` loops until a `STOP` file appears in the home, `vivi pump once`
runs one tick, and `vivi pump status` prints `status.txt` (or `no status yet`).
A second pump on the same home refuses to start. Defaults:

| Table | Keys (default) |
| --- | --- |
| `[pump]` | `branch` (required), `committer_name` and `committer_email` (required; every pump commit uses them), `remote_url` (clone source when `repo` is missing), `reserved_file` (`reserved.txt` in home), `remote` (`origin`), `main` (`main`; empty turns off merging main in and promotion), `promote` (`true`), `tick` (20 s), `home` (`<project>/.vivi/pump`), `repo` (`<home>/repo`), `shell_prefix`, `hand_rules` (a file; default is an embedded generic text), `fix_prefix` (`pumpfix`), `from` (`operator`), `to` (`hand`), `reply_from` (`mind`) |
| `[pump.board]` | `from` (list), `since`, `mark` (`pump`), `every` (300 s), `planners_max` (3), `plan_timeout` (1800 s), `reply_to`, `pull` (`true`: run `vivi sync pull` first), `push_since` (`30m`) |
| `[pump.planner]` | `run` (command; `{charter}` becomes `charter`), `charter` (file), `split_timeout` (1500 s) |
| `[pump.dispatch]` | `lanes` (list of `{ model, cap }`, required), `last_resort` (model), `max_attempts` (3), `budget` (600 s), `overdue_grace` (900 s), `per_tick` (6), `from`, `default_kind` |
| `[pump.dispatch.kinds.<name>]` | `pools` (required, non-empty list; a unit goes to the listed pool with the most free slots), `max` (cap on that kind in flight), `skip_proof` (`false`) |
| `[pump.merge]` | `batch` (8), `lanes` (1), `format`, `shared_checks` (list), `proof_timeout` (300 s), `remote_wait` (90 s), `remote_limit` (1800 s) |
| `[pump.gate]` | `command` (required), `every_merges` (8), `every_seconds` (1800), `pools` (list), `model`, `budget` (1800 s), `results_dir`, `results_wait_seconds`, `ignore_file`, `max_replans` (2) |

The config is validated at start and every violation is named: an empty
`branch`, no `lanes`, a zero `tick`, `per_tick`, `batch`, `max_attempts` or
`every_merges`, a missing committer or gate command, no `shared_checks` while
promotion is on, an unreadable `hand_rules` file, an undefined `default_kind`,
or a pool name that is not a `[pools.*]` table. A missing integration clone is
cloned from `remote_url`, or the pump refuses to start. The pump needs
those pools to dispatch to and a Git remote it can reach.

The home holds `state.json` (the pump's whole memory, saved atomically),
`status.txt`, `log.txt`, `exceptions.txt` (what needs a person),
`needs-ready.txt`, `lock`, and the working directories `plans/`, `needs/`,
`board/`, `resplit/`, `gatefix/`, `merge-jobs/`, `briefs/` and `repo/` (the
integration clone). Operators steer it with files in the home: `STOP` (exit
after the tick), `PAUSE` (merge and collect but dispatch nothing), `HOLD-MERGE`
(feed only fix units to the merger until a clean gate), `FORCE-GATE` (start a
gate now), `REMOTE-MERGE` (offer merge batches to a remote merger first),
`skip.txt` (need handles never to plan) and the reserved file (paths whose
units wait).

The design record is
[`skills/vivi/references/pump-contract.md`](skills/vivi/references/pump-contract.md) and
[`skills/vivi/references/pump-protocols.md`](skills/vivi/references/pump-protocols.md).
It ships inside the binary: `vivi skills show vivi --file references/pump-contract.md`. As with any design record,
check a detail against the code before relying on it.

## Sync, the server and MCP

```toml
# .vivi/mailspace.toml
[sync]
server  = "https://<your project server>"
project = "<project id>"
name    = "<this checkout's member name>"
```

`vivi sync create` creates a project on the server with this mailspace's key and
prints its id (it writes nothing locally). `vivi sync key` prints the public key
for an admitted member to pass to `vivi sync vouch <name> --key-file <pem>`;
`vivi sync join` registers your name; `vivi sync directory` lists members.
`vivi sync status` shows the cursor and the unsent count and needs no server.
A pull can start anywhere with `--since <age|date>`; `resync` replays
everything; `fetch <handle>` and `pull --refs` bring in older letters;
`prune --older-than <age>` evicts old replicated letters locally (never open,
starred, absorbed or local-origin mail); `keep-local <handle>` stops pushing a
stuck letter. The server accepts letters up to 64 KiB.

`vivi mcp` is a stdio bridge: it forwards each JSON-RPC message from stdin
unchanged to the project server's MCP endpoint, so it does not know the tool
list. It needs a server URL, a server-side project id and a credential, and it
creates no mailspace. A flag beats its variable, and a variable beats `[sync]`:

| Setting | Flag | Variable |
| --- | --- | --- |
| Server base URL | `--server` | `VIVI_MCP_SERVER` |
| Project id on the server | `--project-id` | `VIVI_MCP_PROJECT_ID` |
| Member name | `--name` | `VIVI_MCP_NAME` |
| 32-byte member seed file | `--key-file` | `VIVI_MCP_KEY_FILE` |
| Project API key | none | `VIVI_MCP_API_KEY` |

Use a member (`--name` with `--key-file`) or an API key, not both. Mint a key
with `vivi sync api-key mint --label <text> --out <file>`; `--out` writes it to a
new mode-0600 file instead of printing it. `api-key list` and
`api-key revoke <key-id>` manage keys.

## Agent skills

Three skills ship in [`skills/`](skills/) and are embedded in the binary, so
installed skill text always matches `vivi --version`:

| Skill | Load it for |
| --- | --- |
| [`vivi`](skills/vivi/SKILL.md) | The CLI: mailspaces, roles, goals, work graphs, pools, sync. Load this one always |
| [`tugboat`](skills/tugboat/SKILL.md) | A deliberate multi-agent protocol on a Vivi backlog: a Mind routes, Hands implement, Auditors and Heads review |
| [`speedboat`](skills/speedboat/SKILL.md) | A fast delivery protocol on the same backlog: small parallel Hands, minimum verification |

`tugboat` and `speedboat` are alternatives; each depends only on `vivi`.

```sh
vivi skills list
vivi skills show vivi
vivi skills show vivi --file references/work-graph-flow.md
vivi skills install ~/.agents/skills     # writes vivi/, tugboat/, speedboat/
```

## Documentation

- [`skills/vivi/SKILL.md`](skills/vivi/SKILL.md): the usage reference (also `vivi skills show vivi`)
- [`skills/vivi/references/pump-contract.md`](skills/vivi/references/pump-contract.md) and [`pump-protocols.md`](skills/vivi/references/pump-protocols.md): the pump contract and wire protocols
- [`AGENTS.md`](AGENTS.md): guide for contributors and coding agents
- [`docs/README.md`](docs/README.md): index of the historical planning and release record. Those files are kept as written and are not maintained

## Security

- Vivi keeps no credentials in `mailspace.toml`. Fly and RunPod tokens come from
  the environment or their own tools; the judgment provider key comes from
  `key_cmd` (run with `sh -c` only when `vivi step --apply` consults the
  provider); a project API key is read from `VIVI_MCP_API_KEY` or written to a
  file you name.
- `.vivi/` is machine-local and ignores itself in Git. It holds your sync key
  seed and your whole item history, so do not force-add it to a repository or
  copy it to a machine you do not trust.
- `vivi pool dispatch` runs an agent with the account and environment the pool
  gives it. A `local` pool has no isolation: the worker runs as you.

## License

MIT. See [LICENSE](LICENSE).
