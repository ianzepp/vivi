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
~/.local/bin/vivi-18.0.0
~/.local/bin/vivi -> vivi-18.0.0
```

The prefix is `~/.local`, so binaries live in `~/.local/bin` (usually
already on `PATH`). That is not `~/.vivi`, which is a mailspace
(`mailspace.toml`, `mail.sqlite`) and must never hold binaries.

```sh
curl -fsSL https://raw.githubusercontent.com/ianzepp/vivi/main/install.sh | sh
# or pin a release / prefix:
curl -fsSL https://raw.githubusercontent.com/ianzepp/vivi/main/install.sh | sh -s -- --version 18.0.0 --prefix "$HOME/.local"
```

The script defaults to the release it shipped with (`VIVI_VERSION`, currently
18.0.0) and `~/.local` (`VIVI_PREFIX`). `VIVI_BASE_URL` or `--base-url`
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
| `need` | A prioritised requirement, usually the parent of tasks | send, bind, assign, withdraw, done, reopen, mark |
| `want` | A later idea; never dispatched until promoted | send, set-priority, promote, done/drop, mark |
| `memo` | Durable per-role memory | save, list, search, show, delete |
| `rule` | A project rule every seat can read | save, list, supersede, retire |

Tasks, needs and wants accept `--depends-on <handle>` (repeatable). `absorb`
seals a record so it can no longer change.

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
build output. After a pump unit merges into the configured integration branch,
the pump can queue a local-only reclaim job for the unit's work pool and merge
role pool; promotion to main is separate. A local dispatch refuses when the
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
| `boot` | The whole project frame in one read-only read: seats, loops, unabsorbed mail, handles with verdicts, goals, the backlog sliced for dispatch, and a role-gated pump/pool snapshot. `--role`, `--pump` (`full`, `own`, or `none`), `--json` |
| `mailspace` | `init`, `status`, `description`, `archive` and `archive export`, `watch`, `import`, `identity add\|list\|rename` |
| `role` | `list`, `show`, `add`, `set`, `rename`, `pool`, `status`, `charter show\|set` |
| `mail` | `send`, `reply`, `deliver` (an `.eml` file), `list`, `show`, `thread`, `watch`, `absorb`, `dump` |
| `task` | `send`, `from` (derive from a handle), `list`, `show`, `done`, `reopen`, `watch`, `absorb`, `dump` |
| `need` | `send`, `list`, `show`, `done`, `reopen`, `assign`, `withdraw`, `bind`, `mark`, `unmark`, `watch`, `absorb`, `dump` |
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
| `pool` | `list`, `show`, `add`, `set`, `unset`, `remove`, `bands`, `create`, `status`, `destroy`, `scale`, `dispatch`, `wait`, `reconcile`, `clean`, `reset`, `doctor` |
| `config` | `show [--resolved]`, `validate`, `migrate [--write]` |
| `pump` | `run`, `once`, `status`, `log` (`-n`, `-f`, `--lane`, `--unit`, `--grep`, `--exceptions`), `replan <need> [--force]` |
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
`slots`, `fallback` (next pool in the durable-job routing chain), `image`, `setup`, `seed`, `requires`, `facts`, `build_dir`, `depth`,
`restore`, and the sub-tables `bands` (`P0` to `P5`, each a list of
`{ model, reasoning }`), `github` (`app_id`, `private_key`), `harness` (`name`,
`run`, `models`, `env`, `rules`), `committer` (`name`, `email`), `bridge`
(`enabled`), `workspace` (local provider only), and one table per provider (`fly`, `docker`, `ssh`, `local`,
`runpod`). A `local` pool with `[pools.<name>.local] worktree_of = "<repo>"`
makes each slot a Git worktree of a repository already on the machine, with no
network. With a single-target pump, the pump reads a local worktree pool's
unit branches straight from that repository (collect checks it there, merge
fetches from it), so no `shell_prefix` push hook is needed. Credentials are
not kept in the config: the Fly token comes from
`FLY_API_TOKEN` or `FLY_ACCESS_TOKEN` (or `fly auth token`), and a harness
receives only the environment variables its `env` list names.

`vivi pool doctor` checks pools, providers and repositories and names the fix
for each finding. `doctor --test` creates throwaway slots (billable on Fly; on
RunPod it rents a GPU pod and also needs `--allow-spend`).

Pool identity uses the exact mailspace UUID plus the pool name; the name slug
is used only when the mailspace has no id. Shared-root slots use that key in
their machine names or directory paths. Project-relative local roots are
scoped by the project path. Slots made under older slug or short-prefix keys
are not migrated or adopted under the UUID key; check provider inventory if
old resources need cleanup.

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

The default size is 8 GiB, with 6 GiB kept for the host; a gate pool may need
32 GiB, and a full Rust workspace test build can need 12. The default
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

Roles (**[[identities]]**) and pools (**[pools.&lt;name&gt;]**) are configured
separately. Unit tasks are sent to the role named by **[pump].to**. When
the hand role has a pool, that pool and its fallback chain are the unit's
primary route; **tools** may be empty or name an unknown kind. Defined kind
metadata still applies, but kind pool lists are ignored. If the hand role has
no pool, configured kinds and **default_kind** route unit tasks.

Durable pool jobs are addressed to their work role: planner jobs use
**pump-planner**, integration jobs use **merge**, gates use **gate**, audits and
investigations use **auditor**, promotions use the active **release** role
with a pool or fall back to an active **merge** role with a pool, cadence jobs
use the scheduled role, and reclaim uses **reclaim** or the hand role.
Each role's assigned pool and configured fallback chain provide candidates.
A job stays pending when none has capacity. A live merge pipeline needs a
**merge** role with a pool; the old **[pump.merge].lanes** value does not run
local merges. Gate jobs use the gate role and its fallback, with
**[pump.gate].pools** available as additional candidates. If no auditor role is
configured, gate audits have a built-in read-only fallback using gate pools;
investigations require an auditor role. Managed need acceptance audits require
an active auditor role with a configured pool; the gate-audit fallback does
not replace it.

**vivi mailspace init** and **vivi sync join** provision the active **merge**
role and its shipped verification charter when either is absent. They preserve
existing role settings and charter files. Pool and model routing remain
operator-configured.

Concurrency comes from a pool's **slots**. Optional **[pools.&lt;name&gt;.caps]**
holds per-model caps inside that pool. A role may name a model slug, a band
(**P0** to **P5**, optionally **-S0** to **-S3**), or a list of models. The
pool's **harness.models** is the allow-list.

**read_only** is optional. Kinds default read-only for auditor, triage,
cadence/steward, head, and gate; hand, merge, and lint write. Existing
**--read-only** flags still force read-only.

A pool's **fallback = "&lt;pool&gt;"** points to the next candidate. The target
must exist and cycles fail validation. Durable role jobs follow the role pool
and then its fallback chain; unit tasks use the same fallback chain when the
hand role has a pool. With no free candidate, the work waits. Without a hand
role pool, **[pump.dispatch.kinds]** and **default_kind** route units. With a
hand role pool, a unit's **tools** value may be empty or name an unknown kind;
defined kinds still apply **max** and **skip_proof**, while their pool lists
are ignored. **[pump.dispatch.lanes]** supplies legacy model caps when a pool
has no **[pools.&lt;name&gt;.caps]**.

**vivi config migrate** (dry-run by default; **--write** applies and keeps a
timestamped **.bak**) brings a 15.0.0 **mailspace.toml** to the current shape:
upgrade with **vivi update**, run **vivi config migrate**, review the plan, then
run it again with **--write**. It keeps comments and does the following.

- Assigns the hand, merge and gate roles a pool from the legacy
  **[pump.dispatch]**, **[pump.merge]** and **[pump.gate]** tables, and folds
  **[pump.dispatch.lanes]** into **[pools.&lt;name&gt;.caps]**.
- Adds the pump's own planner role, **pump-planner** (kind **planner**, active),
  on the hand role's pool, unless it exists; with no hand pool to copy it says
  so instead. A role named **planner** (a Tugboat seat) is left alone. A
  **pump-planner** with a blank charter gets the text of the file
  **[pump.planner] charter** names, when readable (so the project's planning
  rules carry over), else the shipped pump-planner charter. Charters are
  written only under **--write**, an existing charter file is copied to a
  **.bak** first, and a charter that has text is never replaced.
- Gives the auditor role the hand role's pool, adding an active read-only
  **auditor** role when there is none, because need acceptance audits have no
  fallback. A blank auditor charter gets the shipped one; one that has text is
  kept and reported, since it is now the system prompt of those audits.
- Sets **kind = "hand"** on the **[pump].to** role when it has no kind: only a
  hand-kind role may publish unit branches. Another kind is reported, not
  changed.
- Adds an active **merge** role on the pump-host pool when there is none, and
  sets **kind = "merge"** on a merge role of another kind: only a merge-kind
  role may publish to the integration branch, and **vivi config validate**
  reports a merge role of any other kind as an error. A merge role whose
  charter is not the shipped one is reported: 16 merge jobs use it as the
  system prompt.
- Renames **[pump.planner] split_timeout** to **split_hard_cap**.
- Removes the keys the current release ignores, each with its reason in the
  plan: **[pump.planner] run** and **charter** (planning runs as durable planner
  jobs on the pump-planner role's pool), **[pump] priority_file** (priority lives in
  **pump.db**), **[pump.merge] lanes** (merge concurrency follows the merge
  role's pool slots), and **[pump.gate] every_merges** and **every_seconds**.
  The last two are a behaviour change: the gate runs after every merge batch
  that lands, not on a merge count or a timer. The plan marks those lines.
- Sets **root = "~/vivi-pool"** on local pools without a **root** when the
  mailspace has no **mailspace.id**, so existing slots keep being used. With an
  id, 16 keys slot directories on the id and defaults to
  **&lt;project&gt;/.vivi/pool**, so 15.0.0 slots are not reused; the plan
  names the pools and the old directory to remove.

The plan ends with **review:** lines for what it cannot settle, such as a new
merge pool that needs a source and warm slots, or roles that need a charter.
A second run on a migrated file reports no changes.

When **[pump]** is configured, **vivi config validate** requires an active
**pump-planner** role, a merge role of kind **merge**, an active auditor role
and the **[pump].to** role of kind **hand**, each with a pool where one is
needed; **vivi doctor** reports the same requirements. It validates roles and
pool references, fallback targets and cycles, model bands, read-only settings,
and configured pump requirements. Run it before starting the pump; it also runs at **vivi pump
run|once** startup.

**vivi config show**, **vivi config validate**, and **vivi config migrate**
inspect, validate, and migrate routing. **vivi role pool** and **vivi role
set** assign role settings. **vivi pool list**, **show**, **add**, **set**,
**unset**, and **remove** manage pools; **vivi pool bands set|unset** sets
model bands. **vivi pool clean** and **reset** manage slot checkouts.

Writers preserve comments and only write keys that were set. A full pool and
all of its configured fallback candidates leave the work pending.

## The pump

The pump coordinates the backlog lifecycle and routes work to durable pool
jobs. Planner workers plan and revise needs; Merge workers integrate branches
and run the configured checks; Gate, Auditor, and release workers handle their
own jobs. The pump records each job before choosing a slot. Jobs without
capacity stay pending, and dispatched work remains owned by its pool across
pump restarts.

| Work | Role and pool |
| --- | --- |
| Unit tasks | The role named by **[pump].to**. Its pool and fallback chain are primary; **tools** may be empty or name an unknown kind. Defined kind limits and proof-skip metadata still apply, but kind pool lists are ignored. Without a hand pool, configured kind pools route the unit. |
| Whole-need, revision, and split planning | The active **pump-planner** role and its pool; its charter is the planner's system prompt. This role with a pool is required when **[pump]** is configured. |
| Integration merges and need branches | The **merge** role and its pool. This role integrates sources, runs the assigned checks, and returns a candidate commit; the privileged pool runner publishes it. |
| Gates | The read-only **gate** role's pool and fallback chain, then configured **[pump.gate].pools** candidates. |
| Audits and investigations | The **auditor** role and its pool, followed by gate-pool candidates for audits. Without an auditor role, gate audits use a built-in read-only fallback; investigations require an auditor role. |
| Need acceptance audits | An active **auditor** role with a configured pool and fallback chain. Gate-pool fallback does not satisfy this route. |
| Promotion | The active **release** role with a pool, or an active **merge** role with a pool as fallback. |
| Cadence and digest | The role whose schedule is due. |
| Reclaim | The **reclaim** role, or the hand role, using local pools only. |

A pool can name a **fallback** pool. Durable jobs try the role's assigned pool
and then its fallback chain; they remain pending when no candidate has a free
slot. **vivi config validate** checks pool references and rejects fallback
cycles. The legacy **[pump.dispatch.kinds]** tables can still route unit tasks
when the hand role has no pool. **[pump.planner].run** and **charter** are not
executed.

The pump saves its live state, including the durable jobs map, in
**&lt;project&gt;/.vivi/pump/pump.db**. A restart reattaches jobs by their stable task
markers, reads pool receipts, and applies results through the ordinary tick
path. **vivi pump stop** finishes the current tick and exits; it does not cancel
or drain pool jobs. Those jobs continue in their pools and are recovered by
the next pump start. A paused pump collects completed work and performs board
bookkeeping, but starts no new dispatch, merge, or gate work. Valid routing,
pool, dispatch, merge, gate, budget, and check changes are loaded between
ticks; changing the pump home or repository target paths requires a restart.

For a shared integration merge, its worker integrates the listed unmanaged
unit and completed role branches, runs the configured formatter, shared
checks, matching scope checks, and unit proofs, then commits a candidate
without pushing. The privileged pool runner
publishes the candidate to the configured integration ref. The pump checks
the runner publication receipt, exact ref and candidate, and remote ancestry
before marking units merged. Promotion is also a role job and is recorded
only after the configured main ref is verified to contain the gated commit.
See [the pump contract](skills/vivi/references/pump-contract.md) and
[the receipt protocols](skills/vivi/references/pump-protocols.md) for exact
state and result contracts.

Managed needs keep unit work on a per-need branch until the current plan
revision is complete and its frozen range passes acceptance. The auditor
reviews the exact need, target, revision, base, and tip; only an accepted range
is sent to the Merge role for publication to the integration branch. Residual
findings become ordinary follow-up needs, while blocking findings keep the
range held for repair or operator review. Landing does not promote to main:
the independent integration gate must pass before the release path can
promote its pinned tip.

A gate is due when there is no baseline, when a merge has landed since the
previous gate, or when **FORCE-GATE** requests one. The legacy
**every_merges** and **every_seconds** values are ignored. The gate pins one
integration tip and runs read-only. New or residual failures go to a durable
auditor job; its valid report creates fix units. An optional external runner
can supply a gate result while its configured heartbeat is fresh.

**vivi pump replan &lt;need&gt;** queues a fresh plan for the next tick. It supersedes
the existing plan file, withdraws pending, held, stuck, and **needs-replan**
units, keeps merged units, and plans from the current thread. It refuses
while live unit work or a planner is running for that need unless **--force**
is supplied; forced replanning leaves live work running.

Role cadence uses each role's latest durable outbound signal. Due runs appear
in the board and pump status. Head-ceo investigations use a need marked
**investigate** with a body line **for: &lt;want-handle&gt;**; the need closes only
when a recent auditor report reply lands on that want addressed to head-ceo. The
default cap is two rounds per want, configurable with
**investigation-rounds: N** in the head-ceo charter.

The pump home defaults to **&lt;project&gt;/.vivi/pump**. It stores **pump.db**,
**status.txt**, **log.txt**, **exceptions.txt**, plan/job briefs, and bounded tick
receipts for **vivi monitor**. **HOLD-MERGE**, **FORCE-GATE**, **skip.txt**, and the
reserved-path file are operator controls. Use **vivi pump pause**, **vivi pump resume**, **vivi pump stop**, and **vivi pump start**;
there are no live STOP or PAUSE marker files. The old **REMOTE-MERGE** flag,
**merge-jobs/** protocol, detached planner reapers, and local merge thread are
not the live execution path.

Use **vivi monitor** for persisted task, pool, pump, and tick snapshots.
Selecting a monitor row tails its raw log; log contents are not redacted. Use
**vivi report** for records and grouped throughput, duration, need, and failure
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
| `native` | In-process adapter (`chatgpt-codex` or `kimi-coding` / `openai-passthrough`) |
| `child` | Vivi spawns the command, health-checks it, restarts with backoff |
| `upstream` | Reverse-proxy to a server that is already listening |

Native Codex reads `~/.codex/auth.json` (then `~/.pi/agent/auth.json`) in
place, refreshes 30s before JWT expiry under an advisory lock, writes back
atomically at mode `0600`, filters SSE to an allowlist and rebuilds an empty
`response.completed.output`, and keeps the existing non-streaming, tool-less,
usage-0 chat shim. Native Kimi re-reads its credential file on every request,
injects `X-Msh-*` identity headers, passes bytes through, and retries once
after a 401 refresh. Cursor stays a `child` (Bun/protobuf engine). Local
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

### Migration from separate `com.ianzepp.proxy.*` jobs

Point each Pi provider `baseUrl` at the one listener and retire the old
per-port jobs:

| Provider | Old base | New base |
| --- | --- | --- |
| Codex | `http://127.0.0.1:18181` | `http://127.0.0.1:18180/codex/v1` |
| Kimi | `http://127.0.0.1:18182/v1` | `http://127.0.0.1:18180/kimi/v1` |
| llama | `http://127.0.0.1:18173/v1` | `http://127.0.0.1:18180/llama/v1` |
| Cursor | `http://127.0.0.1:18183/v1` | `http://127.0.0.1:18180/cursor/v1` |

Keep the Python Codex/Kimi scripts as `kind = "child"` if you want a fallback
instead of the native adapters.

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
then `~/.kimi/credentials/kimi-code.json` for Kimi).

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
