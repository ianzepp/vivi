# Vivi

Project mailspace for private agents: durable, project-local coordination state
— tasks, needs, wants, mail, memos, roles, goals, and executable work graphs —
driven by the `vivi` CLI.

Every project keeps its own `.vivi/` directory. Vivi writes coordination state
there as ordinary files plus one SQLite database, so a group of agents can share
a project's work with no server and without leaving the repository.

Vivi is one of three sibling repositories, split from a single project on
2026-09-21:

| Repo | Owns |
| --- | --- |
| [`vivi`](https://github.com/ianzepp/vivi) (this repo) | Project mailspaces, roles, goals, work graphs, the `vivi` binary |
| [`vivi-mail`](https://github.com/ianzepp/vivi-mail) | IMAP, SMTP, the direct Proton API, sync, send, the local email archive, search, drafts |
| [`vivi-pty`](https://github.com/ianzepp/vivi-pty) | The project-scoped PTY runtime adapter |

They share no code and no data. Vivi owns `<project>/.vivi/`; the email account
configuration and archive that live under `~/.vivarium/` belong to `vivi-mail`.

## Why

Agents working in a repository need somewhere to record what must happen, what
they need from each other, and what they have learned — in a form that survives
restarts, context compaction, and hand-offs between different models. Vivi keeps
that state inside the project: tasks, needs, wants, and memos are messages with
lifecycles, goals are registered file paths, and an executable work graph turns
dependencies into a ready frontier a fleet can dispatch from.

The properties that matter are durability, project locality, and honesty about
what has actually been completed.

## Install

Current release: **11.2.0**
([GitHub releases](https://github.com/ianzepp/vivi/releases),
[notes](docs/release-v11.2.0.md)).

Release binaries are published as assets on this repository's GitHub releases,
and that is the only distribution channel — there is no package-manager formula
to install or maintain.

With curl on macOS or Linux, which selects the archive for your platform and
falls back to a source build where none exists:

```sh
curl -fsSL https://raw.githubusercontent.com/ianzepp/vivi/main/install.sh | bash
```

By hand, download the archive for your platform from the
[latest release](https://github.com/ianzepp/vivi/releases/latest), unpack it, and
put `vivi` on your `PATH`:

- `vivi-aarch64-apple-darwin.tar.gz`
- `vivi-x86_64-apple-darwin.tar.gz`
- `vivi-x86_64-unknown-linux-gnu.tar.gz`

Linux `aarch64` has no binary archive yet. On that platform the installer falls
back to `cargo install` from the release tag.

The installer takes `VIVI_VERSION` to pin a release tag, `VIVI_INSTALL_DIR` for
the destination directory (default `~/.local/bin`), and `VIVI_REPO` to install
from a fork.

The companion `vivi-pty` binary ships from
[`ianzepp/vivi-pty`](https://github.com/ianzepp/vivi-pty), and the email half
from [`ianzepp/vivi-mail`](https://github.com/ianzepp/vivi-mail).

From source, requires Rust 1.93+:

```sh
git clone https://github.com/ianzepp/vivi.git
cd vivi
cargo install --path .
```

## Agent skills

Agent-facing guidance ships as three skills under [`skills/`](skills/):

| Skill | Load it for |
| --- | --- |
| [`vivi`](skills/vivi/SKILL.md) | The CLI: mailspaces, roles, goals, work graphs, the remote lane pool. Load this one always. |
| [`tugboat`](skills/tugboat/SKILL.md) | A deliberate multi-agent protocol on a Vivi backlog: a Mind routes, Hands implement, Auditors and Heads review, Cadence advises. |
| [`speedboat`](skills/speedboat/SKILL.md) | A fast delivery protocol on the same backlog: small parallel Hands, minimum verification, the Mind integrates. |

`tugboat` and `speedboat` are alternatives; each needs only the `vivi` skill.

The same trees are embedded in the binary, so an installed `vivi` prints
skill text that matches `vivi --version`. No mailspace is required:

```sh
vivi skills list
vivi skills show vivi
vivi skills show vivi --file references/work-graph-flow.md
vivi skills install ~/.agents/skills
```

`show` without `--file` prints `SKILL.md` and lists every supporting file
(so relative links can be requested). `install` writes the repository
layout (`vivi/`, `tugboat/`, `speedboat/`) into the directory; `--force`
overwrites. The checkout `skills/` tree remains the source that the next
build embeds.

## Quick Start

Vivi works from a project directory. Reads need an existing mailspace; most
write commands create one on first use.

```sh
cd /path/to/your/project
vivi mailspace init     # create .vivi/ with mailspace.toml and mail.sqlite
vivi boot               # read the whole project frame in one command
vivi board              # actionable work across tasks, needs, and wants
```

`--project <root>` names the project from anywhere, before or after the
subcommand. Without it, Vivi uses its own working directory and looks up the
nearest ancestor holding `.vivi/mailspace.toml`.

A project declares the identities it may use, and the probes `boot` should run:

```toml
# .vivi/mailspace.toml
name = "myproject"

[[identities]]
name = "mind"
kind = "mind"

[[identities]]
name = "hand-1"
kind = "hand"

[[probes]]
name = "git"
command = "git rev-parse --short HEAD"
```

The rest of this document is the mailspace reference.

## Project Mailspaces

Project mailspaces are local-only mailboxes for project-scoped agent addresses.
They are explicit: Vivi discovers an existing `.vivi/mailspace.toml` by walking
upward from the current directory, but it never creates `.vivi/` as a side
effect of send, list, search, or show commands.

```sh
cd /path/to/project
vivi mailspace init
vivi role add ceo --kind head
vivi role add cto --kind head --harness subagent
# legacy alias still works:
# vivi mailspace identity add hand-1
vivi mailspace status
vivi boot
vivi board
```

### Boot (one-read project frame)

`vivi boot` renders the whole project frame in a single bounded read: seat
bindings against observed process state, declared cadences and their silence,
unabsorbed mail, open handles with verdicts, registered goals with register
tallies, memos, charter heads, and the backlog sliced into seat-sized groups.
Use it first when orienting, or after a compaction resets the working picture.

```sh
vivi boot --project /path/to/project
```

It is read-only, stateless, and idempotent: two runs are comparable, and boot
never absorbs, closes, promotes, dispatches, or writes. Act on the frame with
the ordinary verbs.

Every section is capped, and the digest closes with a truncation manifest that
names each cap and how much it omitted. Handles carry a closed verdict
vocabulary (`open`, `blocked`, `stale`, `live`, `unbound`, `unverified`,
`dead`, `zombie`, `remote`, `unknown`), and a registered goal whose Status line
claims a different completion count than its own register is reported as
`MISMATCH`.

Facts Vivi cannot own — git ancestry, lane state, the live seat count of a
harness — arrive through project-declared probes:

```toml
[[probes]]
name = "example"
command = "scripta/boot-probe"   # executable, resolved against the mailspace root
```

A probe prints one JSON object on stdout with `facts` (flat preamble lines),
`sections` (named blocks of lines), and `verdicts` (`handle` plus a verdict
slug and an optional detail). A probe that is missing, exits non-zero, or
prints unparseable JSON is reported under `probes skipped` and never fails
boot, so a project with no probes still gets every native section. See
[`skills/vivi/SKILL.md`](skills/vivi/SKILL.md) for the full contract.

### Roles (agent seats)

Roles are first-class mailspace seats. Each role owns a local mailbox name plus
durable metadata used by multi-agent hosts (especially sub-agent spawns):

| Field | Meaning |
| --- | --- |
| `name` | Local-part / mailbox key (`head-ceo`, `hand-1`) |
| `kind` | Process class (`hand`, `head`, `mind`, `operator`, `steward`, or freeform) |
| `status` | Lifecycle (`active`, `parked`, `retired`, or freeform); default `active` |
| `labels` | Freeform slugs (`auditor`, `floater`, …) |
| `harness` | Execution home (`subagent`, `tmux`, `vivi_pty`, …) |
| `provider` / `model` / `thinking` | Desired capacity (not process liveness) |
| `pid` / `host` | Live process binding; self-set by the role at boot (PID-file semantics). `host` defaults to the local hostname when `pid` is set |
| `cadence` | Optional maximum silence between outbound signals (`15m`, `1h`, …). General-purpose; often used for heads/stewards |
| `charter` | Standing prompt body for the seat (stored under `.vivi/charters/`) |
| `address` | Derived: `{name}@{prefix}.local`, where `prefix` is the first 8 characters of the mailspace id |

```sh
vivi role list --json
vivi role show head-ceo --json
vivi role set hand-1 --provider zai --model glm-5.2 --thinking low
vivi role set head-ceo --harness subagent --cadence 15m
vivi role set head-ceo --clear-cadence
vivi role charter set head-ceo --file personas/ceo.md
vivi role charter show head-ceo
```

### Process status and schedule (by role name)

A role self-registers its live process at boot so any agent can ask "is this
seat's process alive?" by role name alone, without knowing the pid or the
backend. Liveness is computed fresh on each call; nothing observed is stored.

```sh
# At boot, the role's own process writes its pid (host defaults to local):
vivi role set hand-1 --pid $$ --project <root>

# Any agent checks by role name:
vivi role status hand-1 --project <root> [--json]
```

`role status` reports process `state` (`alive`, `zombie`, `dead`, `not_set`,
`remote`, `unknown`), `running`, and — for a live local pid — `name`,
`cpu_percent`, `memory_bytes`, and `uptime_seconds`. If the stored `host`
differs from the local host, it reports `remote` rather than probing the local
table (so a pid on `pharos` is not falsely read as dead from another host). CPU
is sampled with a short two-read interval, so a live pid costs ~200 ms. Clearing
the pid also clears the host.

When a role has `cadence`, `role status` also reports **schedule** health from
the age of that role's latest outbound mailspace message (memos excluded):

| Schedule state | Meaning |
| --- | --- |
| `none` | No cadence configured |
| `never` | Cadence set, no outbound signal yet |
| `ok` | Last signal younger than one cadence (+10% grace) |
| `due` | Silence between one and two cadences |
| `overdue` | Silence at or beyond two cadences |

`due` is advisory visibility for the Mind. `overdue` is an action-required
signal after two full cadence intervals. Board JSON includes `model`,
`thinking`, and a `schedule` block per identity; `schedule.action_required` is
true only for `overdue`. Text output prints configured model capacity, prints a
schedule line when state is not `none`, and marks overdue roles `ACTION REQUIRED`.

```sh
# Bulk capacity flips stay outside vivi (one role per mutation):
for r in head-ceo head-cto hand-1; do
  vivi role set "$r" --provider zai --model glm-5.2 --thinking high
done
```

Parent agents should pass **pointers**, not paste charters:

```text
You are Vivi role head-ceo.
Load charter: vivi role charter show head-ceo --project <root>
Load task:    vivi task show <handle> --project <root>
```

`vivi mailspace identity add|list|rename` remains as a thin roster path; prefer
`vivi role` for new work. Existing `[[identities]]` entries in
`mailspace.toml` load as roles with empty optional fields.

Every mailspace has an id: a UUID minted the first time the mailspace is
opened and stored as the top-level `id` in its `.vivi/mailspace.toml`, which is
machine-local and uncommitted: `.vivi/` ignores itself with its own
`.gitignore`, so nothing in it is committed. The first 8 characters are the
domain part of every address the mailspace mints, so `cto` in a mailspace whose
id begins `3f9a1c7e` is `cto@3f9a1c7e.local`. Do not copy `mailspace.toml` between
machines: two mailspaces with one id collide at `vivi sync join`.

Bare names, aliases, `cto@local`, the own prefix address, and the legacy
`cto@<project-name>.local` all resolve to the local role. An address with a
different 8-hex prefix (`mind@9f8e7d6c.local`) names a role in another
mailspace: `vivi mail send`, `need send`, `task send`, `want send`, and
`mail reply` accept it next to local roles, write it in the letter's To or Cc,
keep only the sent copy locally, and print `queued for sync: ...`; `vivi sync`
carries the letter. `mail deliver` accepts them too. Without a `[sync]` table
the command prints a note that the letter will not leave this checkout, and with
one a letter over 64 KiB is refused at compose time. A reply to a pulled letter goes to its From with no extra
flags. Unknown local roles and other external addresses are rejected. Use the
existing `compose`, `enqueue send`, and `exec send` flows for human or external
mail.

Local agent mail is stored as raw RFC 5322 `.eml` blobs under `.vivi/blobs/`
with mailbox state in `.vivi/mail.sqlite`:

```sh
vivi mail send --from ceo --to cto \
  --subject "review: local delivery" \
  --body "Please review the API shape."

vivi mail list --for cto
# handle  date  from  subject  (add --json for structured output)
vivi mail list --for cto --json
vivi mail list --from ceo
vivi mail list --to cto
vivi mail list --for cto --from ceo
```

Tasks are ordinary local messages delivered to the recipient's `Tasks` folder.
Completing a task moves the same message to `Done`, so the handle remains
stable across the lifecycle.

```sh
vivi task send --from ceo --to cto \
  --subject "Implement local delivery" \
  --body @task.md

vivi task list --for cto
vivi task list --for cto --json
vivi task list --from ceo
vivi task list --for cto --status all
vivi task done <handle> --for cto
vivi task list --for cto --status done
```

Replies are first-class local mailspace messages. Reply targets are
kind-agnostic, so a mail can answer a need and a task can continue that same
conversation. The parent link is captured by stable content identity and
survives a task or want moving to `done`:

```sh
vivi mail reply <handle> --from cto --body "Reviewed and approved."
vivi mail send --from cto --to ceo --subject "Follow-up" \
  --body "Implement the next step." --reply-to <handle>
vivi mail thread <handle> --json
# Bound a large conversation walk when needed
vivi mail thread <handle> --json --limit 100 --max-depth 20
# Trace the cross-role communication tree around a task, want, or mail
vivi trace <handle>
vivi trace <handle> --json --max-depth 5 --limit 100
# Import an executable Mermaid work graph (project-local topology)
vivi graph import --code mir-swarm-wave-2 --file wave.mmd --check --json
vivi graph import --code mir-swarm-wave-2 --file wave.mmd --json
# Topology is always Mermaid (add --include-state for readiness classes)
vivi graph show mir-swarm-wave-2
vivi graph show mir-swarm-wave-2 --include-state
# Status loops: compact frontier, not a topology dump
vivi graph ready mir-swarm-wave-2
vivi graph ready mir-swarm-wave-2 --json
vivi graph apply mir-swarm-wave-2 --file wave-v2.mmd --json
vivi graph complete mir-swarm-wave-2:verify --json
vivi graph activate mir-swarm-wave-2:verify --task <task-handle> --json
vivi graph export mir-swarm-wave-2 --include-state
vivi graph node add --graph mir-swarm-wave-2 --id u4 --label "G-P-10/U4"
vivi graph edge add --graph mir-swarm-wave-2 --from accept --to u4
vivi board --graph --json
vivi task show <handle> --json
```

Lifecycle `--note` values remain in the event ledger and also become normal
captured replies. `mail thread --infer` enables a read-only best-effort view of
older messages using handle citations and reply subjects; inferred links are
marked separately and never replace captured links.

`vivi trace` builds a cross-role tree around any local handle: it walks captured
reply links, `tasked` lifecycle events, and inferred body-citation links, and it
collapses same-content copies (e.g., sender `sent` and recipient `inbox`) into a
single logical node. Use `--json` for agent consumption and `--max-depth` /
`--limit` to keep large mailspaces bounded.

### Executable work graphs

`vivi graph` stores **executable work topology** separately from `vivi trace`
(communication tree). The whole backlog lives in the graph: every `task` /
`need` / `want` send mints a node in the per-mailspace `backlog` graph, and
`--depends-on` on any work-kind send (task/need/want handles) becomes a
prerequisite edge — one dependency substrate for all kinds.

| Concern | Authority |
| --- | --- |
| Planning topology + ready frontier | `vivi graph` (project `mail.sqlite`) |
| Backlog citizenship + dependencies | `backlog` graph (auto-minted; `--depends-on` on send) |
| Lowering: need → unit tasks | `need bind`; the need completes when all units land |
| Dispatch/exception manifest | `vivi step [--json]`; `--apply <handle>` completes settled items |
| Communication history | `vivi trace` |
| Who to spawn / when | The host, not Vivi. Bind an attempt with `graph activate --task` |

Lifecycle moves keep nodes in step (`task done` completes and unlocks
dependents; `reopen` re-locks; `want promote` changes nothing and wants never
dispatch in `vivi step` before promotion). Every node completion records a
`step_decision` graph event in the same transaction.

**Operator gates.** Imported nodes carry kinds: rhombus `id{label}` imports as
`decision`, an `id:::kind` suffix or `class <ids> <kind>` statement marks
`decision` / `stub` / `parked`, and everything else is dispatchable `task`.
Gated kinds never appear in `graph ready`'s ready list (they render under
`gates`), `graph activate` refuses them, and they resolve with
`graph complete --note` — an operator ruling is recorded, never dispatched.

**Dotted couplings never gate.** `-.->` / `-.-` edges import as non-gating
couplings: topology evidence that never blocks readiness. Export round-trips
them dotted. `-->` remains the only prerequisite arrow.

A `sent` copy of work addressed to another mailspace keeps its backlog node,
but the node is `awaiting`: `graph ready` lists it on its own line, `vivi step`
reports it as `awaiting_remote` with the recipient's address, and
`graph activate` refuses it. `need|task|want list --for <sender>` and `dump`
include the author's own open outbound copies (role `sent`), `--depends-on`
accepts them, and `done` settles the node and tells the receiving mailspace (see
State as letters); promoting an open sent copy is refused, because only the
receiving mailspace can. Work held for another
mailspace's role mints no node; `graph audit` reports a node left for it as
`foreign_held` and `--repair` settles it. Dependency edges follow the letters:
an `X-Vivi-Depends-On` header builds its edge on whichever mailspace files the
letter (send, pull, fetch, resync, audit repair), a dependency not held yet
waits for its letter, and `graph audit` reports a held letter whose header names
held work with no edge as `missing_edge` (`--repair` mints it). A letter for a
role this mailspace does not have is held under that name with no node (`boot`
and `sync status` list the names); `role add` or `role rename` to the name
adopts it.

`vivi step` adjudicates **only the `backlog` graph**. Imported topologies never
enter the step manifest — their nodes are dispatched with `graph activate` and
completed at reconcile. `graph ready` without an argument lists every graph's
frontier, including backlog.

Import a narrow Mermaid `flowchart` / `graph` (run `vivi graph import --help`
for the full accepted-subset summary); Vivi assigns
immutable handles, keeps Mermaid as revision evidence, and reports the ready
frontier (open roots). Use `--check` to validate without writing. Re-importing
identical source is idempotent. Later revisions use `graph apply` (source-id
reconciliation, freezes active/done prerequisites, allows new successors).

| Command | Effect |
| --- | --- |
| `graph import --code … --file …` | First create (or idempotent re-import) |
| `graph apply <code> --file …` | Additive revision of an existing graph |
| `graph show` / `export` | Mermaid topology only (`--include-state` optional) |
| `graph ready [--kind <k>]` | Compact frontier with counts for status loops |
| `graph audit [--repair]` | Backlog citizenship check; repair drift |
| `graph connect <dependent> <prereq>` | Post-hoc prerequisite between backlog items |
| `graph complete <code>:<id>` | Mark done; compact receipt (not full topology) |
| `graph activate <code>:<id> --task <h>` | Bind task attempt; compact receipt |
| `graph node add … [--kind <k>]` | Append a node, optionally a gate kind |
| `board --graph` | Frontier on the board JSON/text surface |

Watch graph lifecycle with `--kinds graph --events node_ready` (also
`node_state`, `attempt_bound`, `revision_imported`, `revision_applied`).

Needs and wants are also local messages with stable handles. Wants are parked in
`Wants` for later prioritization. Promoting a want moves it to `Needs`, where it
becomes first-cycle review material for the owner. Completing a need moves it
to `Done` without mixing it into completed task listings. A mark is an
append-only slug on a need or a want (`mark` / `unmark`). `show` prints the
folded set, and `list --mark` keeps items that carry every named slug.

```sh
vivi want send --from ceo --to ceo \
  --subject "Improve board visibility" \
  --body "Consider a future governance dashboard."

vivi want set-priority <handle> --for ceo \
  --priority P1 --rank 20 --repo app --lane correctness
vivi want list --for ceo --repo app --lane correctness \
  --sort priority,rank,created --json
vivi want promote <handle> --for ceo --note "Prioritize next cycle"
vivi need list --for ceo
vivi need mark <handle> bug regression --for ceo
vivi need list --for ceo --mark bug
vivi need unmark <handle> bug --for ceo
vivi need done <handle> --for ceo --note "Delegated and completed"
vivi need list --for ceo --status done --json
vivi want done <handle> --for ceo --note "No longer relevant"
vivi want list --for ceo --status done --json
```

For routine agent intake, start with status or board output, then show one
selected handle. `vivi board` summarizes actionable open tasks and needs first,
with wants capped as secondary backlog context. Prefer `--project <root>` when
the process cwd is not the mailspace root (both placements work):

```sh
vivi mailspace status --project /path/to/project --json
vivi board --project /path/to/project --for cto --json
vivi --project /path/to/project board --for cto --since 4h
vivi board --project /path/to/project --for cto \
  --watermark-file .vivi/agent-board.watermark --write-watermark
vivi task list --project /path/to/project --for cto --json
vivi need list --project /path/to/project --for ceo --json
vivi task show --project /path/to/project <handle>
```

`vivi board --process` adds a live process block per role (one role with `--for`,
or every role without it). It uses a quick probe — accurate `state`, `running`,
process `name`, `memory`, and `uptime`, but `cpu_percent` is null (a board scan
does not sleep for CPU samples; use `vivi role status <name>` for that). A role
with no binding reads `not_set`, which is the "available to assign" signal:

```sh
vivi board --process --project /path/to/project --json
vivi board --process --for hand-1 --project /path/to/project
```

`vivi board --graph` adds a `graphs[]` field (text section + JSON) for
executable work-graph frontiers without removing existing board fields. Each
node entry includes lifecycle state, readiness, blocked-by **handles**, and
successor handles. Use it with `--json` for agent intake of ready work across
campaigns.

Absorb seals a mail, task, need, want, or memo. After absorb, that record
cannot be moved, reopened, prioritized, deleted, or otherwise changed. A second
absorb of the same handle is a no-op. Replies and `task from` still create new
records.

Configure a dedicated git repo as the historical archive. Newly absorbed records
are written as Markdown with TOML frontmatter. Files are rewritten only when the
rendered bytes change. `archive export` backfills records absorbed before the
archive was configured:

```sh
vivi mailspace archive --project /path/to/project --set /path/to/vivi
vivi mailspace archive --project /path/to/project
vivi mailspace archive export --project /path/to/project
vivi mailspace archive --project /path/to/project --clear
```

```sh
vivi mail absorb --project /path/to/project --for mind <handle> \
  --note "Converted to priority request"
vivi task absorb --project /path/to/project --for hand <handle>
vivi need absorb --project /path/to/project --for ceo <handle>
vivi want absorb --project /path/to/project --for mind <handle>
vivi memo absorb --project /path/to/project --for mind <handle>
vivi mail list --project /path/to/project --for mind --status unabsorbed --json
vivi mail list --project /path/to/project --from mind --json
vivi mail list --project /path/to/project --to hand --json
vivi mail dump --project /path/to/project --for mind \
  --status absorbed --absorbed-by mind --json
```

When capacity opens, create executable tasking from a source handle while
preserving source lineage. The initial supported source kind is a want:

```sh
vivi task from <want-handle> --project /path/to/project \
  --for mind --to hand-2 \
  --subject "Fix the prioritized issue" --body-file task.md
```

For compact Mind-style intake, `cycle intake` gathers unabsorbed mail,
completed tasks since the cursor, open needs, and priority-sorted wants:

```sh
vivi cycle intake --project /path/to/project --for mind \
  --cursor-file .vivi/mind-cycle.cursor --write-cursor --json
```

### State as letters

Closing and reopening work are letters, not local flags. The role that authored
a need, task, or want (or holds its `sent` copy) and any role of kind `mind`
may close or reopen it for everyone: their `done`, `drop` and `reopen` each
write a small reply on the item with `X-Vivi-Event: done|reopened`,
`X-Vivi-Event-Of: <item Message-ID>` and `X-Vivi-Event-Prev: <the event it
follows>`. The letter goes to the item's To and Cc minus the sender, files
read, and carries `--note` as its body. Every mailspace holding the item
applies it: folders move and nodes complete or reopen, so the assignee's queue
empties when the owner closes and a reopen brings the item back. A mind may
name any copy of the item in the mailspace (the delivered one, its own, the
author's `sent` copy) and prints `done <handle>; event <id>`.

Any other role reports with an ordinary reply and prints `done <handle>;
reported`. `task done --note` moves its own copy and sends the note, and never
changes the item's state; a report cannot reopen what its owner closed. The
current state is the chain head, the
event no other held event names as `Prev`; a fork resolves by the later
`(Date, Message-ID)` on every checkout, and the next event names all heads. An
event counts only when its From domain is the item's From domain. Anything
else is filed as plain mail and counted in the pull summary, as is an event
for an absorbed item.

State is a function of the letters held, so it works with no `[sync]`: the
server only carries them. `pull`, `fetch`, `resync` and `pull --refs` apply
every event they see, an event that arrives before its item is applied when the
item does, and `vivi graph audit` reports a folder that disagrees with its
item's latest event as `event_state` (`--repair` re-applies it).
`need|task|want show` prints `State: done|reopened`.

### Remote lane pool

`vivi pool` manages preseeded remote machines so agent work can start in
seconds against a warm build, and hands a Vivi task to one of them. The pool
runs on Fly (the default), on Docker (`provider = "docker"`: containers on the
local daemon, or on a remote one over SSH), or, as a minimal form that is not
the recommended one, on a plain host reachable by SSH (`provider = "ssh"`: each
slot is a directory and the agent runs there as the login user, with no
isolation), or on the machine Vivi itself runs on (`provider = "local"`: each
slot is a directory and the worker is a process Vivi launches there, as you,
with no isolation, optionally as a Git worktree of a repository on this
machine), or on rented GPU pods (`provider = "runpod"`: one short-lived pod per
dispatch, with no resting slots; see "RunPod: short-lived GPU pods");
`[pools.<name>.fly]`, `[pools.<name>.docker]`, `[pools.<name>.ssh]`,
`[pools.<name>.local]` and `[pools.<name>.runpod]` are the host-specific parts
of the config.

#### Named pools

A mailspace holds any number of **named pools**, each a `[pools.<name>]` table
in `.vivi/mailspace.toml`. The name is the table key (lowercase letters, digits
and dashes, starting and ending with a letter or digit, and not ending in
`-doctor`). Every pool table is complete on its own: everything a pool needs is
written in it, and nothing is inherited from another pool (there are no shared
defaults). The old single `[pool]` table is gone; a mailspace that still has one
gets an error naming the new form on the first `vivi pool` command, and its
text is left alone. Typical use: a small `local` pool (Docker on this machine)
for ordinary work, and a larger rented pool (Fly) that is scaled up for a big
push and back down afterwards.

```toml
[pools.local]                         # small, free, always there
provider = "docker"
repo = "owner/name"
slots = 4
image = "ubuntu:24.04"
build_dir = "target"
# ...setup, seed, [pools.local.github], [pools.local.harness], [pools.local.committer]

[pools.local.docker]
memory = "4g"

[pools.lanes]                         # rented, scaled out when needed
provider = "fly"
repo = "owner/name"
slots = 32
image = "rust:1.97-bookworm"
build_dir = "target"
# ...the same sub-tables again, written out in full

[pools.lanes.fly]
app = "some-app"                      # required for a Fly pool: no default
```

```
vivi pool create --pool <name> [--slots N] [--base <commit>] [--porcelain]   # make the app if missing, create + seed slots, wait until free
vivi pool scale --pool <name> <N> [--yes] [--base <commit>] [--porcelain]   # bring the pool to N slots
vivi pool status [--pool <name>] [--json | --porcelain]   # pool, name, id, state, task, running time of budget, seeded base
vivi pool destroy --pool <name> (--slot <name> | --all) --yes
vivi pool dispatch --pool <name> <task> --model <slug> [--from <ref>] [--branch <name>] [--budget <secs>] [--allow-default-branch] [--ready-timeout <secs>] [--porcelain]   # --ready-timeout: RunPod only
vivi pool dispatch --pool <name> <task> --model <slug> --read-only [--from <ref>] [--budget <secs>] [--porcelain]   # publishes nothing
vivi pool wait [--pool <name>] [<task>...] [--timeout <secs>] [--porcelain]   # block until a dispatch finishes; collect it
vivi pool reconcile [--pool <name>] [--porcelain]   # collect finished, stop overdue, free slots
vivi pool doctor [--pool <name>] [--json | --porcelain]   # read-only checks; each finding names its fix
vivi pool doctor --pool <name> --test [--long] [--allow-spend] [--json | --porcelain]   # also run a live test of the provider and of Vivi (billable on Fly; free on Docker, SSH and local; on RunPod it refuses without --allow-spend)
```

**`--pool`.** When the mailspace has exactly one pool the flag may be omitted
everywhere. With more than one: `create`, `destroy`, `dispatch`, `scale` and
`doctor --test` require it and fail with a message that lists the pool names;
`status`, `doctor` (without `--test`), `wait` and `reconcile` cover **all** pools
when it is not given and one pool when it is. There is no default-pool setting.
Each dispatch record carries its pool name, so `wait` and `reconcile` open the
right machine host for each dispatch. A dispatch whose pool is no longer in the
config is reported (`wait` shows it as a problem and leaves it open) and
`reconcile` closes it as `lost`; a record made before pools were named has no
pool and is handled the same way. An unknown `--pool` lists the known names.

**`scale --pool <name> <N>`** brings a pool to N slots. Going up creates and
seeds what is missing (it is `create --slots N`, so it needs the pool's
repository, or `worktree_of` on a local pool). Going down destroys **free** slots only, highest index first, and
needs `--yes`; it never destroys a busy, seeding, finished-uncollected or
unseeded slot, and prints each one it passed over as `skipped` (the pool then
ends above N; run it again once they are free). N above the pool's configured
`slots` is refused before any machine is asked. `scale 0` empties the pool of
its free slots.

**`build_dir` is optional and has no default.** A pool that names none has no
memory disk and no pack or restore, and the job runner's clean names no
directory. That clean is `git clean -fd` (no `-x`) on every provider: it
removes untracked files the repository does not ignore and leaves gitignored
ones alone, so a plain `target/` survives between jobs whether or not
`build_dir` is set. When `build_dir` is set on a pool whose build directory
persists (ssh, local, runpod), the clean also names it, so it survives even
when the repository does not ignore it; on fly and docker it is a memory disk
packed at seed and restored before every job instead. Name one
(`build_dir = "target"`) to get the warm build described below.

**A pool can be defined with only its provider settings**, for testing: `repo`,
`github`, `harness` and `committer` are all optional (`github` is required
together with `repo`). Such a pool can be checked, listed, waited on,
reconciled, destroyed and live-tested, but not used for real work:
`create`, `scale` (when it would create) and `dispatch` fail with a message
naming the missing setting (`[pools.<name>] repo is required to create slots`).
`doctor` reports the Repository group as one `skipped` line ("no repository
configured for this pool") instead of the token, branch and protection checks;
`doctor --test` never needed the repository either way, because it brings its
own source tree, stand-in harness and committer.

**Slot identity.** A slot belongs to a mailspace **and** a pool: its marks
(Fly machine metadata, Docker labels, the mark files of an SSH or local slot) carry both
the mailspace's slug and the pool's name, and discovery, claiming, status,
destroy and the self-test all filter on both. Two mailspaces may each have a
pool called `local` on the same Docker daemon, SSH host or Fly app and never
see or touch each other's slots. Machine names are
`<mailspace>-<pool>-slot-<n>` (Fly machine name, Docker container name) and an
SSH or local slot is the directory `<root>/<mailspace>/<pool>/slot-<n>/`. The mailspace
slug is the mailspace name lowercased with every other character turned into a
dash (so `My Project` is `my-project`, and two mailspace names that slug the
same collide). The self-test pool is `<pool>-doctor` of the same mailspace.
The SSH and local `destroy` guards check the pool mark **and** the mailspace mark.

**Progress and `--porcelain`.** The default output is for a person. Progress
goes to standard error and results stay on standard output. `create`,
`dispatch`, `wait` and `doctor --test` print a line when each step starts, so
the last line on screen names what is running (the finish lines are unchanged),
and a long wait prints a heartbeat every 30 seconds: one short line with what
is being waited on and how long against the budget or limit, for example
`waiting  3f2a on p-slot-2  running 150s of 1800s budget`, or for `create`
`waiting  seeding p-slot-1 120s, p-slot-2 118s (gives up at 900s)`. A wait
shorter than 30 seconds prints no heartbeat. `doctor --test` says up front that
it takes about three minutes (about ten with `--long`). `--json` keeps its
meaning (full detail, silent until the end).

**An orchestrating agent passes `--porcelain` to every pool command it runs**,
above all to a `wait` it runs in the background: the output of a background
command is read back into the agent's context, and progress lines there are
wasted context. `--porcelain` prints no start lines, no heartbeats and no
decoration: only result lines on standard output, one per result, fixed fields
separated by single spaces, in the order below. An absent field is `-`. A value
that is empty, holds whitespace, a quote or a backslash, or is itself `-` is
double-quoted with `\"`, `\\`, `\n`, `\r`, `\t` and `\u{hex}` escapes. Times are
epoch seconds. Exit codes are unchanged. `--porcelain` with `--json` is a usage
error. Warnings (for example missing harness environment) still go to standard
error.

Every line that is about a pool carries `POOL`, the pool's name: it is the
first field after the line kind, except on the lines that lead with `TASK`
(`dispatched`, `collected`, `reconciled`), where it follows `TASK`.

```
pool create     home POOL TEXT                                          only when the app was made
                created POOL NAME ID STATE SECONDS EXIT OUTCOME ERROR   OUTCOME: seeded|timeout|failed|unmarked
                exists POOL COUNT                                       nothing to create
                issue POOL KIND SUBJECT MESSAGE FIRST CLEARED           KIND: list-error, SUBJECT: -
pool status     slot POOL NAME ID STATE TASK RUNNING BUDGET BASE ARCH CREATED MODE   MODE: write|read-only, - with no open dispatch
pool scale      created POOL ...   (as create; also home, exists, issue)
                destroyed POOL NAME ID
                skipped POOL NAME ID STATE TASK                         a slot that was not free, so kept
                scaled POOL TARGET COUNT                                COUNT: slots the pool holds now
pool dispatch   dispatched TASK POOL SLOT JOB MODEL FROM START BRANCH BUDGET MODE ABANDONED COST
                                                                        BRANCH: - when read-only; MODE: write|read-only;
                                                                        ABANDONED: pods made and terminated before the one that works because they
                                                                        did not answer on SSH in time (always 0 off RunPod); COST: their approximate
                                                                        total charge in dollars (`0.12`), `-` when none or no price was known
pool wait       collected TASK POOL SLOT STATE DETAIL                   STATE: finished|timeout|lost
                nothing-open                                            already collected
                issue POOL KIND SUBJECT MESSAGE FIRST CLEARED           KIND: poll-error, SUBJECT: the task
pool reconcile  reconciled TASK POOL SLOT STATE DETAIL                  STATE: running|finished|timeout|lost|error|stale-claim|orphan-stopped
                nothing-open                                            no dispatch was open
pool doctor     check POOL STATUS GROUP NAME DETAIL FIX MILLIS          STATUS: ok|warn|fail|skipped
                summary POOL OK WARN FAIL SKIPPED                       one per pool
```

**What changed from the formats of 1c9931e (one pool per mailspace):** every
line gained `POOL`, and nothing else moved. `home`, `created`, `slot`, `issue`,
`check` and `summary` have it as the first field after the line kind
(`created p-slot-1 ...` is now `created POOL ms-p-slot-1 ...`; `summary 11 1 0 2`
is now `summary POOL 11 1 0 2`); `dispatched`, `collected` and `reconciled` have
it right after `TASK`. `exists` already led with the pool name. `POOL` is `-` only
where it is not known (a dispatch recorded before pools were named, or an `issue`
not about one pool). `scale` adds `destroyed`, `skipped` and `scaled`. Machine
names in `NAME` and `SLOT` now include the mailspace: `<mailspace>-<pool>-slot-<n>`.
In `--json`, each slot of `status` and each check of `doctor` gained a `pool`
field. The human output names the pool on each slot or result line too.

Literal examples: `dispatched 3f2a local ms-local-slot-1 0123456789ab sonnet main <40-hex> work/x 1800 write`,
`dispatched 3f2a local ms-local-slot-1 0123456789ab sonnet main <40-hex> - 1800 read-only`,
`collected 3f2a lanes ms-lanes-slot-2 finished "pushed work/x 0123456789"`,
`collected 3f2a lanes ms-lanes-slot-2 finished read_only_unchanged`,
`collected 3f2a lanes ms-lanes-slot-2 finished "read_only_discarded 3"`,
`created local ms-local-slot-1 m1 stopped 42 0 seeded -`,
`slot lanes ms-lanes-slot-1 m1 free - - - <base> amd64 2026-10-11T10:00:00Z -`,
`scaled lanes 0 0`,
`issue lanes poll-error 3f2a "Fly unreachable" 1760000000 1760000090` (`-` as the last
field: the problem lasted to the end). `issue` lines are printed by `wait` and
`create` even when the command then fails, so a caller learns why a wait was
slow: the pool, the kind, the task, the message, when it was first seen and when it
cleared. In `status`, `RUNNING` and `BUDGET` are seconds, `BASE` is the full
seeded commit.

`doctor` checks each pool (all of them, or `--pool`) on three sides under their own headings (each heading names the pool): the provider (Fly: `fly`
installed and logged in, an API token, the app, the harness secrets; Docker: the
`docker` binary, the daemon, the image, daemon memory against a slot's limit,
the harness environment names; SSH: the `ssh` client, a non-interactive login,
a Linux host with the tools the slots need, the root's writability, free disk
space, the architecture, the harness environment names; local: the tools on
`PATH`, `ps`, the root's writability, free disk space, the source repository of
a worktree pool and its worktree count, the harness environment names; RunPod: `runpodctl`,
the key file's mode, that the API accepts the key, the account's SSH keys, each GPU type against
the live market, the pool's pods and stray pods), the
repository (GitHub: `openssl`, the App key, a token, the default branch and its
protection; one `skipped` line for a pool with no repository), and the pool itself (config, slot count, each slot's state and how
far its seeded base trails the default branch, leaked runners, orphaned
dispatches, stale claims, overdue slots). It changes nothing, prints a `fix:`
line under every `warn` or `fail`, and exits nonzero when any check fails.
Run it at orchestrator boot and whenever something looks stuck.

`doctor --test` adds a live exercise that answers two questions apart: is the
provider working, and is Vivi's own pool machinery working? It first runs the
read-only checks above and stops if the provider checks fail (the repository
checks are shown but do not block it). Then it creates a throwaway pool named
`<pool>-doctor` (in the same mailspace) of two of the smallest machines in the
same provider application and runs stand-in jobs on it. Each step is reported `ok` or `fail`
with a duration under a Provider or a Vivi heading, and a failure says which
side it is on and what was expected and observed. Provider steps: create two
slots and see both reach `free`, a start and stop round trip. Vivi steps: two
dispatches take two different slots; a third is refused with "no free slot";
`wait` collects a job during the linger; a job past its budget is killed and
recorded; a second dispatch continues a pushed branch with a new commit on top;
a job that finishes while nobody waits is later collected from the stopped slot
and the slot ends stopped and free; `reconcile` finds and stops a running slot
with no dispatch. `--long` adds one `wait` that runs past six minutes (the
lifetime of the Fly API token) and still collects. The slots are always
destroyed at the end (also after a failed step or Ctrl-C), a final check
confirms nothing of the test pool remains, and if something does it names the
machines and the command that removes them.

The test is deliberately independent of the project. Its source is a tiny
tree carried inside the vivi binary; each slot turns it into a bare Git remote
on its own disk, and jobs fetch from and push to that remote. The stand-in
harness is a shell command that appends a line to a file. So the test needs no
repository, no GitHub App, no token, no model key and no agent harness, and it
does not touch the project's mailspace (its tasks live in a temporary one).
What it therefore does not cover: minting a repository token, fetching from and
pushing to the real repository, repository rules such as branch protection (the
plain checks look at those), and any harness or model. Run it after a change to
the pool code, before a campaign that depends on the pool, and when something
looks stuck. On Fly it creates billable machines for a few minutes (about three
with a healthy provider; `--long` adds about seven). On Docker it creates two
containers on your daemon and removes them (about a minute and a half with the
image present; `--long` adds about seven): it is free, so a local Docker daemon
is the check to run after any change to the pool code, whichever host the real
pool uses. On an SSH host it makes two slot directories under the pool's root
and removes them (about a minute; `--long` adds about seven); it needs no
privileges and costs nothing beyond that host's cores and disk. On `local` it
does the same on this machine, always in fetch mode (a worktree pool's test
brings its own embedded remote instead of touching your repository). On RunPod it would rent a real GPU pod, so it refuses unless `--allow-spend` is given: the refusal says it would rent a pod, the hourly price of the first GPU type when RunPod will say, and that nothing was rented; with the flag it rents one pod for a stand-in job and terminates it.

#### RunPod: short-lived GPU pods

`provider = "runpod"` rents one GPU pod per dispatch from RunPod, through the
`runpodctl` command line, and reaches it over SSH. It exists for jobs that need
a GPU you do not own. It differs from every other host in one way that shapes
everything below: **a pod bills by the second from the moment it exists, and a
stopped pod keeps nothing, so there are no resting slots.** `slots` is the most
pods that may exist at once, not a count of waiting machines.

```toml
[pools.gpu]
provider = "runpod"
slots = 2                       # at most 2 pods at once (a ceiling, not a pool size)
image = "runpod/pytorch:2.4.0-py3.11-cuda12.4.1-runtime-ubuntu22.04"   # needs git, tar, setsid, timeout, sha256sum, ps and bash
# repo, github, harness, committer, setup, seed, requires, facts as for the other hosts

[pools.gpu.runpod]
api_key_file = "~/.config/secrets/runpod.key"   # required; mode 600; Vivi never reads it
gpu_types = ["NVIDIA A100 80GB PCIe", "NVIDIA H100 PCIe"]   # required; best first, as `runpodctl gpu list` spells them
gpu_count = 1                   # GPUs per pod (1 to 8; default 1)
cloud = "secure"                # or "community"; default secure
container_disk_gb = 40          # default 40; the pod keeps nothing else
# min_cuda_version = "12.8"     # lowest CUDA the host's driver must support
# data_centers = ["EU-RO-1"]    # default: any
ready_timeout = 120             # seconds a new pod gets to answer on SSH (30 to 1800; default 120)
```

A different tier is a different pool: write a second `[pools.<name>]` with its
own `gpu_types` rather than looking for a switch. The GPU type a pod actually
got is recorded in the dispatch record and shown in `dispatch`'s output.

**`ready_timeout`: how long a new pod gets to answer on SSH.** A working pod
answers in well under a minute (35 to 45 seconds when measured). A pod that
RunPod accepts and lists as running but that never answers is not going to, and
it bills by the second until it is terminated. So Vivi waits `ready_timeout`
seconds (default 120) for each new pod; when that expires it **terminates the
pod, confirms it is gone from RunPod's list, and tries the next GPU type**.
When every type has been tried the dispatch fails and nothing is left behind.
`vivi pool dispatch --ready-timeout <secs>` sets it for one dispatch (same range,
30 to 1800); on a pool whose slots rest between jobs (Docker, Fly, SSH, local)
the flag is a usage error. This limit covers only the wait for the pod to
answer. Seeding after that has its own, separate limit of 30 minutes, counted
from the moment the pod answers.

Every pod given up on is reported. The progress output (not `--porcelain`) says,
per pod, its id, the GPU type, how long Vivi waited, and the **approximate**
charge (the pod's hourly price times that time; RunPod's bill is the authority).
The final result then records how many pods were abandoned before the one that
worked and their approximate total cost: in `dispatch`'s human line
(`abandoned N pod(s) that did not become reachable, approx $X`), in the dispatch
record and reply on the task, and in `--porcelain` as two trailing fields on the
`dispatched` line, `ABANDONED COST` (see the table above; existing readers that
take the first ten fields are unaffected). When a dispatch fails, the error
gives the same count and cost. If the CLI keeps the pod on a timeout, Vivi finds
it by the id in the CLI's error (checked: the pod must carry this dispatch's
name), or, when the error names none, by the nonce in its name.

**How the commands differ**

- `create` and scaling up do not apply and say so (nothing is made ahead of a
  job). `scale <N>` and `destroy` only remove: they terminate pods, and wait
  until RunPod's list no longer shows them. `status` shows
  `N pod(s) of at most M`.
- `dispatch` makes the pod, which takes minutes (renting, waiting for SSH,
  seeding), pins its SSH host key, runs the same seed as every other host,
  delivers the job, starts it and returns. It prints the pod, its hourly price
  and the time it was told to end by, and that **the pod bills until collected**.
- `wait` and `reconcile` collect the result as for other hosts, then
  **terminate the pod and confirm it is gone from RunPod's list** (up to three
  minutes; if it is still listed they say so and print the command that
  terminates it). A pod the provider no longer runs has lost its disk: the
  engine reads that as a lost job.
- A result is a pushed branch or a receipt, as everywhere in Vivi. Nothing else
  leaves a pod: there is no artifact, checkpoint or output-file collection
  today. A job that needs to keep a model or a dataset must push it somewhere
  itself (and the pod's image must let it).
- `doctor` is read-only (see below). `doctor --test` rents a pod, so it refuses
  without `--allow-spend`.

**What keeps it from billing unattended.** Every one of these exists; read the
limits next to each.

1. *The pod is told when to die.* `pod create` is given `--terminate-after`
   with a time of dispatch + the worst case of setup + the job's budget + 15
   minutes (the same silence `wait` allows before it calls a job lost). The
   worst case of setup is (number of GPU types x `ready_timeout`, or the
   dispatch's `--ready-timeout`) + the 30-minute seeding limit. **Do not rely on
   it**: on 2026-10-11 it fired on neither pod tested (a CPU pod 12 minutes
   past the time, a GPU pod rented by a Vivi dispatch 20 minutes past it); each
   ran until something terminated it. Collect every dispatch promptly.
2. *Collection terminates.* `wait` and `reconcile` end the pod and check the
   list. This needs Vivi to run: an uncollected pod costs its hourly rate until
   something does.
3. *A failed dispatch terminates its own pod* at whatever step failed (rent,
   host key, seed, claim, delivery). If even that fails, the error says the pod
   is still billing, when its provider-side deadline is, and the command to end
   it.
4. *`reconcile` finds leaks.* A pod of the pool that no dispatch record
   explains, past the longest a dispatch may spend setting up (number of GPU
   types x the pool's `ready_timeout` + 30 minutes seeding + 1 minute; 35
   minutes for two types at the default), is terminated. A dispatch that passes
   a `--ready-timeout` longer than the pool's is not covered by this grace.
5. *`doctor` reports leaks* with the money framing: pods of this pool with how
   long they have run and their hourly price; any past their deadline as a
   failure; and **stray** pods (a pod with Vivi's name prefix for a pool this
   mailspace no longer has, or a malformed name) as a failure, "billing until
   terminated", with the command that ends it. Pods of other mailspaces are
   counted and left alone.

`reconcile` and `destroy` act only on pods whose name says they belong to this
mailspace and pool. Nothing here terminates a pod it did not name.

**Identity and the claim, without labels or leases.** RunPod pods carry a name
and nothing else Vivi can use, so the name is the record:
`vivi-rp1-<mailspace>-<pool>-<mailspace length>-<slot index>-<task>-<claimed>-<deadline>-<nonce>`
(at most 191 characters, which the config check enforces; the task field is the
task handle, or a hash of it when the handle is not a plain word; the nonce is
12 random hex digits). A name that does not parse is not Vivi's and is never
touched. There is no lease, so a dispatch claims a slot like this:

1. On one machine, a file lock per mailspace and pool serialises dispatches.
2. It picks the lowest unused slot index among the pool's pods, and refuses if
   the pool already holds `slots` pods.
3. After the pod exists, it lists the pool's pods and orders them by RunPod's
   own creation time (ties by pod id). A pod loses, and is terminated, if an
   earlier pod holds its slot index, or if earlier pods already fill `slots`.
   A pod whose creation time cannot be read gives way.
4. The same check runs again just before the job starts.

Limits, stated plainly: the rule is correct when the provider lists both pods
by the second look, and two dispatchers on different machines have no lock
between them. If RunPod's list lags enough that neither pod sees the other at
either look, two pods can end up with one slot index or the pool can exceed
`slots` for a while; no two jobs ever share a pod, and `doctor` and `reconcile`
show and clear the excess. This was reasoned and tested against a simulated
race; it was not observed on the real service.

**SSH.** The pod's host key is captured once, at first contact: `ssh-keyscan`
twice with a pause, the two answers must match, and the keys are written to a
per-pod file under `.vivi/runpod/pins/` (mode 600). Every later call uses
`StrictHostKeyChecking=yes` against that file alone, so a changed key fails
closed. The account needs an SSH public key registered with RunPod (`doctor`
warns when there is none). Inside the pod the work is done by the same remote
script, boot script and job script as on the `ssh` host, run as root in the
container, with the agent as the unprivileged `hand` user.

**The API key.** `api_key_file` is required and must not be readable by group
or others (Vivi refuses it and says `chmod 600`). Vivi never reads the file:
each `runpodctl` call is started through a fixed one-line `sh` wrapper that
reads the file and passes the key to the CLI in its environment
(`RUNPOD_API_KEY`, which the CLI prefers to its own saved login). The key is
not an argument, not in Vivi's memory and not in any message of Vivi's;
`runpodctl`'s own error text is scrubbed of anything shaped like a key before
it is printed. The key never reaches the pod. Harness environment variables
(`harness.env`) are read from the environment of the `vivi` process at
dispatch and sent inside the pinned SSH connection to a file only root can read
in the pod; they do not pass through RunPod's API.

**`doctor`** (read-only) checks: `runpodctl` and its version; the key file and
its mode; that the API accepts the key (identity and balance only); the SSH
keys on the account; `ssh` and `ssh-keyscan`; each configured GPU type against
the live market (price and stock in the configured cloud and data centres); the
image name; the pods of this pool; stray pods. **`doctor --test` does not spend
by default.** It refuses, says it would rent a pod, quotes the hourly price of
the first GPU type when RunPod will say, and says `--allow-spend` is needed.
With `--allow-spend` it rents one pod for a stand-in job and terminates it. The
flag does nothing without `--test`.

**What has and has not been verified live.** Verified against a real, empty
RunPod account, reading only: the shape of `runpodctl user`, `gpu list`,
`datacenter list`, `ssh list-keys` and `pod list` output; that
`RUNPOD_API_KEY` overrides the CLI's saved login; `vivi pool doctor` and
`vivi pool status`, and the `doctor --test` refusal. One
`doctor --test --allow-spend` run on 2026-10-11 (one pod, secure cloud, the
first configured GPU type available, a public image known to publish SSH)
then verified, end to end in 51 seconds: the create call, the pod name being
accepted, the SSH host key capture, the root-mode boot and job scripts on a
RunPod image, collection, the termination and the absence check. Two dispatches the same day
verified the fallback to a second GPU type (the first pod never became
reachable), and a read-only dispatch against a real repository: token, fetch,
`facts` recorded from the rented card, and `reconcile` collecting the finished
job 55 minutes later and terminating the pod. `--terminate-after` was checked
live and did not fire. Still
**unverified live**: the failure text for no capacity, the
`ready_timeout` path (a pod that never answers, the id in the CLI's timeout
error, the termination and absence check that follow, the approximate cost it
reports: **this code path is verified against a fake account only**), the
creation-time ordering and list lag the claim rule depends on, the state words
of a stopped or terminating pod, a write dispatch that pushes a branch, and the
harness-environment path. Those are covered by tests against a fake account
and a fake pod only. The self-test does not run a pool's `requires` or
`facts`.

#### Requirements and facts (special hardware)

Two optional lists in the pool table, both plain shell commands, one line each.
Nothing in them is known to Vivi: they are for a pool whose machines have
something particular (a GPU, a toolchain, a device) and whose results are only
evidence if they say what produced them.

```toml
[pools.gpu]
provider = "ssh"
slots = 1                       # one slot per device, so two jobs never share it
requires = ["command -v gpu-query", "gpu-query --list"]
facts    = ["gpu-query --name --driver --memory --csv", "uname -srm"]
# [pools.gpu.ssh] host = "gpu-box" ... repo, github, harness, committer as usual
```

- `requires`: each command must exit 0 on a slot for the slot to be usable.
  They run during the seed, in the checkout, after `setup` and before `seed`,
  as the user the worker will run as (`hand` on Fly and Docker, the login user
  on SSH and local), each under a two-minute limit. If one fails, the seed
  fails: `create` and `scale` print `exit 7 (a `requires` command failed)` and,
  where the host can read a stopped slot's disk (SSH, local, Docker), the
  failing command, its exit code and the last line of its output; the slot
  stays `unseeded`. `vivi pool doctor` reports, per seeded slot, that the
  requirements passed at seed time (from a mark the boot script wrote, listing
  what it checked), or, when `requires` was configured or changed after the slot
  was seeded, `not checked on this slot; re-create it`. Doctor only reads: it
  never starts a slot to check, so a running slot, or a host that cannot read a
  stopped disk (Fly), is reported as not read. `doctor --test` ignores both
  lists.
- `facts`: each command runs before the harness starts, in the checkout, as the
  worker's user, with a clean environment, a 10-second limit and an output cap
  (the first 40 lines and 4096 bytes; the command may write at most 8 MB per
  stream). Every job gets them, normal or `--read-only`, on every provider.
  Each is recorded with its command, its exit code and its standard output
  (its standard error when it failed, or its standard output if that is empty).
  A fact that fails or times out (shown as `timed out`) is recorded as such and
  never fails the job. The facts are in the task's result reply under a
  `--- facts` heading before the receipt, and in the `pool_collected` JSON as a
  `facts` array of `{command, exit, output}`. They are not in porcelain lines.

The commands are configuration, so they are trusted; their output is treated
as data (control characters dropped, escaped in the result file, only ever
shown or stored). A requirement is checked once, at seed time, and a fact is
evidence of the moment the job started; neither selects or schedules slots.

#### Read-only dispatch

```
vivi pool dispatch --pool <name> <task> --model <slug> --read-only [--from <ref>] [--budget <secs>] [--porcelain]
```

A read-only job is for a reviewer or auditor that must not change the
repository but is useful only with a full shell (Git, builds, tests). Instead
of withholding the shell, the pool makes sure nothing the job does can leave
the slot. `--from` (or a `from:` line in the task body) says what to check out;
`--branch` and `--allow-default-branch` are refused together with `--read-only`
(a `branch:` line in the body is ignored, and the human output says so). It is a
per-dispatch flag only; there is no config setting.

What Vivi does for a read-only job:

- **The token cannot push.** For a GitHub remote the installation token is
  minted with `contents: read` instead of `contents: write`. The slot holds
  nothing that can push; GitHub refuses a write with that token.
- **The runner publishes nothing.** It checks `from` out detached (no branch is
  made or moved), runs the harness exactly as for any job, and then never
  commits and never pushes. If the tree differs from `from` afterwards (tracked
  changes, commits the agent made, untracked files; the build directory is not
  counted), it records the changed paths and puts the tree back to `from`, so
  the slot is clean for the next job. Changes are never a failure of the job,
  but they are always visible.
- **Two outcomes**, not shared with the write outcomes (`pushed`, `committed`,
  `no_changes`, `push_refused`):
  `read_only_unchanged` (the tree was as checked out) and `read_only_discarded`
  (changes were discarded; the count is in `discarded_count`). The result on the
  task shows the outcome, the discarded paths (at most 50 are listed; the count
  is exact) and the receipt. In the JSON note of the `pool_collected` event the
  paths are the array `discarded`, next to `discarded_count` and
  `"read_only": true`. The `pool_dispatched` record carries `"read_only": true`
  with an empty `branch`, and `status` shows `read-only` for a busy slot
  (`--json`: `read_only`).
- `--porcelain`: `dispatched ... BRANCH MODE` has `-` for BRANCH and `read-only`
  for MODE (`write` otherwise); `collected ... finished read_only_unchanged` or
  `collected ... finished "read_only_discarded COUNT"`; `slot ... CREATED MODE`.

What it guarantees depends on the provider:

| Provider | Guarantee |
| --- | --- |
| Docker, Fly, RunPod | The strong form (RunPod: by design, from the same scripts; not verified live). The worker is confined and holds no credentials: no push token, no keys, and the changes cannot leave the slot. A review role can have a full shell, even edit files, safely. |
| SSH, local (fetch mode) | The runner publishes nothing and the token cannot push, but the worker runs as a real user account that may have its own credentials, and Vivi cannot stop it from using that account's access. `dispatch --read-only` prints a one-line warning to standard error saying so (never in `--porcelain` result lines). |
| local (worktree mode) | No token and no push exist; the job starts detached at `from`, makes no commit and no branch, and its changes are reported and discarded. The runner never moves or creates a branch. But the worker shares your repository, so it could still commit to it with plain Git; the dispatch warns of that. |

Hard time limit, clean harness environment, collection, `wait` and `reconcile`
are unchanged. Not covered: roles, permission profiles, network restrictions,
any sandboxing beyond what the provider gives.

A slot can only fetch what the remote repository already has, so push the base
before dispatching. `--model` is resolved by the caller in the harness's own
vocabulary; Vivi only checks it against the allowed list. The usual order:
`doctor`, push the base, `dispatch`, `wait` in the background, read the receipt
with `task show`, integrate the pushed branch, delete the remote branch, close
the task. Neither `wait` nor `reconcile` closes the task.

A slot is `free`, `seeding`, `busy` (a task holds it and the machine runs),
`finished-uncollected` (a task holds it and the machine stopped, its result
still on disk), or `unseeded` (the seed failed or `create` was interrupted).
Only a `free` slot is claimed. `status` shows how long a busy slot's dispatch
has run against its budget (`running 95s of 1800s`). `dispatch` takes the task body as the brief,
claims a slot atomically (a Fly machine lease serialises the claim and machine
metadata is the durable mark; on Docker, creating a claim container named after
the slot is the claim, because the daemon refuses a second container with the
same name, and its labels are the mark; on SSH, a hard link of a complete file
inside the slot directory, which fails if the claim exists; the same on local), delivers the job as
one archive, starts the slot, records the dispatch on the task, and returns without waiting. The slot
runs the configured harness as an unprivileged user under a hard time limit,
commits what changed as the configured bot identity, pushes without force, and
leaves a result. `wait` and `reconcile` collect it, record the result and the
agent's receipt on the task (a `pool_collected` event plus a reply in the
task's thread), and free the slot. Neither ever closes the task.

`dispatch` refuses the repository's default branch as the push target unless
`--allow-default-branch` is passed. `wait` and `reconcile` print a provider
error to stderr as soon as a poll fails (and say when polling recovers); an
error is never reported as "still running". After a collect the slot is
stopped and confirmed `stopped` before it is freed, and `reconcile` also stops
a running slot that no task holds.

Slots are discovered from Fly machine metadata (`vivi_pool*`), Docker
container labels, marks and claim containers, or the marks in each slot
directory on an SSH host or on this machine; Vivi keeps no local slot records.
The Fly app is **required** for a Fly pool (`[pools.<name>.fly] app`) and has no
default: Fly app names are unique across all Fly accounts and a pool name is a
short word that would collide. If Fly says the name is taken, `create` says so
and the fix is to pick another. Several pools, and several mailspaces, may share
one app (the machine names and marks keep them apart).
`create` makes the app in `[pools.<name>.fly].organization` (default
`personal`) when it does not exist. A job's Git address is `https://github.com/<repo>.git`
for `repo = "owner/name"`; slots take it as a full remote address.
Model keys are app secrets on Fly: `create` and `dispatch` list the app's
secret names, never values, and warn about harness `env` names without one
(vivi does not set secrets). On Docker the `env` names are copied from the
environment of the `vivi` process when a slot is created; `create` and `doctor`
name the ones that are unset. On SSH the `env` names are read from the login
environment of the remote user when a job runs. Configure each pool with a
`[pools.<name>]` table in `.vivi/mailspace.toml` (provider, slots, image,
setup/seed commands, optionally `requires` and `facts`, `build_dir`, and optionally `repo`),
`[pools.<name>.fly]` (app, which is required, and optional organization, region,
size, memory, tmpfs), `[pools.<name>.docker]` (optional host, memory, cpus,
tmpfs), `[pools.<name>.ssh]` (host, optional root) or `[pools.<name>.local]`
(optional root, optional `worktree_of`),
`[pools.<name>.github]` (App id and key path; with `repo`),
`[pools.<name>.harness]` with `run`, `models`, `env` and `rules`, and
`[pools.<name>.committer]`. See `skills/vivi/SKILL.md` for the full table. Nothing in
the pool is specific to a language, a repository, or an agent harness. The
GitHub App key is read by `openssl` when minting a one-hour, single-repo token;
Vivi never stores or prints it, and the token is not in the agent's
environment.

#### Docker provider

```toml
[pools.local]
provider = "docker"
image = "ubuntu:24.04"          # the daemon pulls it when missing
# ...repo, slots, setup, seed, build_dir as for Fly

[pools.local.docker]
# host = "ssh://user@server"    # a remote daemon (DOCKER_HOST for the docker CLI); omit for the local one
# memory = "4g"                 # per slot, including its memory disk
# cpus = 4
# tmpfs = "2g"                  # memory disk over build_dir
```

A slot is a container: stopped, it is a slot at rest and its writable layer is
the persistent disk; its main process is the boot script. The build directory's
memory disk is a tmpfs mount declared at creation (the script cannot mount
without privileges, and containers are never privileged), so the boot script
skips its own mount when `BUILD_MOUNTED` is set. Container labels cannot change,
so they hold fixed facts only; the seed mark and the architecture are small
files on the slot's disk read and written with `docker cp`, and the claim (task
and claim time) is a separate claim container, `vivi-claim-<slot id>`: the
daemon refuses a second container with a name in use, so two dispatchers,
including separate `vivi` processes sharing one daemon, cannot both claim a
slot. The claim has no expiry: a dispatcher that dies mid-claim leaves a claim
that `vivi pool reconcile` frees as stale after five minutes. A local lock file
would not work for a daemon shared by two machines, which is why the daemon
itself decides. `cargo test --test pool_docker_live -- --ignored` proves it
against a real daemon.

What differs from Fly: no per-second billing, but all slots share the daemon's
cores and memory (`doctor` warns when a slot's `memory` exceeds the daemon's);
the values of the `[pools.<name>.harness].env` names are read from the environment of
the `vivi` process when a slot is created and then live in the container's
configuration on that daemon (anyone with access to the daemon can read them
with `docker inspect`; names only appear in config, and no value is printed or
written to the mailspace); the daemon may be arm64 or amd64 (each slot records
its architecture, shown by `status` and `doctor`), so `setup` commands must not
hard-code an architecture; two mailspaces with the same pool name on one daemon do
not see each other's slots (the labels carry both names; containers are named
`<mailspace>-<pool>-slot-<n>`). With `git`, `zstd` and `curl`
already in the image, the pool and `doctor --test` run offline.

#### SSH provider

A minimal supported form, not the recommended one: a host reachable by SSH where
each slot is a subdirectory and the agent runs as the login user, with whatever
tools that host already has.

```toml
[pools.box]
provider = "ssh"                # no `image`: setting one is an error
# ...repo, slots, setup, seed, build_dir as for Fly

[pools.box.ssh]
host = "user@server"            # anything `ssh` accepts, including a Host alias
# root = "~/vivi-pool"          # default; each slot is <root>/<mailspace>/<pool>/slot-<n>/
```

What the host needs: SSH that works without a prompt (`BatchMode=yes`; a key in
an agent, a known host key), a POSIX login shell, Linux (`/proc`), and `bash`,
`git`, `tar`, `setsid`, `timeout`, `sha256sum` and `ps` from the base system.
Nothing is installed: a missing tool is a `doctor` failure with a hint, not
something a slot script fixes. `zstd` and `curl` are not needed. `doctor`
reports the architecture, free disk space (it warns under 5 GiB) and whether
the root can be created.

What it deliberately lacks. **There is no isolation**: the slot's scripts and the
agent run as the SSH login user and can read and write whatever that user can,
start processes, and use that user's credentials and network. There is no image,
so the host's tools are what a job sees. Slots share the host's cores, memory
and disk with each other and with everything else on it. `setup` commands
run once per slot as the login user, not as root, so they cannot install
packages. The Git token of a job is removed from the slot as soon as the runner
has read it, but an agent running as the same user is not kept from anything
else that user owns. Use Docker or Fly when any of that matters.

How it maps. A slot is `<root>/<mailspace>/<pool>/slot-<n>/`; it holds the checkout, the
build directory, the job directory, the boot log and the marks (one small file
per mark, in `marks/`). The build directory is a plain directory that persists:
there is no memory disk and nothing to pack or restore, so a seeded slot keeps
its warm build between jobs (the job runner's clean spares it even if the
repository does not ignore it). Starting launches the boot script detached
(`setsid`, a pid file, no systemd) so it outlives the SSH session; a slot is
running while that process is alive and carries the slot's tag in its
environment, and stopping ends everything that carries the tag, so nothing a
job started survives a stop (a process that scrubs its own environment can
escape this; the hard time limit and the job's own process group still apply).
The claim is the hard link of a complete file named `claim`, which fails if it
exists, so two `vivi` processes on two machines cannot both claim a slot; it
carries the task and the claim time. A job is one archive copied over SSH into
the slot; results are read as files over SSH whether or not the slot runs.
Calls share one SSH connection through a private control socket in the
temporary directory, removed when `vivi` exits.

`destroy` deletes a slot directory, so it is guarded on the host: the id must be
`<pool>/slot-<n>`, the root must be a directory of its own (never `/`, empty, or
the home directory), the slot must be a plain directory (not a link) that really
lies inside the root, and it must hold Vivi's own mark naming the pool. Anything
else is refused with a message and nothing is deleted. A root that Vivi created
is removed when its last slot goes; a root that was already there is left.

The values of the `[pools.<name>.harness].env` names never pass through Vivi or the
mailspace: the job reads them from the remote user's login environment when it
runs. Put `export NAME=...` in `~/.profile` on the host (a non-interactive login
does not read `~/.bashrc`); `doctor`, `create` and `dispatch` name the ones the
host lacks. `cargo test --test pool_ssh_live -- --ignored` (with
`VIVI_SSH_TEST_HOST`) proves the process handling and the claim against a real
host.

#### Local provider

The machine `vivi` runs on is the host: each slot is a directory under a root,
and the worker is a process Vivi launches itself, detached, as the current
user. No rented machine, no container, no `ssh`.

```toml
[pools.here]
provider = "local"              # no `image`: setting one is an error
slots = 4
# build_dir = "target"          # optional, no default
# ...repo + github, or worktree_of; setup, seed, harness, committer as for Fly

[pools.here.local]
# root = "~/vivi-pool"          # default; each slot is <root>/<mailspace>/<pool>/slot-<n>/
# worktree_of = "~/work/repo"   # optional: slots are Git worktrees of this local repository
```

**There is no isolation.** The boot script, the `setup` and `seed` commands and
the worker all run as the user who runs `vivi`, with that user's whole account:
every file it can read or write, every credential in its environment and home
directory, its network. Slots share this machine's cores, memory and disk with
each other and with everything else on it. `setup` commands run as you, not as
root, so they install nothing system-wide. Use it for work you would run in a
terminal yourself; use Docker or Fly when that is not enough.

**Two ways to get the code.**

- *Fetch mode* (no `worktree_of`): a slot checks out its own clone, fetched as
  an SSH slot's is, from `repo` (with `[github]`, a one-hour token) or, for
  `doctor --test`, from the tiny embedded remote. A job ends in `pushed`.
- *Worktree mode* (`worktree_of` set): a slot's checkout is a Git worktree of a
  local repository, made with `git worktree add --detach` at the seeded base.
  No `repo`, no GitHub App, no token, no network; setting both `worktree_of` and
  `repo` is an error. A job ends in a commit on `branch` in that repository,
  reported as `committed <branch> <sha>` instead of `pushed`. Nothing is pushed
  anywhere; the repository's own remotes are not touched.

**Branch rules in worktree mode.** Nothing is ever forced: no `-B`, `--force`,
or `branch -f`, and an existing branch is never moved or reset. The decision is
made at dispatch, before a slot is claimed, so a refusal costs nothing:

| `branch` | `from` | What happens |
| --- | --- | --- |
| does not exist | a commit | created at that commit |
| does not exist | another branch | created at the tip of that branch (a stack) |
| exists | the same branch | continued: checked out as it is, the new commit goes on top |
| exists | anything else | **refused**: reset or rebase it yourself, or continue it with `--from <branch>` |
| checked out in another worktree | any | **refused**, naming the path that holds it (Git allows one worktree per branch) |
| the primary checkout's branch | any | **refused** |
| the repository's default branch | any | **refused** unless `--allow-default-branch` |

A slot at rest is always on a **detached HEAD**, so it never holds a branch: it
goes back to detached after a finished job, after a job the hard time limit
killed, and when the slot is stopped or freed. A new branch is created only
when the job has something to commit, so a job with `no_changes` leaves no
branch behind. The slot is cleaned back to `from` before the worker starts,
sparing the build directory. Only `git worktree add`, `remove` and `prune` (and
read-only queries) touch the source repository, and the first three are
serialised per repository by a lock on its Git directory, which also holds
between separate `vivi` processes. `destroy` runs `git worktree remove`, then
removes the slot directory, and never deletes a branch; the commits of a
finished job stay in the repository.

**What the orchestrator does differently in worktree mode.** There is no base
to push first: `create` (or `scale` up) seeds each new worktree at `--base`, by
default the tip of the repository's default branch, so make sure that commit is
where you want it. There is no remote branch to delete afterwards: the result
is already a local branch, so integrate it (merge or cherry-pick) and, if you
like, delete it yourself; Vivi never deletes a branch. The sequence is
`doctor`, `dispatch`, `wait` in the background, read the receipt with
`task show`, integrate `committed <branch> <sha>`, close the task. A branch a
slot holds cannot be dispatched again to another slot, so run one job per
branch at a time.

**A fixed pool bounds build output.** Each slot keeps one persistent build
directory (a plain directory; nothing is packed or restored), so a pool of N
slots holds at most N build directories however many jobs run. The job
runner's clean spares it even when the repository does not ignore it. A pool
with no `build_dir` runs the same `git clean -fd` without `-x`, so gitignored
output such as `target/` is left alone; it is simply one more
untracked-but-ignored directory per slot, so the bound of N slots and N build
directories still holds for it.

**Processes.** Starting runs the boot script detached, in a process group of
its own, so it outlives `vivi`; a slot is running while that process is alive
(told by process id and start time, so a reused id is never mistaken for it).
Stopping ends everything the slot started: the group, the descendants (from one
`ps` listing) and, on Linux, any process whose environment carries the slot's
tag. The slot's processes are frozen (`STOP`) until no new one appears, then
ended with `TERM` and `KILL`, so nothing is born after the listing. This works
on macOS and Linux with `bash` and `ps` only (no `setsid`, no `/proc` on
macOS, and the tools macOS lacks, `timeout`, `sha256sum` and `nproc`, are
small stand-ins the slot carries). The limit: on macOS a process that
deliberately leaves both the group and the process tree (a double fork) is not
found; the hard time limit and the runner's own group still apply.

**State.** The marks are small files in the slot directory, the claim is the
hard link of a complete file (atomic across processes), and everything Vivi
knows comes from the directories; there are no other records. The values of the
`[pools.<name>.harness].env` names are read from the environment of the `vivi`
process at job time; `doctor` names the unset ones, and no value is stored.

`destroy` is guarded like SSH's: the id must be `<pool>/slot-<n>`, the root must
be a directory of its own, the slot must be a plain directory inside the root
(never a link) and must carry Vivi's own mark naming the mailspace and pool.
`cargo test` covers the process handling, the claim and the branch rules on the
platform it runs on; the Linux-only tag scan is gated to Linux, not skipped.

### Blocking on local mailspace changes

Use project-local watch when an agent has handed off work and should wait for a
turn-end message or a board lifecycle change before running one more cycle:

```sh
# Mind: file work, then block for one matching reply
vivi mailspace watch --for mind --kinds mail --match-subject-prefix "turn end:" \
  --timeout 2m --json

# Or wait for one task completion, then invoke the next cycle
vivi task watch --for mind --events moved --statuses done \
  --match-from hunter-2 --timeout 2m --json
```

`mailspace watch` polls the project-local `.vivi/mail.sqlite` event ledger and
supports caller-owned event-id cursor files with `--cursor-file
<path> --write-cursor`. `--once` performs one non-blocking scan. The aliases
`mail watch`, `task watch`, `need watch`, and `want watch` each watch one
kind and do not take `--kinds`. Use `vivi mailspace watch --kinds` to mix
kinds. This is deliberately different from `vivi-mail`'s `sync-events --watch`
and account-scoped `watch-inbox`, which observe inbound IMAP activity and
emit stable JSON events after local sync. `watch-inbox` never wakes an LLM or
executes outbound work; the Ops bridge owns wake delivery and debounce.

For long local bodies, keep using `--body @path` or pass an explicit body file.
`--body -` reads stdin:

```sh
vivi task send --from ceo --to cto --subject "Review evidence" --body-file evidence.md
printf "Long residual evidence\n" | vivi need send --from cto --to ceo --subject "Residual" --body -
```

Use dumps for audits or export. Work dumps default to open tasks or needs;
include `--status all` only when you intentionally want done history.
Stdout dumps over 25 records or 64 KiB refuse unless you pass
`--confirm-large` (or write the result with `--output <path>`):

```sh
vivi mail dump --participant cto --since 48h --output audit-mail-cto.md
vivi mail dump --participant mind --since 2026-07-14T03:44:00 \
  --status unabsorbed --json
vivi task dump --participant cto --body blocker --json
vivi need dump --participant ceo --status all --json --output audit-needs.json
vivi task dump --for cto --status all --confirm-large
vivi want dump --from ceo --status all --json
vivi want list --for ceo --json
vivi want list --from ceo --status all
```

Mailspace actions performed through Vivi are recorded in a local event ledger.
For example, local sends record sent-copy and delivery events, and task
completion/reopen commands record folder moves with optional `--note` text.
Dump output includes those events so a board review can distinguish current
state from command history.

Recovered mailspaces can be imported into an active project mailspace. Start with
`--dry-run` to see the message, blob, event, link, and conflict counts before
writing anything:

```sh
vivi mailspace import --project /path/to/current/project \
  --from /path/to/recovered/project --dry-run

vivi mailspace import --project /path/to/current/project \
  --from /path/to/recovered/project/.vivi
```

An import is one transaction. When the two mailspaces hold different messages
for the same project item (two checkouts of one project that both synced the
same letter under the same role name), it stops with an `import conflict`
message and changes nothing; `--dry-run` finds the same conflict and exits 1.

## Storage Layout

Everything Vivi owns lives under the project's `.vivi/` directory:

```
myproject/.vivi/
├── mailspace.toml         # name, description, identities, archive, probes, judgment, sync
├── mail.sqlite            # items, events, goals, local links, and the work graphs
├── blobs/                 # content-addressed message payloads
├── chat/                  # the sync identity key; named for the retired `vivi chat`, kept so registered names stay valid
├── sync/                  # sync cursor, floor, and lock; the directory gitignores itself
└── judgment-corpus.jsonl  # shadow-screen answers, appended by `vivi step --apply`
```

Rules:

- `mail.sqlite` is the store; there is no separate index to rebuild
- Message handles are the first eight characters of the id. A synced letter
  keeps that id, and so that handle, on every checkout that pulls it. Any other
  item's id is store-local, and its handle is stable inside that mailspace
- Letters filed by `vivi sync pull` are a replica of the project's history.
  `sync prune` can drop old ones, and `sync resync` rebuilds them
- Deleting `.vivi/` deletes that project's coordination state, its local mail,
  and the replica of synced letters

## Commands

`vivi --help` is the live top-level list. Today that is: `board`, `boot`,
`sync`, `mcp`, `mailspace`, `mail`, `task`, `need`, `want`, `memo`, `rule`,
`goal`, `pool`, `role`, `cycle`, `trace`, `graph`, `skills`, `step`.

`pool` is the remote lane pool (create, status, destroy, scale, dispatch, wait,
reconcile, doctor). `rule` holds project rules every seat can read; `--local`
marks a rule that a later sync should skip, and nothing syncs rules yet.
`skills` lists, shows, and installs the agent skills embedded in this binary
(`vivi`, `tugboat`, `speedboat`); it does not need a mailspace.

A worker's `done` is a report. Only the item's author, or a role of kind
`mind`, accepts it; the acceptance completes the work, unlocks its dependents,
and travels to every checkout as an event letter. `vivi step` lists reported
work as `awaiting_acceptance`, and `vivi graph ready` has a `reported` line.

`sync` replicates a mailspace with a project on a server, the sibling
repository `vivi-server`: `vivi sync` pushes local letters and pulls the
others'. Identity is a transparent per-mailspace ED25519 key. A pull reads pages of 500 and saves its
cursor per page; `sync pull --since 7d` starts a new mailspace at a time and
records where its history starts (`sync status` prints `history from: ...`);
`sync resync` replays everything; `sync fetch <handle>` and
`sync pull --refs [--depth N]` bring in older letters by handle or by
reference; `sync prune --older-than 30d [--dry-run]` evicts old letters
locally, and any of the three restores them byte for byte. To attach a
checkout (new or existing) to a server project, see **Linking a mailspace
to a server project** in the
[vivi skill](skills/vivi/SKILL.md#project-sync).

### `vivi mcp` and project API keys

`vivi mcp` is a thin stdio bridge to the project server's MCP endpoint
(`POST /v1/projects/{id}/mcp`): it speaks MCP on stdin/stdout
(newline-delimited JSON-RPC 2.0, the standard stdio transport) and forwards
each message to that endpoint unchanged. It is a pass-through — it does not
know the tool list, so new server tools need no new bridge — and writes
nothing but protocol messages to stdout (diagnostics go to stderr; a
notification gets no reply). A machine needs only the binary, a server URL, a
project id, and a credential: no local mailspace, no sync, and the bridge
never creates `.vivi/`. When the server refuses a request after the one
allowed renewal (or at once for an API key), the bridge answers that request
with a JSON-RPC error (code `-32000`, `data.http_status`) instead of hanging,
and keeps serving. The server's Project MCP tools are `overview`, `list`,
`show`, `thread`, `send`, `reply`, `done`, `reopen`, `memo`, `graph`; writes
share the server's 30-per-minute-per-identity send budget, and a tool failure
is a result with `isError: true`.

Credentials are either a member name plus key file
(`--name`/`VIVI_MCP_NAME` plus `--key-file`/`VIVI_MCP_KEY_FILE`; the 32-byte
seed file a mailspace keeps at `.vivi/chat/identity.ed25519`; the bridge mints
the identity token as `vivi sync` does and renews it once when the server
refuses it (401) and ahead of time when `token_expires_at` is within 30
seconds) or a project API key in `VIVI_MCP_API_KEY` (sent as the bearer as it
is; no minting, no renewal, a 401 is final). Giving both is an error. A flag
beats its variable (`--server <URL>`, `--project-id <ID>`, `--name <NAME>`,
`--key-file <PATH>`; `VIVI_MCP_SERVER`, `VIVI_MCP_PROJECT_ID`, `VIVI_MCP_NAME`,
`VIVI_MCP_KEY_FILE`), a variable beats the `[sync]` table, and the API key is
read from the environment only (no flag), so it does not show in a process
list. The global `--project <ROOT>` still names a mailspace root (used only to
find a `[sync]` table); it is not the server project id. Inside a mailspace
that has a `[sync]` table, `vivi mcp` with no flags uses its server, project,
name, and own key.

Example client configuration for Claude Code's `--mcp-config` (a JSON file):

```json
{"mcpServers":{"vivi":{"command":"vivi","args":["mcp"],"env":{"VIVI_MCP_SERVER":"https://<server>","VIVI_MCP_PROJECT_ID":"<project id>","VIVI_MCP_API_KEY":"<key from vivi sync api-key mint>"}}}}
```

The member-name variant replaces the API key with
`"args":["mcp","--name","<name>","--key-file","<path>"]`. pi connects to
stdio MCP servers through its `mcp.json`; the same `command`, `args`, and
`env` work there: `pi mcp add vivi --env VIVI_MCP_SERVER='https://<server>'
--env VIVI_MCP_PROJECT_ID='<project id>' --env
VIVI_MCP_API_KEY='<key from vivi sync api-key mint>' -- vivi mcp`.

Project API keys: `vivi sync api-key mint --label <LABEL>` prints `key_id`,
`label`, and the raw `key` once (Vivi never stores the key; label 1 to 64
characters); `vivi sync api-key list` shows `key_id`, `label`, `created_by`,
`created_at`, never the secret; `vivi sync api-key revoke <KEY_ID>` retires a
key. They run from a mailspace with a `[sync]` table and use that mailspace's
identity (the server accepts only an identity token for these routes, never
another key). At most 32 live keys per project, and a live key works on every
project bearer route, including the letter wire.

Not built here: how a pool slot is handed its credential, and how the pool
harness is told to load the bridge (tracked as need `8e39ed90` on the
operator's board). The bridge has been run against a staging server only; no
production use.

Almost everything Vivi does is project-scoped, so `--project <root>` is accepted
either before or after the subcommand and those commands resolve the same
mailspace. `vivi skills` is the exception: it reads the embedded catalog.
The mailspace section above is the command reference; the most common entry
points are:

```
vivi mailspace init --project .              # create .vivi/
vivi mailspace status --project . --json     # is there a mailspace, and what version wrote it
vivi boot --project .                        # one-read project frame
vivi board --project . --json                # actionable work as JSON
vivi graph ready --project .                 # dispatchable frontier
vivi step --project . --json                 # dispatch/exception manifest
vivi step --apply <settled-handle> --project .  # complete a settled item's graph node
```

## Security

- Vivi stores no credentials. Accounts, transport, and the email archive live in
  `vivi-mail`, which owns `~/.vivarium/`.
- A mailspace may name a `key_cmd` for the judgment provider. It is run through
  `sh -c` only when `vivi step --apply` consults the provider, and the resulting
  key is never written to config or to the process environment.
- Mailspace items are project-local files, so they are exactly as private as the
  repository that holds them. Do not commit a `.vivi/` directory that carries
  work you would not publish.

## Architecture

- **The project directory is the store.** Coordination state lives in
  `<project>/.vivi/`, next to the code it describes.
- **Items are messages with lifecycles.** Tasks, needs, wants, mail, and memos
  share one store and one set of folder semantics; lifecycle moves are the API.
- **The graph is derived from items.** Every `task` / `need` / `want` send mints
  a node, dependencies become edges, and completions unlock dependents.
- **Vivi decides readiness; the Mind dispatches.** Vivi never launches an agent.
- **Reads never mutate.** Only explicit lifecycle, graph, and step commands write.
- **Goals are pointers, not copies.** A registered goal path references a file
  that lives wherever its owner keeps it.

## License

MIT
