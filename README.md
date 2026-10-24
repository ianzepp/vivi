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

The local pool provider can reset its slot to the dispatch base before launch.
Remote jobs fetch the job's `FROM` with their job-scoped credential, then check
out and clean that revision before the harness starts; remote dispatch does
not pre-reset the machine.

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
[GitHub releases](https://github.com/ianzepp/vivi/releases). `install.sh`
verifies the archive SHA-256, writes a versioned binary, and points a
relative `vivi` symlink at it:

```text
~/.local/bin/vivi-20.0.0
~/.local/bin/vivi -> vivi-20.0.0
```

The prefix is `~/.local`, so binaries live in `~/.local/bin` (usually
already on `PATH`). That is not `~/.vivi`, which is a mailspace
(`mailspace.toml`, `mail.sqlite`) and must never hold binaries.

```sh
curl -fsSL https://raw.githubusercontent.com/ianzepp/vivi/main/install.sh | sh
# or pin a release / prefix:
curl -fsSL https://raw.githubusercontent.com/ianzepp/vivi/main/install.sh | sh -s -- --version 20.0.0 --prefix "$HOME/.local"
```

The script defaults to the release it shipped with (`VIVI_VERSION`, currently
20.0.0) and `~/.local` (`VIVI_PREFIX`). `VIVI_BASE_URL` or `--base-url`
can point at a local mirror. Optional `GITHUB_TOKEN` / `GH_TOKEN` raises
the GitHub rate limit. Archives are built for `aarch64-apple-darwin`,
`x86_64-apple-darwin`, and `x86_64-unknown-linux-gnu`.

```sh
vivi update --check            # compare this binary to the newest public release
vivi update                    # install that release and flip the symlink
vivi update --version 14.1.1   # pin / roll back (when that release has a .sha256)
vivi versions                  # list versioned installs; * is active
vivi uninstall --version 14.1.1
```

`~/.local/bin` is shared with other tools. `vivi uninstall` only deletes
`vivi-<semver>` files and the `vivi` symlink; it never removes or renames
anything else. A plain-file `vivi` already in that directory (not a
symlink) is refused: pass `--adopt` / `--force` to move it aside to
`vivi.unmanaged` (or `vivi.unmanaged.N`). A `cargo install` binary at
`~/.cargo/bin/vivi` is the same kind of unmanaged file; `vivi update`
refuses it unless you pass `--prefix` or `--adopt` (adopt installs under
`~/.local/bin`, never into `~/.cargo/bin`). Both refuse a path inside a
mailspace (`mailspace.toml`).

From source (`rust-toolchain.toml` pins Rust 1.93):

```sh
git clone https://github.com/ianzepp/vivi.git
cd vivi
cargo build --release      # target/release/vivi
cargo install --path .     # unmanaged; `vivi update` will refuse this file
vivi --version
```

## Quick start

Everything here runs offline, in any directory.

```sh
cd /path/to/your/project
vivi mailspace init                           # creates .vivi/
vivi role add mind --kind mind                # the seat that routes and accepts work
vivi role add hand-1 --kind hand --harness subagent

# A need (the requirement) and a task under it. A need must say when it is
# done: --done-when builds the letter's front matter; a need without one is
# refused. The task carries the same check for `vivi step`.
vivi need send --from mind --to hand-1 --subject "Add a greeting" \
  --body "Print hello." \
  --done-when "running it prints hello"
vivi task send --from mind --to hand-1 --subject "Write hello.sh" \
  --body "Print hello." \
  --done-when "running it prints hello"
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
| `need` | A prioritised requirement, usually the parent of tasks | send, bind, assign, withdraw, done, reopen, mark |
| `want` | A question or goal under discussion; the needs that serve it decide its state | send, set-priority, done/drop, mark, depend |
| `memo` | Durable per-role memory | save, list, search, show, delete |
| `rule` | A project rule every seat can read | save, list, supersede, retire |

Tasks, needs and wants accept `--depends-on <handle>` (repeatable), and
`depend <handle> --add <h>... --remove <h>... --for <role>` changes them after
the send; the change travels to every member. `absorb`
seals a record so it can no longer change.

**Letters.** A need, want or task body is a *letter*: TOML front matter between
two `+++` lines, then Markdown prose. A need requires `done_when` (a non-empty
list), a want requires `question`, and a task may carry `done_when`,
`depends_on`, `target`, `write_scope` and more; an unknown key is refused. The
send flags (`--done-when`, `--question`, `--target`, `--write-scope`,
`--depends-on`, `--serves`, `--explores`) build the front matter for you. A body
whose first line is `+++` is taken as the whole letter, and a flag beside it is
refused. A need with no `done_when` is refused at send and nothing is stored.
Mail and replies are gated too: every send carries `X-Vivi-Format: 1`, a
plain-prose message is stored exactly as written, and a body that opens with
`+++` must be a pump note, a pump action or a role reply envelope, or the send
is refused. The [envelope reference](skills/vivi/references/envelope.md) has the
parser rules.

**Wants.** A want is where a discussion lives; it is never dispatched. Work
goes out as needs: `vivi need send --serves <want> --done-when "..."`, one
need per piece of work, each with its own `done_when`. A research need uses `--explores <want>`
instead; it does not count toward the want's state, and the two flags are
exclusive. The state is computed from the serving needs: open while any is
open, and done once the want has at least one and all are closed (a withdrawn
need counts as closed). A new serving need reopens it, a want with no serving
needs is open, and a reply does not change it. `want done` by hand is for a
want abandoned before any need. `want show` and `board` list the serving needs.

**Who may finish work.** A worker's `done` is a report. For tasks and ordinary
work, the item's author or sent-copy holder, or a role of kind `mind` or
`operator`, accepts it; acceptance completes the work, unlocks dependents and
is recorded as an event letter that other checkouts apply when they sync. An
active delegate can also close an assigned need; that closes the need for
everyone and lapses the assignment. `vivi step` lists reported work as
`awaiting_acceptance`.

`task done` and `need done` can record `--branch` and `--commit` together on
the close event; the flags must be paired. An authorized reopen or a close with
no location clears an older pair. `need show` displays the pair when known. A
joined need inherits provenance only when every bound unit has both values and
all use the same branch; it records the commit from the latest unit close.
Otherwise each unit keeps its own provenance and the need-level pair is omitted.

An author, the current assigning role, or an operator can assign a need to a
local role or another member mailspace. Each assignment is a reply letter; the
newest assignment wins. Active assignments appear in the delegate's work list
and create a local backlog node for binding tasks. Closing as the delegate
lapses that assignment; if the need is reopened, assign it again before the
former delegate can act.

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

**Pools.** **vivi pool** manages preseeded machines ("slots") that each run
one task and publish a branch. Providers are **fly**, **docker**, **ssh**,
**local** and **runpod**. **pool dispatch <task>** claims a free slot and
starts the configured harness; **pool wait** and **pool reconcile** collect
results. **pool clean** and **pool reset** reclaim eligible local worktrees and
build output. A local dispatch refuses when the
volume stays under 10 GiB free. A role's band (**P0** to **P5**) selects a
model from the pool's bands table.

**Pump.** `vivi pump` is an unattended Mind written as a state machine; see
[The pump](#the-pump).

## Command overview

`vivi --help` is the live list and `vivi <command> --help` the flag reference.
The `vivi` skill (see [Agent skills](#agent-skills)) documents usage patterns.

| Command | What it does |
| --- | --- |
| `board` | Actionable tasks, needs and wants per role. `--for`, `--since`, `--process`, `--graph`, `--json` |
| `doctor` | Read-only health verdict and ranked findings for the install and selected project. `--project`, `--json`, `--fix-hints` |
| `boot` | The whole project frame in one read-only read: seats, loops, unabsorbed mail, handles with verdicts, goals, and the backlog sliced for dispatch. `--json` |
| `mailspace` | `init`, `status`, `description`, `archive` and `archive export`, `watch`, `import`, `identity add\|list\|rename` |
| `role` | `list`, `show`, `add`, `set`, `rename`, `pool`, `status`, `charter show\|set` |
| `mail` | `send`, `reply`, `deliver` (an `.eml` file), `list`, `show`, `thread`, `watch`, `absorb`, `dump` |
| `task` | `send`, `from` (derive from a handle), `list`, `show`, `done`, `reopen`, `depend`, `watch`, `absorb`, `dump` |
| `need` | `send`, `list`, `show`, `done`, `reopen`, `assign`, `withdraw`, `bind`, `mark`, `unmark`, `depend`, `watch`, `absorb`, `dump` |
| `want` | `send`, `list`, `show`, `set-priority`, `done`, `drop`, `mark`, `unmark`, `depend`, `watch`, `absorb`, `dump` |
| `memo` | `save`, `list`, `search`, `show`, `delete`, `absorb`, `dump` |
| `rule` | `save`, `list`, `show`, `search`, `supersede`, `retire`, `absorb` |
| `goal` | `add`, `list`, `show`, `drop` |
| `graph` | `import`, `apply`, `show`, `export`, `ready`, `audit [--repair]`, `connect`, `complete`, `activate`, `node add`, `edge add` |
| `step` | Adjudicate the backlog graph into a dispatch/exception manifest; `--apply <handle>` completes a settled item |
| `trace` | The cross-role communication tree around a handle (not work-graph topology) |
| `cycle` | `cycle intake --for <role>`: cycle intake for a role, with an optional cursor file |
| `sync` | Needs a server: `push`, `pull`, `resync`, `fetch`, `join`, `create`, `directory`, `vouch`, `api-key mint\|list\|revoke`. Local: `status`, `key`, `prune`, `keep-local` |
| `mcp` | Needs a server: a stdio bridge to the server-level MCP endpoint (`<server>/v1/mcp`); `tools` lists its tools and `call <tool> [json]` runs one |
| `pool` | `list`, `show`, `add`, `set`, `unset`, `remove`, `bands`, `create`, `status`, `destroy`, `scale`, `dispatch`, `wait`, `reconcile`, `clean`, `reset`, `doctor` |
| `config` | `show [--resolved]`, `validate`, `migrate [--write]`, `set <key> <value>`, `unset <key>` |
| `pump` | `run`, `once`, `start`, `pause`, `resume`, `stop`, `log` (`-n`, `-f`, `--unit`, `--grep`, `--exceptions`), `priority`, `target list\|add\|remove`, `rungs show\|set`, `service`, `send --need <need>`, the actions on a need's open pump record (`requeue <step-task>...` or `requeue --job <job>`, `withdraw <need>...`, `replan <need>`, `answer <step-task> <word>`, `unlock <need>`) and the pump record verbs `list`, `show`, `done`, `reopen`, `history` |
| `flow` | `list`; `show <kind> [--need <need>]`: the shipped flows, and a need's runs through one (read-only) |
| `delivery` | `send` a structured branch handoff to a merge role; `close` after merge |
| `monitor` | Read-only live pump and pool-slot view; `--project` adds roots, `--once [--json]` prints a snapshot |
| `report` | Read-only attempt and need summaries: `records`, `throughput`, `durations`, `needs`, `failures` |
| `proxy` | Local loopback front door for model providers: `run`, `install`, `uninstall`, `status`, `log`, `providers`, `check` |
| `skills` | `list`, `show <name> [--file <path>]`, `install <dir> [--force]` for the skills embedded in the binary; needs no mailspace |

Commands that list, show or dump take `--json`. A large dump to stdout needs
`--confirm-large` (or `--output <file>`). The `watch` commands (`mail`, `task`,
`need`, `want` and `mailspace watch`) block until a matching local event
arrives; they take `--timeout`, `--once` and a caller-owned `--cursor-file`.

`vivi doctor` prints one `ok`, `warn` or `fail` verdict and its findings; its
process exit code is 0, 1 or 2 respectively. Text output can include suggested
commands with `--fix-hints`; JSON returns the same verdict and structured
findings. The checks are read-only. If a WAL-mode mailspace has no
`mail.sqlite-shm`, doctor skips the store, inbox and `done_when` checks instead
of opening SQLite in a way that could create the sidecar. Proxy credential
checks inspect file presence and permissions without reading credential
contents or running `key_cmd`.

An address with a different eight-hex prefix (`mind@9f8e7d6c.local`) names a
role in another mailspace. The letter is stored as sent, locally, and
`vivi sync` carries it; without `[sync]` Vivi prints a note that it will not
leave this checkout.

## Monitoring and reports

`vivi monitor` is a read-only live view of registered pump projects and pool
slots. Add one or more `--project <root>` values for extra projects; use
`--once` for a table snapshot or `--once --json` for JSON. `vivi report` reads
attempt records and need lifecycle events in the selected project mailspace,
without starting work:

```sh
vivi report records --format csv|jsonl
vivi report throughput [--since <age|date|RFC3339>] [--until <age|date|RFC3339>] [--by <dimension>]
vivi report durations [filters] [--format table|json|csv|md]
vivi report needs [filters] [--format table|json|csv|md]
vivi report failures [filters] [--format table|json|csv|md]
```

The fixed report views preserve missing historical values as unavailable rather
than treating them as zero. The [Vivi skill](skills/vivi/references/monitoring-and-reports.md)
documents the row fields, filters, time windows and measures.

## Configuration and files

Vivi's own configuration is one file per project, under `<project>/.vivi/`.

| Path | Contents |
| --- | --- |
| `mailspace.toml` | The config file. Writes are atomic and held under `mailspace.toml.lock`. Config commands (`config set`/`unset`, `pump target`, `pump rungs`, `pool` and `role` edits) are refused while the pump runs (`vivi pump pause` first), keep a `mailspace.toml.bak.<stamp>` backup (10 kept) and roll back a write that adds a new validate error |
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
| `[[identities]]` | The role roster: `name`, `kind`, `status`, `labels`, `harness`, `provider`, `model`, `models`, `thinking`, `pid`, `host`, `cadence`, `aliases`, `pool`, `read_only`. `vivi role add` writes it |
| `[memo]` | `hidden_tags` and per-tag `retain` durations; the built-in `digest` tag is hidden by default |
| `[[probes]]` | Boot probes: `name` and `command`. `command` is an executable path (relative to the mailspace root), run directly with no arguments and no shell, that prints one JSON object with `facts`, `sections` and `verdicts` on stdout. A missing, failing or non-JSON probe is reported as skipped and never fails `boot` |
| `[absorb]` | `quiet_hours` (default 24, `0` turns the wait off): how long an item's newest event must stand before a role that may close it can absorb it |
| `[judgment]` | Optional shadow screens on `vivi step --apply`: `provider` (only `typesafe`), `endpoint`, `model`, `timeout_ms` (default 4000) and `key_cmd` (a shell command whose stdout is the API key; required when `provider` is set). Provider answers never gate a completion |
| `[sync]` | `server` (base URL), `project` (the server-side project id), `name` (this checkout's member name), `send` (`all`, the default, or `addressed`) |
| `[pump]`, `[pump.*]` | The pump; see below |
| `[pools.<name>]` | One table per remote lane pool; see below |

Memo retention can be set per tag. On a tagged memo save, Vivi checks only that
identity's memos carrying each newly saved tag and deletes records dated
strictly before the configured cutoff. Missing or invalid rules, and malformed
memo dates, retain the memo. Durations accept bare seconds or `s`, `m`, `h`,
and `d` units.

```toml
[memo.retain]
digest = "90d"
```

### Pools

A pool table is complete on its own: nothing is inherited. Keys: `provider`
(`fly` by default, `docker`, `ssh`, `local`, `runpod`), `repo` (`owner/name`),
`slots`, `fallback` (next pool in the role routing chain), `image`, `setup`, `seed`, `requires`, `facts`, `build_dir`, `depth`,
`restore`, and the sub-tables `bands` (`P0` to `P5`, each a list of
`{ model, reasoning }`), `github` (`app_id`, `private_key`), `harness` (`name`,
`run`, `models`, `env`, `rules`), `committer` (`name`, `email`), `bridge`
(`enabled`), `workspace` (local provider only), and one table per provider (`fly`, `docker`, `ssh`, `local`,
`runpod`). A `local` pool with `[pools.<name>.local] worktree_of = "<repo>"`
makes each slot a Git worktree of a repository already on the machine, with no
network. With a single-target pump, the pump reads a local worktree pool's
unit branches straight from that repository, so no `shell_prefix` push hook is
needed. Credentials are
not kept in the config: the Fly token comes from
`FLY_API_TOKEN` or `FLY_ACCESS_TOKEN` (or `fly auth token`), and a harness
receives only the environment variables its `env` list names.

`vivi pool doctor` checks pools, providers and repositories and names the fix
for each finding. `doctor --test` creates throwaway slots (billable on Fly; on
RunPod it rents a GPU pod and also needs `--allow-spend`).

Pool identity uses the exact mailspace UUID plus the pool name; the name slug
is used only when the mailspace has no id. Shared-root slots use that key in
their machine names or directory paths. Project-relative local roots are
scoped by the project path.

Only a `local` pool can enable a per-job workspace disk, and it requires macOS.
`[pools.<name>.workspace] enabled = true` gives each job a fresh disk for its
temporary checkout and build output. `kind` picks what backs it:

- `ram` (the default): a RAM disk. Before mounting, Vivi checks free memory,
  already-held RAM disks and the configured margin; a job that does not fit
  is refused with `ramdisk-wait`.
- `apfs`: an APFS volume with a quota of `size_gib`, in the APFS container
  that holds the slot (or `container = "diskN"`). It costs no memory for the
  disk itself, so it suits machines whose memory is small next to their
  storage. Vivi checks free memory against the margin (for the compilers) and
  the container's free space, less what other Vivi volumes may still grow
  into, against the quota plus the margin; a job that does not fit is refused
  with `apfs-wait`. Volumes use a quota and never a reserve, so an idle volume
  claims no space.

The default size is 8 GiB, with 6 GiB kept for the host; a pool that runs the
proof rungs may need 32 GiB, and a full Rust workspace test build can need 12. The default
environment turns off Cargo incremental output and debug symbols for those
jobs. A custom `env` table replaces those defaults. A refused manual dispatch
returns the wait message; a durable pump job remains pending if no configured
fallback pool can accept it. There is no SSD fallback on that local slot. The
disk is removed when the job finishes or is stopped. Teardown verifies that
the device matches the job's ownership record; unknown or mismatched records
are preserved with a warning. On pump startup, orphaned APFS volumes and then
RAM disks for that mailspace are swept, while disks owned by live jobs are
left in place. APFS volumes survive a reboot, so the sweep is what removes
them after one; the RAM sweep never ejects a device that is an APFS volume.

```toml
[pools.workers]
provider = "local"
slots = 2

[pools.workers.workspace]
enabled = true
kind = "apfs"     # or "ram"
size_gib = 12
margin_gib = 6
```

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

## Role → pool routing

Roles (`[[identities]]`) and pools (`[pools.<name>]`) are configured
separately. The pump addresses every job to a role. The role's assigned pool is
the first candidate and its `fallback` chain follows. A job stays pending when no
candidate has a free slot.

| Step | Role |
| --- | --- |
| Research | `researcher` |
| Plan, plan revision | `pump-planner` |
| Admit | `admitter` |
| Slice, rework slices | `slicer` |
| Slice task | the hand role named by `[pump].to` |
| Slice merge, sync main into the need branch, merge to main | `merge` |
| Audit, judge | `auditor` |
| Failure escalation | `head-cto` |
| Proof rungs | the pools in `[pump.rungs] pools` |
| Repair attempts | `repairer` |
| Cadence, digest | the role whose schedule is due |

`vivi flow show build` prints the steps in order; see [The pump](#the-pump).

`vivi mailspace init` and `vivi sync join` provision the operator, `merge`,
`pump-planner`, `researcher`, `admitter`, `slicer`, `repairer` and `head-cto`
roles with their shipped charters. They preserve existing role settings and
charter files. The `auditor` role, the hand role, and every pool and model
route are yours to configure.

Concurrency comes from a pool's `slots`. Optional `[pools.<name>.caps]` holds
per-model caps inside that pool. A role may name a model slug, a band (`P0` to
`P5`, optionally `-S0` to `-S3`), or a list of models. The pool's
`harness.models` is the allow-list. A role that names a slug takes its effort
from the role's `thinking`, else from a band entry with that model.

`read_only` is optional on a role. Kinds default read-only for `auditor`,
`triage`, `cadence`, `steward`, `head`, `digest`, `researcher`, `admitter` and
`slicer`; `hand`, `merge` and `lint` write. A `--read-only` flag still forces
read-only.

A pool's `fallback = "<pool>"` points to the next candidate. The target must
exist and cycles fail validation. A full pool and all of its fallback
candidates leave the work pending.

`vivi config migrate` converts old-style pump routing to role pool
assignments. It is a dry run by default; `--write` applies it and keeps a
timestamped `.bak`.

When `[pump]` is configured, `vivi config validate` requires the `pump-planner`,
`researcher`, `admitter`, `slicer`, `repairer`, `merge` (kind `merge`),
`auditor` and `head-cto` roles and a `[pump].to` role of kind `hand`, each with
a pool that can run its jobs; `vivi doctor` reports the same requirements. It
also validates role and pool references, fallback targets and cycles, model
bands, and read-only settings. Run it before starting the pump; it also runs at
`vivi pump run|once` startup.

`vivi mailspace init` writes a starter `[pump]` table that holds only the git
`committer_name` and `committer_email`, both empty. A project that only lacks
things the pump needs is *not ready*, not invalid: a missing committer, roles
and pools, or a pool without a harness or bands, and a missing `task`, `need`
or `main` proof rung (for each target, counting the rungs it inherits from
`[pump.rungs]`; set them with `vivi pump rungs set` or
`vivi pump target add --rung`, after `[pump.rungs] pools` names a pool: a rung
command with no effective pool is refused and rolled back).
`vivi config validate` lists each missing item under `not ready:` and exits 3;
a wrong value (a bad number, a pool name that points at nothing, a misspelled
role, a pump-role charter without the reply envelope) is an error and exits 1;
ready exits 0. `vivi pump run` and `once` on a not-ready project dispatch
nothing and exit cleanly; the log lists the items. A fresh `pump.db` starts
*stopped*: once the project is ready, run `vivi pump start` (the service starts
it when the pump is registered there).

`vivi config show`, `validate` and `migrate` inspect, validate and migrate
routing. `vivi role pool` and `vivi role set` assign role settings.
`vivi pool list`, `show`, `add`, `set`, `unset` and `remove` manage pools;
`vivi pool bands set|unset` sets model bands. `vivi pool clean` and `reset`
manage slot checkouts. Writers preserve comments and only write keys that were
set.

## The pump

The pump runs flows. A flow is a Mermaid chart shipped in the binary: its nodes
are steps, each step is a job for a role, and the answer word of a step picks
the next one. `vivi flow list` names the flows. `vivi flow show <kind>` prints
one, and `vivi flow show <kind> --need <need>` adds that need's runs with each
step's task, state and verdict.

| Flow | Steps |
| --- | --- |
| `build` | Optional research, plan, admit (one revision allowed), slice, then the slice tasks: each hand task merges onto the need branch `vivi/need/<need>` after the `task` rung. The need is then verified: audit and the `need` rung side by side, a judge, and an optional higher rung. One rework pass may follow. Then merge to main. A merge conflict syncs main into the need branch, reruns the `need` rung, and merges again. A failure goes to `head-cto`, which restarts the run at research, plan, slice or rework, or locks it |
| `research` | The researcher posts its analysis on the want |
| `release` | Waits until every dependency need is on main |
| `repair` | Two repairer attempts, the `main` rung on the repair branch, then a merge; otherwise the need is unrepaired, main stays red and the pump raises an alarm. A red main files a repair need |

A need runs when it has an open **pump record**. `vivi pump send --need <need>`
files one, and sending is the dispatch. The flow is the record's `kind`
(`build`, `research`, `release` or `repair`), else the need's mark (`research`,
`release`, `repair`), else `build`. A landing closes the need and closes the
record `landed`. A record also closes `withdrawn` or `superseded`. A send is
refused for a need that already has an open record, a need that is done, or a
handle that is not a need. The front matter of the record is in the
[envelope reference](skills/vivi/references/envelope.md#pump-record).

`vivi pump requeue`, `withdraw`, `replan`, `answer` and `unlock` act on the
need's open pump record: each posts one structured reply on it (front matter
with an `action` key) and the pump applies it at its next tick, exactly once. A
need with no open record is refused.

- `vivi pump requeue <step-task>` runs that step's newest job again;
  `vivi pump requeue --job <job>` releases a job the pool refused three times
  (held with a `job-dispatch-refused` exception).
- `vivi pump withdraw <need> --reason <text>` closes the need and the record
  `withdrawn`.
- `vivi pump replan <need>` plans the need again.
- `vivi pump answer <step-task> <word>` answers a flow step by hand.
- `vivi pump unlock <need> [--entry <entry>]` restarts a locked run at an
  entry (default: the flow's first). A locked run keeps its record open and
  raises `flow-need-locked`; a reply on the need never releases the lock.

The pump records each job before choosing a slot. Jobs without capacity stay
pending, and dispatched work remains owned by its pool across pump restarts: a
restart reattaches jobs by the `[pump-job:<id>]` subject marker or the `[job]`
table of the brief, reads pool receipts, and applies results through the
ordinary tick path. `vivi pump stop` finishes the current tick and exits; it
does not cancel or drain pool jobs. A paused pump collects completed work but
starts no new work. Valid routing, pool, budget and check changes are loaded
between ticks, and a target added to the config applies at the next tick. A
change the running pump cannot apply (the pump home, or the integration path of
a running target) keeps the old value and raises one `config-reload-refused`
exception; restart the pump to take it.

Only the pool runner publishes a branch: after a clean agent exit it pushes
the candidate commit, fetches the ref back and verifies it, and the agent never
pushes. A role may publish only the refs its kind owns. The pump marks work
merged only after it reads the ref itself. [The pump
contract](skills/vivi/references/pump-contract.md) has control, routing and
restart behavior, and [the job protocols](skills/vivi/references/pump-protocols.md)
have job identity, briefs, recovery and publication.

Role cadence uses each role's latest durable outbound signal. Due runs appear
on the board. To look into an unclear want, head-ceo files a research need:
`vivi need send --mark research --explores <want-handle> --done-when "..."`,
then starts it with `vivi pump send --need <need>` (mark first). The researcher
role posts its analysis on the want. The cadence prompt tells head-ceo to file
at most two research needs for one want, then to send serving needs
(`vivi need send --serves <want-handle> --done-when "..."`) or drop it.

The pump home defaults to `<project>/.vivi/pump`. It stores `pump.db`,
`log.txt`, `exceptions.txt`, job briefs, the integration clone and bounded tick
receipts for `vivi monitor`. Use `vivi pump pause`, `resume`, `stop` and
`start` to control it; there are no marker files. To see where work is, use
`vivi flow show <kind> --need <need>`, `vivi pump list`, `vivi pump show
<record>`, `vivi pump log` and `vivi step`.

Use `vivi monitor` for persisted task, pool, pump, and tick snapshots.
Selecting a monitor row tails its raw log; log contents are not redacted. Use
`vivi report` for records and grouped throughput, duration, need, and failure
views. Missing historical timing fields remain unavailable rather than zero.
Filters, formats, and measures are in
[monitoring and reports](skills/vivi/references/monitoring-and-reports.md).

## Delivery

```sh
vivi delivery send --from <mind> --to <merge-role> --branch <branch> --tip <sha> --base <sha> --gate "<command>" --gate-result "<result>"
vivi delivery close <handle> --for <merge-role> --merge-commit <sha>
```

`vivi delivery send` pushes the Mind's local integration branch to
`refs/heads/mind/<branch>` and files a structured need assigned to the merge
role. It records the local and remote branch names, base and tip, push status,
gate command and result, completed needs, blocked needs with reasons, and newly
filed work. The command records the supplied gate result; it does not run the
gate. If the push fails, the need is still filed with `push.status =
not_pushed` and a safe failure detail.

After the merge role lands the branch on `main`, close the delivery need with
`vivi delivery close <handle> --for <merge-role> --merge-commit <sha>`. The
close event records the main commit as provenance, which appears in `need
show` and travels with the need.

## The proxy

`vivi proxy` is a user-level loopback front door. One listener (default
`127.0.0.1:18180`) path-routes `/<provider>/v1/{responses,chat/completions,messages,models}`
— and the same paths without `/v1` — to native adapters, supervised child
processes, or an already-running upstream. Config lives next to the pump
service registry, at the platform config dir under `vivi/proxy.toml`
(`--config` or `VIVI_PROXY_CONFIG` overrides it). The file holds paths and
`key_cmd` / `auth_token_cmd` only; never inline secrets.

```sh
vivi proxy run                 # foreground listener + child supervisor
vivi proxy install             # macOS LaunchAgent, or Linux systemd --user
vivi proxy uninstall
vivi proxy status
vivi proxy log [-f] [--provider <name>]
vivi proxy providers           # Pi base URLs, health, credential expiry
vivi proxy check [provider]    # tiny live GET to /<p>/health
```

`install` writes `com.vivi.proxy` (launchd) or `vivi-proxy.service` (systemd
`--user`) so the pump job and the proxy job are independent. The unit
`ExecStart` prefers the stable `vivi` symlink next to a versioned
`vivi-<semver>` binary. A second `vivi proxy run` refuses the instance lock.
The systemd unit uses `Restart=on-failure` with `StartLimitBurst=5` in 60s;
the launchd plist keeps `KeepAlive` and sets `ThrottleInterval` 10.

Backends:

| `kind` | Meaning |
| --- | --- |
| `native` | In-process adapter (`chatgpt-codex`, `kimi-coding` / `openai-passthrough`, or `xai-grok`) |
| `child` | Vivi spawns the command, health-checks it, restarts with backoff |
| `upstream` | Reverse-proxy to a server that is already listening |

Native Codex reads `~/.codex/auth.json` (then `~/.pi/agent/auth.json`) in
place, refreshes 30s before JWT expiry under an advisory lock, writes back
atomically at mode `0600`, filters SSE to an allowlist and rebuilds an empty
`response.completed.output`, and keeps the existing non-streaming, tool-less,
usage-0 chat shim. Native Kimi re-reads its credential file on every request,
injects `X-Msh-*` identity headers, passes bytes through, and retries once
after a 401 refresh. `openai-passthrough` shares the request path only: it
sends just the bearer (a `key_cmd` or its own configured credential file),
never refreshes, is never marked dead, and does not look at Kimi's
home-directory credentials, so a dead Kimi login cannot refuse another
route. `/<provider>/health` for it reports `provider` as
`openai-passthrough`. Some upstreams refuse a request that lacks a session
id (OpenCode Go: HTTP 400 `MissingSessionID`). Set `session_header =
"x-opencode-session"` on that provider: a client-sent header of that name
is forwarded unchanged, otherwise the proxy adds `ses_<32 hex>` to non-GET
requests, a hash of the model plus the first non-system message (stable for
one conversation, never derived from a credential), or a random id when the
body has none. Native Grok (`xai-grok`) reads the Grok CLI login file
(`~/.grok/auth.json`, entries keyed `<issuer>::<oidc_client_id>`), picks the
entry for the origin of `token_url` (`client_id` disambiguates several), and
sends its `key` as the bearer to `https://api.x.ai/v1`. It refreshes 30s
before expiry (`expires_at`, else the JWT `exp`) and once after a 401, with a
public-client `refresh_token` form post to `https://auth.x.ai/oauth2/token`.
Under the advisory lock it re-reads the file first (a newer token already on
disk wins), then writes back `key`, a rotated `refresh_token`, and
`expires_at` into that entry only, atomically at mode `0600`. Cursor stays a `child` (Bun/protobuf engine). Local
llama-server is an `upstream`.

The listener refuses a non-loopback bind unless `allow_remote = true` and
`auth_token_cmd` are both set. Inbound requests need `Host` of localhost /
the bind address, no `Origin`, and `Content-Type: application/json` on POST.
Credential and token-endpoint failures become a fixed message plus a request
id; token-endpoint bodies are never echoed. A token-endpoint 400/401 (or a
missing refresh token when a JWT must be refreshed) marks that credential
file dead in memory. Later requests to that provider return 502
`credential_error` immediately — no upstream and no token-endpoint call —
until the file's identity (path + mtime + size) changes or the proxy
restarts. Transient token-endpoint failures (network, 429, 5xx) fail only
that request. `/health` and `/<provider>/health` report `status` of
`credential_dead` with HTTP 503; `vivi proxy check` and `vivi proxy
providers` show the same label. Access log is rotated JSONL under
`log_dir` (bodies are never written). There is no background token refresh.

A Mac starting point, written to
`~/Library/Application Support/vivi/proxy.toml` (Linux:
`~/.config/vivi/proxy.toml`):

```toml
listen = "127.0.0.1:18180"

[providers.codex]
kind = "native"
adapter = "chatgpt-codex"
credentials = "~/.codex/auth.json"
fallback_credentials = "~/.pi/agent/auth.json"
# Optional. When omitted, /codex/v1/models serves gpt-6-luna and gpt-6.1-sol.
# models = ["gpt-6-luna", "gpt-6.1-sol"]

[providers.kimi]
kind = "native"
adapter = "kimi-coding"
credentials = "~/.kimi-code/credentials/kimi-code.json"

[providers.grok]
kind = "native"
adapter = "xai-grok"
credentials = "~/.grok/auth.json"
# Optional: upstream = "https://api.x.ai/v1"
# Optional: token_url = "https://auth.x.ai/oauth2/token"
# Optional: client_id = "<oidc client id>"   # picks one of several entries
# Optional: models = ["grok-code-fast-1"]    # else /grok/v1/models passes through
# Alternative to credentials (static bearer, no refresh):
# key_cmd = "..."

# OpenCode Go: the existing passthrough adapter with a static key.
[providers.opencode-go]
kind = "native"
adapter = "openai-passthrough"
upstream = "https://opencode.ai/zen/go/v1"
key_cmd = "python3 -c 'import json,os;print(json.load(open(os.path.expanduser(\"~/.local/share/opencode/auth.json\")))[\"opencode-go\"][\"key\"])'"
session_header = "x-opencode-session"   # OpenCode Go refuses chat requests without it

[providers.llama]
kind = "upstream"
backend = "http://127.0.0.1:18173"

[providers.cursor]
kind = "child"
command = ["bun", "run", "/path/to/cursor-proxy"]
backend = "http://127.0.0.1:18183"
```

Adjust the Cursor `command` to the Bun entrypoint you already run. Credential
paths may be omitted when the adapter defaults are enough (`~/.codex/auth.json`
then `~/.pi/agent/auth.json` for Codex; `~/.kimi-code/credentials/kimi-code.json`
then `~/.kimi/credentials/kimi-code.json` for Kimi; `~/.grok/auth.json` for
Grok). `openai-passthrough` forwards any path under the route to `upstream`
(`/messages`, `/responses`, `/chat/completions`, anything else as given), but
it shares the Kimi code path and still adds the `KimiCLI` user agent and
`X-Msh-*` identity headers to every request.

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
unchanged to the server's MCP endpoint, `<server>/v1/mcp`, so it does not know
the tool list. It needs a server URL and an API key, and it creates no
mailspace. The project id is optional: it is sent as the `project` argument of
a tool that declares one (the bridge fills it into a tool call that leaves it
out), and it is not part of the URL. A flag beats its variable, and a variable
beats `[sync]`:

| Setting | Flag | Variable |
| --- | --- | --- |
| Server base URL | `--server` | `VIVI_MCP_SERVER` |
| Project id (optional) | `--project-id` | `VIVI_MCP_PROJECT_ID` |
| API key | none | `VIVI_MCP_API_KEY` |

The API key is the only credential and it comes from the environment alone.
Mint a key
with `vivi sync api-key mint --label <text> --out <file>`; `--out` writes it to a
new mode-0600 file instead of printing it. `api-key list` and
`api-key revoke <key-id>` manage keys.

Two one-shot subcommands call the same endpoint without an MCP client, with the
same flags and variables. An MCP client caches the tool list when its session
starts, so a tool a server upgrade adds stays invisible until a restart; these
commands read the live list and call any tool now:

```sh
vivi mcp tools [--json]                      # name and first description line
vivi mcp call overview '{}' [--json]         # ARGS_JSON, or --args-file PATH|-
```

`call` prints the result text (`--json`: the full result object). It exits 0 on
success, 1 when the tool reports `isError` (its text goes to stderr), and 2 on a
transport, auth, or JSON-RPC error or arguments that are not a JSON object.

## Agent skills

Three skills ship in [`skills/`](skills/) and are embedded in the binary, so
installed skill text always matches `vivi --version`:

| Skill | Load it for |
| --- | --- |
| [`vivi`](skills/vivi/SKILL.md) | The CLI: mailspaces, roles, goals, work graphs, pools, sync, proxy. Load this one always |
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

- Vivi keeps no credentials in `mailspace.toml` or `proxy.toml`. Fly and RunPod
  tokens come from the environment or their own tools; the judgment provider
  key and the optional proxy bearer come from `key_cmd` / `auth_token_cmd`
  (run with `sh -c`); a project API key is read from `VIVI_MCP_API_KEY` or
  written to a file you name.
- `.vivi/` is machine-local and ignores itself in Git. It holds your sync key
  seed and your whole item history, so do not force-add it to a repository or
  copy it to a machine you do not trust.
- `vivi pool dispatch` runs an agent with the account and environment the pool
  gives it. A `local` pool has no isolation: the worker runs as you.

## License

MIT. See [LICENSE](LICENSE).
