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

Current release: **10.2.0**
([GitHub releases](https://github.com/ianzepp/vivi/releases),
[notes](docs/release-v10.2.0.md)).

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

## Agent skill

Agent-facing CLI guidance lives at [`skills/vivi/SKILL.md`](skills/vivi/SKILL.md).
Symlink that folder into a client skill directory:

```sh
ln -s "$(pwd)/skills/vivi" ~/.agents/skills/vivi
```

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
| `address` | Derived: `{name}@{mailspace}.local` |

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
You are fleet role head-ceo.
Load charter: vivi role charter show head-ceo --project <root>
Load task:    vivi task show <handle> --project <root>
```

`vivi mailspace identity add|list|rename` remains as a thin roster path; prefer
`vivi role` for new work. Existing `[[identities]]` entries in
`mailspace.toml` load as roles with empty optional fields.

The default local domain is derived from the project directory name. In a
project named `hanta-monitor`, `cto` resolves to
`cto@hanta-monitor.local`. Unknown local roles are rejected, external
recipients are rejected by the local delivery commands, and mixed
local/external sends are not sent automatically. Use the existing
`compose`, `enqueue send`, and `exec send` flows for human or external mail.

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

### Remote lane pool

`vivi pool` manages preseeded remote machines so agent work can start in
seconds against a warm build, and hands a Vivi task to one of them. The pool
runs on Fly (the default) or on Docker (`provider = "docker"`: containers on the
local daemon, or on a remote one over SSH); `[pool.fly]` and `[pool.docker]` are
the host-specific parts of the config.

```
vivi pool create [--slots N] [--base <commit>]   # make the app if missing, create + seed slots, wait until free
vivi pool status [--json]                        # name, id, state, task, running time of budget, seeded base
vivi pool destroy (--slot <name> | --all) --yes
vivi pool dispatch <task> --model <slug> [--from <ref>] [--branch <name>] [--budget <secs>] [--allow-default-branch]
vivi pool wait [<task>...] [--timeout <secs>]    # block until a dispatch finishes; collect it
vivi pool reconcile                              # collect finished, stop overdue, free slots
vivi pool doctor [--json]                        # read-only checks; each finding names its fix
vivi pool doctor --test [--long] [--json]        # also run a live test of the provider and of Vivi (billable on Fly, free on Docker)
```

`doctor` checks three sides under their own headings: the provider (Fly: `fly`
installed and logged in, an API token, the app, the harness secrets; Docker: the
`docker` binary, the daemon, the image, daemon memory against a slot's limit,
the harness environment names), the
repository (GitHub: `openssl`, the App key, a token, the default branch and its
protection), and the pool itself (config, slot count, each slot's state and how
far its seeded base trails the default branch, leaked runners, orphaned
dispatches, stale claims, overdue slots). It changes nothing, prints a `fix:`
line under every `warn` or `fail`, and exits nonzero when any check fails.
Run it at orchestrator boot and whenever something looks stuck.

`doctor --test` adds a live exercise that answers two questions apart: is the
provider working, and is Vivi's own pool machinery working? It first runs the
read-only checks above and stops if the provider checks fail (the repository
checks are shown but do not block it). Then it creates a throwaway pool named
`<pool>-doctor` of two of the smallest machines in the same provider
application and runs stand-in jobs on it. Each step is reported `ok` or `fail`
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
pool uses.

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
same name, and its labels are the mark), delivers the job as one archive, starts the
slot, records the dispatch on the task, and returns without waiting. The slot
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

Slots are discovered from Fly machine metadata (`vivi_pool*`) or Docker
container labels, marks and claim containers; Vivi keeps no local slot records.
The Fly app defaults to the pool name (itself the mailspace name) plus a dash
and six hex digits derived from the Fly organization, because Fly app names are
unique across all Fly customers (`[pool.fly] app` overrides it; a pool made
before this default lives in the app named after the bare pool name, so set
`app` to that to keep it; if the name is taken, `create` says so and names the
line to set). `create` makes the app in `[pool.fly].organization` (default
`personal`) when it does not exist. A job's Git address is `https://github.com/<repo>.git`
for `[pool].repo = "owner/name"`; slots take it as a full remote address.
Model keys are app secrets on Fly: `create` and `dispatch` list the app's
secret names, never values, and warn about harness `env` names without one
(vivi does not set secrets). On Docker the `env` names are copied from the
environment of the `vivi` process when a slot is created; `create` and `doctor`
name the ones that are unset. Configure with a `[pool]` table in
`.vivi/mailspace.toml` (repo, slots, image, setup/seed commands, `build_dir`),
`[pool.fly]` (optional app, organization, region, size, memory, tmpfs) or
`[pool.docker]` (optional host, memory, cpus, tmpfs),
`[pool.github]` (App id and key path), `[pool.harness]` with `run`, `models`,
`env` and `rules`, and `[pool.committer]`; Fly keys left in `[pool]` are
rejected with a message naming `[pool.fly]`. See `skills/vivi/SKILL.md` for the full table. Nothing in
the pool is specific to a language, a repository, or an agent harness. The
GitHub App key is read by `openssl` when minting a one-hour, single-repo token;
Vivi never stores or prints it, and the token is not in the agent's
environment.

#### Docker provider

```toml
[pool]
provider = "docker"
image = "ubuntu:24.04"          # the daemon pulls it when missing
# ...repo, slots, setup, seed, build_dir as for Fly

[pool.docker]
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
the values of the `[pool.harness].env` names are read from the environment of
the `vivi` process when a slot is created and then live in the container's
configuration on that daemon (anyone with access to the daemon can read them
with `docker inspect`; names only appear in config, and no value is printed or
written to the mailspace); the daemon may be arm64 or amd64 (each slot records
its architecture, shown by `status` and `doctor`), so `setup` commands must not
hard-code an architecture; two mailspaces with the same pool name on one daemon
share slots, so give them distinct `[pool].name`s. With `git`, `zstd` and `curl`
already in the image, the pool and `doctor --test` run offline.

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

## Storage Layout

Everything Vivi owns lives under the project's `.vivi/` directory:

```
myproject/.vivi/
├── mailspace.toml         # name, description, identities, archive, probes, judgment, chat
├── mail.sqlite            # items, events, goals, local links, and the work graphs
├── blobs/                 # content-addressed message payloads
├── chat/                  # chat identity, room cursors, and the outbox; the directory gitignores itself
└── judgment-corpus.jsonl  # shadow-screen answers, appended by `vivi step --apply`
```

Rules:

- `mail.sqlite` is the store; there is no separate index to rebuild
- Message handles are the first eight characters of the id. A chat item keeps
  that id, and so that handle, on every checkout that pulls it. Any other
  item's id is store-local, and its handle is stable inside that mailspace
- Room items filed by `vivi chat pull` are a replica of the room. `chat prune`
  can drop old ones, and `chat resync` rebuilds them
- Deleting `.vivi/` deletes that project's coordination state, its local mail,
  and the chat replica

## Commands

`vivi --help` is the live top-level list. In 10.2.0 that is: `board`, `boot`,
`chat`, `mailspace`, `mail`, `task`, `need`, `want`, `memo`, `goal`, `role`,
`cycle`, `trace`, `graph`, `step`.

After 10.2.0 the tree also has `pool` (remote lane pool: create, status, destroy, dispatch, wait, reconcile, doctor) and `rule`: project rules every seat can read.
`--local` marks a rule that a later sync should skip. Nothing syncs rules yet.

`chat` is new in 10.2.0: `vivi chat room create`, `join`, `rooms`, `send`,
`read`, `watch`, `flush`, `pull`, `resync`, `prune`, `leave`, and `status`
connect two vivi checkouts anywhere on the internet through a public
rendezvous server — rooms are real mailspaces, identity is a transparent
per-project ED25519 key, offline sends queue and flush on reconnect, and the
board appends an unread-chat line. `send --kind need|task|want` crosses a work
item to another seat, and `pull` files room needs, tasks, and wants into the
mailspace under traveling ids that keep one handle on every machine; `prune`
sheds old cached items and `resync` rebuilds them byte-identically from the
room. The server is a sibling repository, `vivi-server`.

Everything Vivi does is project-scoped, so `--project <root>` is accepted either
before or after the subcommand and every command resolves the same mailspace.
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
