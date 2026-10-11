---
name: vivi
description: Use the Vivi CLI for project mailspaces, roles, goals, work graphs, the remote lane pool (`vivi pool`: preseeded remote machines that run a task and push a branch), local email search and drafts, queues, and explicitly authorized email mutations.
---

# Vivi

Use `vivi` as the interface. Do not inspect or mutate `.vivi` databases, blob
trees, email caches, or indexes directly unless the user explicitly requests
low-level debugging.

The command surface evolves. Before relying on exact flags or versioned
features, run:

```sh
vivi --version
vivi --help
vivi <command> --help
```

Use `--project <root>` whenever the project cannot be inferred safely from the
current directory. If the task concerns Vivi product implementation or release
behavior, use only the source repo named by the user.

## Project Mailspaces

A project mailspace is durable coordination state. Use these kinds
consistently:

| Kind | Meaning |
|---|---|
| `task` | Concrete work assigned now, with an owner and done condition |
| `need` | Must-do-soon work awaiting lowering or assignment |
| `want` | Deferred work or an idea whose precondition is not yet met |
| `mail` | Question, report, decision, or handoff communication |
| `memo` | Private durable context for one role; not work or communication |
| `rule` | Project rule shared by every seat; not work and not a private memo |

The board aggregates open work. Kind-specific `list` commands orient within one
queue. `show` loads one handle. Use `dump` only for bounded audits or recovery;
full dumps are noisy and can hide the current frontier.

A handle is the first eight characters of the message id — a fixed-width
prefix of one record, so it never changes as other records arrive. Any
mailspace command that takes a handle also accepts the full message id. Two
records can share a handle when their ids collide; resolving such a token
reports it as ambiguous rather than guessing, and the full id is the remedy.

Default inspection order:

```sh
vivi boot --project <root>
vivi step --project <root> --json
vivi board --project <root> --for <role> --process --graph --json
vivi task list --project <root> --for <role> --status open
vivi need list --project <root> --for <role> --status open
vivi mail list --project <root> --for <role>
vivi task show --project <root> <handle>
```

## Boot

`vivi boot` prints the whole project frame in one read: seat bindings against
observed process state, declared cadences and their silence, unabsorbed mail,
open handles with verdicts, registered goals with register tallies, active
project rules, memos, charter heads, and the backlog sliced into
seat-sized groups. It is read-only,
stateless, and idempotent, so it serves a cold boot and a post-compaction warm
boot alike, and two runs are comparable.

```sh
vivi boot --project <root>
```

Run it first when orienting or re-orienting. Every section is capped, and the
digest closes with a truncation manifest naming each cap and how much it
omitted — so the digest is complete in coverage and bounded in length. Boot
never absorbs, closes, promotes, dispatches, or writes. Act on it with the
ordinary verbs. The digest has one shape: there is no JSON or sweep variant.

Seats are listed for roles that hold open work or carry a bound process. The
rest of the roster is counted in the totals line, so a role you expect and do
not see is being summarized rather than omitted.

Handles carry a closed verdict vocabulary: `open`, `blocked`, `stale`, `live`,
`unbound`, `unverified`, `dead`, `zombie`, `remote`, `unknown`. `unverified`
means a subagent harness owns liveness and an OS PID is not a valid signal;
`unbound` means the seat is active with no bound process; `stale` means a probe
found the landing already present; `blocked` comes from the backlog graph and
outranks a probe verdict.

For a registered goal, boot reads the Status line and any markdown table with
a `Status` column. When the Status line's `N/M` completion claim disagrees with
the register's own done count, it prints `MISMATCH` with both numbers.

### Probes

Boot renders the facts Vivi owns. Facts it cannot own — git ancestry, lane
state, the live seat count of a harness — arrive through probes the project
declares in `.vivi/mailspace.toml`:

```toml
[[probes]]
name = "example"
command = "scripta/boot-probe"   # executable, resolved against the mailspace root
```

The probe prints one JSON object on stdout:

```json
{
  "facts": ["main is at deadbeef"],
  "sections": [{"title": "world", "lines": ["radix main deadbeef"]}],
  "verdicts": [{"handle": "abc12345", "verdict": "stale", "detail": "commit already on main"}]
}
```

`facts` render with the preamble, `sections` render as named blocks, and
`verdicts` merge onto the handle inventory by full handle or by a unique
prefix of at least four characters. A probe that is missing, exits non-zero,
or prints unparseable JSON is reported under `probes skipped` and never fails
boot, so a project with no probes still gets every native section.

`vivi step --json` is the bounded intake: a fixed-shape manifest of
dispatches and exceptions over the backlog graph. Prefer it (and the delta
tools below) over dumps and raw graph exports as loop input. Use `vivi trace
<handle>` to reconstruct communication lineage; do not confuse that tree
with the executable work graph.

## Roles and Goals

Roles are durable seats with a mailbox, kind, status, charter, harness, model
preferences, cadence, and labels. Read the charter before operating as a role:

```sh
vivi role show <role> --project <root>
vivi role charter show <role> --project <root>
vivi role status <role> --project <root>
```

Capacity and model fields are preferences. Use `role status` or
`board --process` when process state matters. A host starting a role should
pass a short pointer to the charter and one assignment handle rather than
pasting the standing procedure.

Registered goals are the project's explicit working set:

```sh
vivi goal list --project <root> --json
vivi goal show --project <root> <handle-or-path>
```

Goal registration does not replace reading the referenced document. Adding or
dropping a goal changes registration only; it does not create or delete the
document.

## Lifecycle

Use `send` to create a project-local record and `mail reply` to continue its
thread. `--depends-on` on any work-kind send (task, need, or want handles)
creates a graph edge; handles are validated before anything is created, so
an unknown dependency fails the send. `X-Vivi-Depends-On` headers remain on
the message as evidence. There is one dependency substrate — the graph — for
small and multi-unit relationships alike.

Closing a task, need, or want records its current disposition. Reopen when the
CLI permits and evidence changes. `want promote` moves deferred work into the
must-do queue; promotion is request-only and never fires because a
dependency completed. Lifecycle notes (`--note`) never inbox the acting
identity: other participants receive the receipt, and the actor keeps only a
read sent copy.

`vivi task from <source> --for <identity> --subject … --body …` creates a task
from an existing record and records that source on the new task, which is where
task associations are read from — the work graph holds the dependency chains.
A want carries no list of tasks created from it.

`absorb` seals a record. After absorption it cannot be changed, reopened,
promoted, dropped, reprioritized, marked, or deleted. A later reply or derived
task is a new record. Absorb only when the identity is finished with the item
and its state should remain frozen.

A need or a want can carry marks: free slugs such as `bug`. A mark is an
event on the item, not a rewrite of the message. `need mark` / `need unmark`
and the same pair on `want` each take the handle, one or more slugs, and
`--for` the holder. Each slug is its own event. Repeating a slug that is
already in that state changes nothing. `show` prints `Marks: bug regression`
when any are present, sorted. `list --mark` is repeatable and keeps an item
only when it carries every named slug. The slug is letters, numbers, dot,
dash, and underscore, compared exactly. Marks survive promote, done, reopen,
and drop. They do not change graph readiness, `vivi step`, or `need bind`.
They stay in this mailspace; nothing syncs them.

Memos preserve durable role context. They are not routing, assignment, or
completion evidence. Standing project rules belong in `vivi rule`. Delete
superseded unsealed memos; absorb memos that should be retained as immutable
history.

When a mailspace has a configured archive, absorption exports one Markdown
record to that dedicated Git repository. Vivi does not stage or commit the
archive. Use the archive commands and `--help`; do not write exported files by
hand.

## Rules

A rule is the project's shared rule set. Every seat reads the active set with
`vivi rule list` and, when any are active, in `vivi boot`. The boot section
is capped at 10, ordered by subject then handle, and omitted when empty. A
rule is not work: it has no backlog node, and `vivi step` does not dispatch
it.

```sh
vivi rule save --for <identity> --subject <s> (--body <b> | --body-file <f>) [--local]
vivi rule list [--for <identity>] [--all] [--json]
vivi rule show <handle> [--json]
vivi rule search <query> [--for <identity>] [--subject] [--all] [--json]
vivi rule supersede <handle> --for <identity> --subject <s> (--body <b> | --body-file <f>) [--local]
vivi rule retire <handle> --for <identity> [--note]
vivi rule absorb <handle> --for <identity> [--note]
```

The bytes are written once. There is no edit and no delete. `supersede`
saves a new rule and drops the old one from the active set. `show` still
reads the old rule and names the successor. `retire` withdraws a rule. A
second retire prints `{handle} already retired` and appends nothing.
`absorb` seals a rule. An absorbed rule refuses a later retire or
supersede. Absorbing a retired or superseded rule is allowed; superseded
still outranks retired, and retired still outranks absorbed. Any mailspace
identity may retire, supersede, or absorb. `--for` records who acted.

`--local` writes `X-Vivi-Rule-Scope: local` on that rule. Without it the
scope is `project`. Scope is fixed when the rule is saved. A superseding
rule has its own scope. Nothing syncs rules yet. The header is the mark a
later sync will use to skip a local rule.

List, search, and boot show the active set. `--all` includes retired,
superseded, and absorbed rules. `--for` on list and search keeps rules that
identity wrote, including names it used before a rename. Duplicate subjects
are allowed. Search matches subject and body, case-insensitively.
`--subject` matches the subject only.

## Work Graphs

Work graphs are project-local DAGs. Topology can be authored through the
supported Mermaid subset (`graph import` / `apply`) — or it accumulates
automatically: every `task` / `need` / `want` send mints an open node in the
per-mailspace `backlog` graph, and `--depends-on` on any work-kind send
(accepting task/need/want handles, validated before send) becomes a
prerequisite edge. Confirm the installed Vivi version and command help
before use.

Core semantics:

- import or apply validates the whole graph atomically;
- source node identifiers remain stable across label changes;
- a node is ready when all solid-arrow prerequisites are done;
- activation binds one task attempt to a ready, dispatchable node;
- completion may unlock successors, and writes a `step_decision` graph event
  in the same transaction recording the deciding path;
- active or completed prerequisites cannot be rewritten incompatibly;
- graph readiness describes eligibility, not scheduling policy.

**Node kinds and operator gates.** Rhombus nodes (`id{label}`) import as
`decision`; an `id:::kind` suffix or `class <ids> <kind>` statement marks
`decision`, `stub`, or `parked`; stadium `id([label])` and rect nodes are
ordinary work. Gated kinds never dispatch: `graph ready` lists them under
`gates` instead of `ready`, and `graph activate` refuses them. Resolve a
gate with `graph complete --note` (the ruling is the completion record).
Dotted edges (`-.->`, `-.-`) are non-gating couplings — topology evidence
that never blocks readiness; `-->` is the only prerequisite arrow. Run
`vivi graph import --help` for the accepted-subset summary; parse errors
name the rejected construct and the subset.

**Step scope.** `vivi step` adjudicates only the `backlog` graph. Imported
topologies never enter the step manifest — dispatch their nodes with
`graph activate` at dispatch time and complete them at reconcile. `graph
ready` without an argument lists every graph's frontier, including backlog.

Lifecycle moves keep backlog nodes in step: `task done` / `need done` /
`want done|drop` complete the item's node and unlock dependents; `reopen`
re-locks them (cascading through join parents and content siblings);
`want promote` never changes node state and wants never dispatch in
`vivi step` before explicit promotion. Lowering is a graph fact:
`vivi need bind <need> <task>...` binds unit tasks to a need, and the need
auto-completes when every bound unit lands — node and mailbox item together
(`via=join` in the event log); reopening a bound unit restores the need to
open the same way. `vivi need show <handle>` prints the bound units with
their states. Imported Mermaid graphs are for
non-item topology (pipelines, environment flows); delivery lowering uses
`need bind` in the backlog, so work is never double-represented.

**Dependency grammar.** Structural dependencies — a handle cited with
`--depends-on` — become edges and derive readiness. Dependencies discovered
after filing use `vivi graph connect <dependent> <prereq>` (idempotent;
refuses self edges, active/done dependents, and handles without nodes).
Ambient conditions
("when a seat is free", time windows, operator approval) stay prose in the
body and are evaluated by the host at dispatch; they are never encoded as
topology. Multi-recipient sends are one work item: dependency handles may
cite any copy (edges canonicalize to one copy per content), and completing
any copy completes its siblings.

**Dispatch sequence.** `task send` mints the node; `graph activate
<handle> --task <handle>` binds the attempt and marks it active — active
nodes leave the `vivi step` manifest, so in-flight work is not re-offered.
The activate receipt and `attempt_bound` event record the task's content
hash (`content=<sha256>`) — the citable pin for "task body as dispatched";
every record's `show` output prints its `Content:` hash the same way.
Bare source ids address the backlog graph; imported topologies use
`graph:source-id`. Lifecycle moves act as the identity holding the item
(`--for`): the send receipt's `created <recipient> <handle>` line names the
holder, and closing under the wrong identity fails with the holder named in
the error. **Settle sequence.** The worker settles with
`task done --verdict/--repo/--tip` (the node completes, dependents unlock,
a `step_decision via=lifecycle` event records the transition); the host
then runs `vivi step --apply <handle>` — idempotent, never settles itself —
which records `via=step-apply` and runs the receipt screen when a judgment
provider is configured.

Use `graph show`, `graph ready`, and `board --graph` for inspection.
`graph ready` prints a `counts` line first — use it (or `--kind
task|need|want|decision|stub|parked`) instead of reading long id lists.
Use `vivi step [--json]` for a mechanical adjudication of the backlog into
`dispatches` (ready, verifiable work with clause counts) and `exceptions`
(reason vocabulary: `want_requires_promotion`, `lowered_awaiting_units`,
`no_done_when`, `item_missing`, `not_settled`, `untracked_item`). Wants
render as parked in their exception detail. A clauseless task/need body
warns at send time, and a duplicate work send (same sender, same subject,
still open in the recipient's folder) warns on stderr without blocking.
`vivi step
--apply <handle>` completes an already-settled item's node and lists
transitions under `decisions`; applying an item the lifecycle already
settled records `via=lifecycle (already settled; nothing to apply)` instead
of silence. The coordination host decides which ready nodes to dispatch.

**Citizenship audit.** `vivi graph audit [--repair]` checks the backlog
invariant — every work item sent while the graph existed has a node in step
with its folder state — and reports `missing_node`, `node_state`,
`node_kind`, and `orphan_node` findings. `--repair` mints missing nodes
(from folder state and dependency headers), completes nodes for settled
items, settles need mailboxes whose join fired, and corrects kinds.
`graph activate` / `need bind` errors point here. Stale vivi binaries
silently skip node minting entirely (observed with an old Homebrew 8.1.0 on
PATH): `mailspace status` prints the running binary's version — check it
when nodes go missing, and keep one vivi on PATH.

### Work-graph flow (dispatching ordered work)

The procedure for turning a delivery document into ordered, dispatched work
lives in [`references/work-graph-flow.md`](references/work-graph-flow.md): read
it before lowering a multi-unit delivery. The load-bearing rules:

- Every unit is its own task send with its own handle; the send mints the node.
  Cite prerequisites with `--depends-on`; an unknown handle fails the send.
- `done` means the worker closed the handle. It does not mean the code is on
  main, and Vivi has no merged state. "On main" is a **merge task** filed with
  `--depends-on <unit>`; anything that needs the code `--depends-on` the merge
  task, not the unit.
- `vivi step` dispatches a ready node only if a line of its body begins with
  `done_when:` (a `write_scope:` line is reported, not required). One-line
  compressed forms, bullets, and bold labels do not match.
- Per unit, in order: file with `--depends-on` and a valid body, `need bind`
  the need to its units (and their merge tasks), claim the lane, `graph
  activate <handle> --task <handle>`, spawn the seat, and after it closes run
  `step --apply <handle>`. A merge landing closes the merge task and turns its
  dependents ready.
- Do not stack units on one handle: stacked units are not separate nodes and
  one handle holds one lane lock.
- `graph audit --repair` fixes missing nodes, kind drift, and node/folder state
  drift (folder wins); read the audit first, because `node_state` repairs can
  reverse a hand completion. The reference file lists exactly what each finding
  repairs.

### Judgment provider (optional, off by default)

A user-level `[judgment]` section enables System One receipt screens on
`step --apply`: one yes/no judgment per `done_when` clause plus a
completion-honesty check, answers appended to
`.vivi/judgment-corpus.jsonl` as calibration evidence. Screens are shadow —
they never gate the mechanical completion — and absent or unreachable
providers degrade to `judgment=skipped(<class>)`. No read path makes
provider calls.

```toml
[judgment]
provider = "typesafe"
key_cmd  = "cat ~/.config/secrets/typesafe-ai.key"
```

Authentication is exclusively `key_cmd` (`sh -c`, `password_cmd` semantics):
never an envvar (ambient to spawned processes) and never an inline key
(no field exists). The table lives in the project's own mailspace config at
`<project>/.vivi/mailspace.toml`; there is no user-level config in this repo.
A wrong path fails silently (`judgment=off`, empty corpus), so verify with one
apply after configuring. Screens judge the receipt against the clauses: tasks
that want meaningful screens should carry their validation claim in the body,
not only verdict flags.

## Remote lane pool

`vivi pool` manages a pool of preseeded remote machines ("slots") and hands a
Vivi task to one of them. Two machine hosts exist, chosen by
`[pool].provider`: `fly` (the default; Fly Machines, billed by the second) and
`docker` (containers on a Docker daemon: the local one, or a remote one over
SSH; see "Docker provider" below). `[pool.fly]` and `[pool.docker]` hold the
host-specific settings. Everything else in this section works the same on both.
In-place reseed, other hosts (a plain SSH host is not a host), several named
pools, and overflow machines do not exist; do not invent them.

```sh
vivi pool create [--slots N] [--base <commit|branch>] [--timeout SECS] [--porcelain]
vivi pool status [--json | --porcelain]
vivi pool destroy (--slot <name-or-id> | --all) --yes
vivi pool dispatch <task> --model <slug> [--from <ref>] [--branch <name>] [--budget SECS] [--allow-default-branch] [--porcelain]
vivi pool wait [<task>...] [--timeout SECS] [--porcelain]
vivi pool reconcile [--porcelain]
vivi pool doctor [--json | --porcelain]
vivi pool doctor --test [--long] [--json | --porcelain]
```

**Agents: pass `--porcelain` to every pool command you run, especially a
background `vivi pool wait`.** Output of a background command is read back into
your context, and the default output of `create`, `wait` and `doctor --test`
carries progress lines and a heartbeat every 30 seconds meant for a person at a
terminal. `--porcelain` prints only result lines on standard output (no start
lines, no heartbeats, no decoration); exit codes are unchanged. `--porcelain`
with `--json` is a usage error (`--json` stays the full structured detail,
silent until the end).

Default output (for a person): progress to standard error, results to standard
output. A line when each step starts (so the last line names what is running),
the usual line when it finishes, a heartbeat every 30 seconds while waiting
(`waiting  3f2a on p-slot-2  running 150s of 1800s budget`; for `create`
`waiting  seeding p-slot-1 120s, p-slot-2 118s (gives up at 900s)`; none for a
wait under 30 seconds), and `doctor --test` states up front that it takes about
three minutes (about ten with `--long`).

#### Porcelain line formats

One line per result; fields separated by single spaces, in this order; `-` for
an absent field; a value that is empty, holds whitespace, a quote or a
backslash, or is itself `-` is double-quoted with `\"`, `\\`, `\n`, `\r`, `\t`,
`\u{hex}` escapes; times are epoch seconds.

```
create     home TEXT                                          only when the app was made
           created NAME ID STATE SECONDS EXIT OUTCOME ERROR   OUTCOME: seeded|timeout|failed|unmarked
           exists POOL COUNT                                  nothing to create
           issue KIND SUBJECT MESSAGE FIRST CLEARED           KIND list-error, SUBJECT -
status     slot NAME ID STATE TASK RUNNING BUDGET BASE ARCH CREATED
dispatch   dispatched TASK SLOT JOB MODEL FROM START BRANCH BUDGET
wait       collected TASK SLOT STATE DETAIL                   STATE: finished|timeout|lost
           nothing-open                                       already collected
           issue KIND SUBJECT MESSAGE FIRST CLEARED           KIND poll-error, SUBJECT the task
reconcile  reconciled TASK SLOT STATE DETAIL                  STATE: running|finished|timeout|lost|error|stale-claim|orphan-stopped
           nothing-open                                       no dispatch was open
doctor     check STATUS GROUP NAME DETAIL FIX MILLIS          STATUS: ok|warn|fail|skipped
           summary OK WARN FAIL SKIPPED
```

Examples: `collected 3f2a p-slot-2 finished "pushed work/x 0123456789"`,
`created p-slot-1 m1 stopped 42 0 seeded -`,
`issue poll-error 3f2a "Fly unreachable" 1760000000 1760000090` (CLEARED `-`:
the problem lasted to the end). `wait` and `create` print their `issue` lines
even when they then fail, so a slow wait explains itself. Warnings (such as
missing harness environment) stay on standard error.

### doctor

`vivi pool doctor [--json]` is read-only: it creates, starts, stops, marks and
pushes nothing, and repairs nothing. It prints one line per check (`ok`, `warn`,
`fail`, or `skipped` when an earlier check it depends on failed) under three
headings, so you can tell which side a problem is on, and each `warn` or `fail`
carries a `fix:` line naming the command that fixes it. Exit code is nonzero
when any check fails; warnings alone exit zero. `--json` gives the same data
(`ok`, `counts`, and `checks` with `group`, `name`, `status`, `detail`, `fix`).

- **Provider (Fly)**: `fly` is installed and logged in, an API token can be
  obtained, the app exists, and every `[pool.harness].env` name is an app
  secret (names only; a name the image provides can be ignored).
- **Provider (Docker)**: the `docker` binary runs; the daemon (local, or
  `[pool.docker].host`) answers, with its version and platform; the image is
  present or the registry has it (nothing is pulled by `doctor`); the daemon
  has at least as much memory as one slot may use (`warn` otherwise); every
  `[pool.harness].env` name is set in this process's environment (`warn` names
  the unset ones: slots created now would lack them).
- **Repository (GitHub)**: `openssl` is present, the App key file is readable, a
  one-hour token mints for the repo (never printed), the default branch
  resolves, and whether it is protected (whether the App may bypass the
  protection is not checked).
- **Pool (Vivi)**: `[pool]` validates; slot count against `slots`; each slot's
  state, seeded base, and how many commits that base is behind the default
  branch tip; a running slot with no open dispatch (leak); an open dispatch
  whose machine is gone (orphan); a held slot with no dispatch (stale claim)
  and its age; a busy slot past its budget (overdue). Slots on an older boot
  script are not detectable and are not checked.

Run it at orchestrator boot and whenever something looks stuck (a dispatch that
never finishes, a slot that will not free, an unexpected bill). An orphaned
dispatch (its machine no longer exists) is closed by `vivi pool reconcile`: it
is recorded on the task as `lost` and the task can be dispatched again.

### doctor --test

`vivi pool doctor --test [--long] [--json]` runs the read-only checks, then a
live exercise that answers two questions apart: is the provider working
(**Live test: provider**), and is Vivi's own pool machinery working
(**Live test: Vivi**)? It stops after the read-only checks if the provider
checks fail; the Repository checks are shown but never block it.

- It creates a throwaway pool `<pool>-doctor` of two of the smallest machines
  on the same host (the same Fly app, or the same Docker daemon), seed command
  `true`, no setup commands. It never touches the real pool's slots. **On Fly
  it creates billable machines for a few minutes** (about 3 minutes; `--long`
  adds about 7 more). **On Docker it costs nothing**: two containers on your
  daemon, removed afterwards, about 1.5 minutes with a local daemon and an
  image that is present (`--long` adds about 7 more, the job really sleeps past
  six minutes). On a local Docker daemon it is the free check to run after any
  change to the pool code, whichever host the real pool uses.
- Source: a tiny tree carried in the vivi binary, turned into a bare Git remote
  on each slot's own disk (`file://`). Jobs fetch from and push to that remote.
  The harness is a built-in shell command that appends a line to a file. No
  repository, GitHub App, token, model key or harness is used, and the
  project's mailspace is not written (the throwaway tasks live in a temporary
  mailspace).
- Provider steps: create two slots; both reach `free`; start and stop round
  trip. Vivi steps: two dispatches take two different slots; a third is refused
  with "no free slot"; `wait` collects a job during the linger; a job past its
  budget is killed and recorded; a second dispatch continues a pushed branch
  with a new commit on top (it must land on the slot that holds the branch,
  since each slot has its own remote); a job finished while nobody waits is
  collected later from the stopped slot and the slot ends stopped and free;
  `reconcile` finds and stops a running slot with no dispatch.
- On Docker the same steps run, and the same code. Two collect paths differ
  underneath: a finished job is read by `exec` while the container still
  lingers, and from the stopped container's disk with `docker cp` (no restart)
  once it has stopped. A step that cannot apply to a host is reported
  `skipped` with the reason, never `ok`; none is skipped today.
- `--long` adds one step: a `wait` that runs past six minutes (the Fly API
  token lives about six minutes) and still collects.
- Every step is `ok`, `fail` (with the side, what was expected and what was
  observed) or `skipped` (an earlier step failed), with a duration. Exit code is
  nonzero when anything failed. `--json` adds `millis` to each step.
- Cleanup always runs, also after a failed step or Ctrl-C (a second Ctrl-C quits
  at once): the test slots are destroyed and a last step confirms nothing of the
  test pool remains. If something does, the step names the machines and the
  command that removes them. Slots left by an earlier killed run are removed
  first and reported as a warning.
- Not covered, on purpose: minting a repository token, fetching and pushing
  against the real repository, repository rules (branch protection), harnesses
  and models. The plain checks look at the first group; a real dispatch is the
  only test of the rest.
- When to run it: after a change to the pool code, before a campaign that
  depends on the pool, and when something looks stuck.

### Slot states

| State | Meaning | Claimable |
|---|---|---|
| `free` | seeded, stopped, no task holds it | yes |
| `seeding` | `create` started it and has not yet seen the seed finish | no |
| `unseeded` | stopped without the seed mark: the seed failed, or `create` was interrupted before marking (re-running `create` adopts a clean one) | no |
| `busy` | a task holds it and the machine runs (booting, working, or lingering) | no |
| `finished-uncollected` | a task holds it and the machine stopped; the result is still on its disk | no |

`status` shows `task <handle>` on a held slot, and for a slot with an open
dispatch how long it has run against its budget (`running 95s of 1800s`;
`--json` adds `running_secs` and `budget_secs`). A busy slot with no open
dispatch says so; `reconcile` stops it. A slot with an uncollected result is
never claimed.

### Slots, create, destroy

- A slot is a Fly Machine with a persistent root disk (on Docker, a container;
  see below). On first boot it runs
  the configured `setup` commands as root, fetches the repo at the base commit,
  runs the `seed` commands in the checkout with a memory disk over `build_dir`,
  packs the build output, and exits. `create` then removes the fetch token from
  the machine's config (the machine stays stopped) and sets the seed mark in the
  same update. Later starts restore the pack, run one job, linger, and stop.
- `create` fills the lowest free slot numbers up to N (default and maximum:
  `slots` in config), seeds them in parallel, waits until each is stopped, and
  prints one line per slot with its id, state, and seed seconds. Existing slots
  are left alone. It exits nonzero if a slot timed out or exited nonzero.
- Slot state comes from Fly, never from local files: machine metadata
  `vivi_pool`, `vivi_pool_slot`, `vivi_pool_base`, `vivi_pool_state`
  (`seeding`|`ready`), and, while a task holds the slot, `vivi_pool_task` and
  `vivi_pool_claimed`. Machines in the same Fly app without `vivi_pool` are
  never listed or touched. (On Docker the same facts live in container labels,
  a mark file and a claim container; see below.)
- `--base` takes a commit, branch, or tag, resolved to a full hash through
  GitHub. Default: the repo's default branch tip.
- `create` does not return until a fresh listing shows each new slot `free`.
- The Fly app is `[pool.fly].app`, else the pool name (which defaults to the
  mailspace name); there is no prefix or suffix. Fly app names are unique
  across ALL Fly accounts and Vivi does not try to make the name unique: if Fly
  says it is taken, pick another (`[pool] name` or `[pool.fly] app`).
  `create` makes the app when it does not exist, in
  `[pool.fly].organization` (default `personal`). If Fly says the name is
  taken, `create` says so and names the lines to set. Model keys are app secrets:
  `create` and `dispatch` list the app's secret NAMES and warn, naming the
  harness `env` entries with no secret and the command shape
  `fly secrets set -a <app> NAME=<value>`. Vivi never reads or sets a secret
  value; a name the image itself provides can be ignored.
- `destroy` requires `--yes` and only touches this pool's slots.
- A slot can only fetch what the remote repository already has: push the base
  (the commit or branch passed as `from`) before dispatching.
- `--model` is resolved by the caller, in the harness's own vocabulary; Vivi
  only checks it against `[pool.harness].models`.
- Slots made by the first iteration (before dispatch existed) lack the seed
  mark and the new boot script: destroy and re-create them.
- Requires `fly` (flyctl, logged in; or `FLY_API_TOKEN`) and `openssl` on PATH,
  and a GitHub App with contents access to the repo. Tokens are one hour,
  narrowed to the one repo, and never printed.

### Docker provider

`provider = "docker"` runs each slot as a container. Use the `docker` CLI; no
client library is involved, so a local and a remote daemon go through the same
code: `[pool.docker] host = "ssh://user@server"` is passed to the CLI as
`DOCKER_HOST`; omit it for the local daemon (or whatever your own
`DOCKER_HOST` names). Never run a plain SSH host as a "provider": only Docker.

- A slot is a container. A stopped container is a slot at rest and its writable
  layer is the persistent disk; its main process is the boot script, so it
  stops when the script exits. Containers are created with `--init`, a memory
  limit, an optional CPU limit, and never privileged.
- The memory disk over `build_dir` is a tmpfs mount declared when the container
  is created (`/work/repo/<build_dir>`, with `exec`), because the script cannot
  mount without privileges. The boot script skips its own mount when
  `BUILD_MOUNTED` is set. The tmpfs counts against the container's memory limit.
- Labels cannot change after creation, so they hold only fixed facts:
  `vivi_pool` (pool name), `vivi_pool_slot`, `vivi_pool_base`,
  `vivi_pool_role=slot`. The architecture and the seed mark (`vivi_pool_state`)
  are small files under `/work/vivi-marks/` on the slot's disk, written and
  read with `docker cp` (which works on stopped containers).
  Nothing is kept locally.
- The claim is a separate, never-started container named
  `vivi-claim-<slot id>`, with the task and claim time as its labels
  (`vivi_pool_task`, `vivi_pool_claimed`). The daemon refuses a second
  container with a name in use, so two dispatchers (any number of `vivi`
  processes, on any machines that share the daemon) cannot both claim a slot;
  the loser moves on. After winning, the claim re-reads the slot (stopped,
  seeded) and takes the claim back if it is not free. Removing the claim
  container frees the slot. Limits: atomicity is the daemon's, so it holds
  per daemon only; there is no expiry, so a dispatcher that dies between
  claiming and recording leaves a claim that `vivi pool reconcile` frees as
  stale after five minutes (`doctor` shows it); anyone with daemon access can
  remove it by hand. The test is `cargo test --test pool_docker_live --
  --ignored` (a real daemon: one winner among twelve claimants and among
  twelve separate `docker create` processes).
- Job delivery copies the job archive into the stopped container, then starts
  it. Collecting execs into the container while it runs, and copies the result
  files out of the stopped container otherwise (no restart; the Fly path starts
  a stopped slot to read it).
- Harness environment: the names in `[pool.harness].env` reach the container
  with their values taken from the environment of the `vivi` process at
  `create` time (`docker create -e NAME`; no value on a command line, none in
  the config or the mailspace). A name unset then is left out; `create` warns
  and `doctor` lists them. **The values then live in the container's
  configuration on that daemon** (`docker inspect` shows them to anyone with
  access to it). To change one, destroy and re-create the slots. The one-time
  Git fetch token is not in the container's configuration: it is a file the
  boot script reads and deletes.
- Architecture: the daemon may be arm64 or amd64, and an image may run emulated.
  Vivi assumes neither; each slot records its architecture (`status` shows
  `arch arm64`, `status --json` has `arch`, `doctor` lists it). **`setup`
  commands must not hard-code an architecture** (download URLs, `x86_64`
  packages); read `uname -m` or use the package manager.
- Differences from Fly: no per-second billing (a busy slot costs your own
  hardware, not money); all slots share the daemon's cores and memory, so a
  full pool of heavy builds competes (size `memory`, `cpus` and `slots` to the
  daemon; `doctor` warns when one slot's `memory` exceeds the daemon's); no
  provider secret store; the pool name is the only namespace on a daemon, so
  two mailspaces with the same pool name on one daemon share slots (set
  distinct `[pool].name`s). The image needs `bash`; the boot script installs
  `git`, `zstd` and `curl` with `apt-get` when they are missing, which needs
  network. With those tools already in the image, the whole pool, `doctor
  --test` included, runs offline.
- Slot ids are the first twelve hex digits of the container id; slot names are
  the container names (`<pool>-slot-<n>`). `destroy` removes the containers,
  their claims and anonymous volumes.

### Using the pool (orchestrator procedure)

1. `vivi pool doctor --porcelain`; fix what it names.
2. Push the base commit or branch to the remote.
3. `vivi pool dispatch --porcelain <task> --model <slug> [--from <ref>] [--branch <name>]`.
4. Run `vivi pool wait --porcelain <task>` in the background.
5. When it returns, read the receipt with `vivi task show <task>`.
6. Integrate the pushed branch.
7. Delete the remote branch.
8. Close the task (`wait` and `reconcile` never do).

### dispatch

`vivi pool dispatch <task> --model <slug> [--from <ref>] [--branch <name>]
[--budget SECS]` hands an open task to a free slot and returns at once.

- The brief is the task body, read from the mailspace. `--model` is required,
  has no default, and must be one of `[pool.harness].models`; it is checked
  before any machine is touched.
- `from` is what to start from (a commit or a branch); `branch` is where to
  push. Each comes from its flag, else from a `from: <ref>` / `branch: <name>`
  line in the task body; both are required one way or the other. One rule covers
  three cases: a new branch (`from` = commit), continuing a branch (`from` =
  `branch`), and stacking (`from` = one branch, `branch` = another). The push
  is never forced: if the branch moved or holds other work, the result is
  `push_refused`.
- `--budget` is the agent's hard time limit in seconds (default 1800). Every job
  also runs under a hard limit enforced by the slot's boot script (budget plus
  ten minutes of slack).
- Steps: mint a one-hour token; resolve `from` through GitHub; claim a free
  slot (the provider's atomic primitive serialises the check-and-mark: a Fly
  machine lease, or a claim container on Docker; two simultaneous dispatches
  get different slots and a third finds none); deliver the job as one
  archive in the machine's config (Fly) or copied into the container (Docker);
  start it; record a `pool_dispatched` event
  and a reply on the task. A failure after the claim undoes it.
- The push target must not be the repository's default branch (resolved through
  GitHub); `--allow-default-branch` overrides that.
- A task with an open dispatch cannot be dispatched again until it is collected.
  After collection it can be dispatched again (for example to continue its
  branch).

### wait and reconcile

- `vivi pool wait [<task>...] [--timeout SECS]` blocks until at least one open
  dispatch (the named ones, or all) has finished, collects each finished one,
  prints one line per task (`<task>  <state>  <slot>  <detail>`; with `--porcelain` the `collected` line above), and exits
  0. On timeout (default 3600) it exits nonzero and says nothing finished.
  Run it in the background to be woken.
- `vivi pool reconcile` handles every open dispatch once: finished ones are
  collected and their slots freed; one still running past its budget plus a
  15-minute grace is stopped and recorded as a timeout; one whose machine
  stopped without a result, or no longer exists at all, is recorded as lost
  (and the task can be dispatched again); one still within budget is
  reported `running`. It also frees stale claims (a held slot with no open
  dispatch, claimed more than five minutes ago).
- Collecting reads the result and the agent's final output from the slot (a
  stopped slot is started, read, and stopped), records a `pool_collected` event
  and a reply on the task (push outcome, exit code, durations, the receipt, log
  tails), then stops the machine and releases the slot. Recording comes before
  freeing, so a failed record leaves the result in place. Neither command
  closes or completes the task; the orchestrator does that. A slot is only
  freed after a fresh read shows it `stopped`; one that will not stop keeps its
  claim and `reconcile` retries.
- Provider failures are visible: while `wait` runs, a poll error is printed to
  stderr when it first appears or changes (`warning: cannot poll ...`) and
  `recovered: ...` when it clears; the final timeout message names the last
  one. The Fly token is re-fetched every minute and after a refusal (a
  token from `fly auth token` stops working after about five minutes).
  `reconcile` also stops any running slot that no task holds and no dispatch
  explains. Read the receipt
  with `vivi task show <handle>`.
- Push outcomes: `pushed <branch> <sha>`, `push_refused`, `no_changes`,
  `fetch_failed`, `checkout_failed`, `commit_failed`. A result with
  `state=killed` means the slot's hard limit stopped the runner.

### On the slot

A generic runner, embedded in the binary and delivered with each job, returns
the checkout to a clean tree at `from`, runs the harness as the unprivileged
user `hand` with a clean environment (only `HOME`, `PATH`, and the names in
`[pool.harness].env`; the Git token is never among them and is readable only by
root), then commits what changed as the configured bot identity, with a
`Pool-Run:` trailer recording slot, model, start ref, exit code, and duration,
and pushes without force. The harness's standard output is the receipt;
standard output and error are kept as files. A finished job lingers for two
minutes so `wait` can collect without a restart; a boot with nothing to run
stops itself after 60 seconds; the job directory survives a
stop and start until the next dispatch replaces it.

Config is a `[pool]` table in `.vivi/mailspace.toml`. Everything provider
neutral sits in `[pool]`; the Fly settings sit in `[pool.fly]` and the Docker
settings in `[pool.docker]`. A Fly key left directly in `[pool]` (`app`,
`organization`, `region`, `size`, `memory`, `tmpfs`) is rejected with a message
naming `[pool.fly]`. Only the table of the chosen `provider` is used and
checked beyond its key names.

```toml
[pool]
# provider = "fly"            # default; or "docker"
repo = "owner/name"           # required
slots = 4                     # required; maximum pool size
image = "rust:1.97-bookworm"  # required; Debian-based (needs apt for zstd/git)
setup = ["npm install -g some-harness"]   # root, once, before the checkout
seed = ["cargo build"]                    # in the checkout, once, warms build_dir
build_dir = "target"          # required; relative to the repo
# name = "lanes"              # defaults to the mailspace name; marks the machines

[pool.fly]                    # provider-specific; every key optional
# app = "my-fly-app"          # default: the pool name; created by `pool create` if missing; a taken name is for you to change
# organization = "personal"   # owner of a newly created app; default personal
# region = "iad"              # default iad
# size = "performance-8x"     # default
# memory = 32768              # MB; default 32768
# tmpfs = "16g"               # memory disk over build_dir; default 16g

[pool.docker]                 # only with provider = "docker"; every key optional
# host = "ssh://user@server"  # a remote daemon; omit for the local one
# memory = "4g"               # per-slot limit (includes the memory disk); default 4g
# cpus = 4                    # per-slot CPU limit; default none
# tmpfs = "2g"                # memory disk over build_dir; default 2g

[pool.github]
app_id = 123456
private_key = "~/secrets/app.private-key.pem"

[pool.harness]                # required to dispatch
name = "pi"
run = "pi -p --model {model} --append-system-prompt {rules} @{brief}"
models = ["zai/glm-5.3-flash"]            # --model must be one of these
env = ["MODEL_API_KEY"]       # names only; values come from the machine's environment
# rules = "docs/worker-rules.md"          # default: a short embedded rules file

[pool.committer]              # required to dispatch
name = "lane-bot"
email = "lane-bot@users.noreply.github.com"
```

`run` is a shell command; `{model}`, `{brief}` and `{rules}` are the only
placeholders (the brief and rules are files inside the slot). Its standard
output is the receipt. Use a print mode that writes only the final answer.
Validation runs when a `vivi pool` command starts, and rejects misspelled keys.
The config holds a key path and variable names, never a key or a value.

## Watches and Cycle Intake

Project-local watch commands observe mailspace events. Prefer `--once` for
fail-fast cycles or an explicit timeout for waiting. Use cursor or watermark
files when repeated cycles must not replay old events. Add graph event filters
only when node readiness or state changes are relevant.

`cycle intake` collects pending tasks, needs, mail, and recent memos for one
identity. It is a bounded intake surface, not a replacement for inspecting the
specific handle selected for work.

## Chat

`vivi chat` is the cross-checkout conversation surface: two vivi
mailspaces on different machines join one persistent room on a chat
server and exchange messages through the CLI alone. The server is a
separate repository (`vivi-server`); its wire truth is
`docs/contract.md` in that repo. Verify the exact flags with
`vivi chat --help` before relying on them.

```sh
vivi chat room create [--seats N]   # mint a room id; creator takes seat 1
vivi chat join <room> [--label ...]  # challenge -> sign -> seat binding
vivi chat rooms                     # rooms joined from this mailspace
vivi chat send <room> [--to <label>] [--subject <s>] (--body <b> | --body-file <f>)
                    [--kind need|task|want] [--reply-to <handle|id>] [--ref <handle|id>]...
vivi chat read [room]               # messages since the cursor, with origins
vivi chat watch [room]              # long-poll and print arrivals
vivi chat flush                     # deliver pending outbox messages
vivi chat pull [room]               # file room items into the mailspace
vivi chat resync [room]             # replay from cursor 0; restores pruned items
vivi chat prune --older-than 30d [--dry-run] [room]   # evict old cached items
vivi chat leave <room>              # local unjoin; the server seat stays
vivi chat status                    # fingerprint, server, rooms, outbox depth
```

Configuration is the `[chat]` table in the project's `mailspace.toml`:

```toml
[chat]
server  = "https://vivi-server-production.up.railway.app"
notify  = true   # board may show the unread-chat badge (default)
name    = "pharos/vivi"  # seat label override; default <short-host>/<project>
recipient = "mind"  # local role that receives room items on `chat pull`
```

Identity is transparent: one ED25519 keypair per mailspace, minted on
first use at `.vivi/chat/identity.ed25519` (0600). The chat directory
writes its own `.gitignore`, so keys, cursors, and outbox state are never
committed. The key fingerprint is shown at join and read time; treat it
as the sender's durable identity.

Offline semantics are honest: `send` enqueues transactionally and then
attempts immediate delivery, printing `sent` or `queued` for what
actually happened. Every chat command flushes opportunistically first;
`vivi chat flush` retries explicitly. Delivery is at-least-once — each
send mints one traveling id (`msg_` plus 24 hex characters) at creation
and every retry re-sends that same id; the server deduplicates by it, so
a replayed flush is a no-op rather than a duplicate message.

`--kind need|task|want` sends a work item instead of plain chat: it files
into the recipient's `needs`/`tasks`/`wants` when they pull. `--reply-to`
and `--ref` thread it (a receipt is just a reply to the original) and
accept a local handle or a full `msg_` id; a full id is passed through
even when this mailspace has not seen it. Send prints `handle <8 hex>` —
the same handle the recipient sees, so cite it in the body freely. The
send is validated against the server's limits (kind, at most 32 refs, ids
of 1-256 bytes) before it is queued, so a bad send fails at the prompt
instead of sitting in the outbox. A queued work item retries with its
kind and threading intact, under the same id.

`vivi chat pull` is the down-sync: it polls each room since its own sync
cursor and files what arrived into the mailspace as real mail items for
the `[chat] recipient` role (required — pull refuses without it, and chat
stays print-only until it is set). Needs, tasks, and wants land in the
`needs`/`tasks`/`wants` folders with a backlog node like a locally sent
item; plain chat lands in `inbox`; the seat's own sends come back as
`sent` copies. Each item keeps its traveling id (`msg_` plus 24 hex), so
the short handle is the same on every machine and a citation like
"supersedes `abcd1234`" resolves wherever the item exists; a reference to
an item this mailspace has not seen stays unresolved rather than failing.
Pull is idempotent and never resets local state: a need you closed stays
closed when the room replays. The server shows every seat the whole room,
so items addressed to another seat are skipped and counted. Ids from
before phase 02 are not traveling ids; those items file under a
store-local id and the summary says so. The sync cursor is separate from
the read cursor, so `chat read --mark-read` never makes pull skip items.

A room is the durable log and the mailspace holds a replica, so the cache
can be shed and rebuilt. `vivi chat prune --older-than <age>` (`30d`,
`12h`, `90m`) evicts room items dated before the cutoff and drops their
blobs; `--dry-run` previews the same selection. The default is keep-all:
nothing ever prunes on its own. Prune never touches open needs, tasks, or
wants (however old), starred or absorbed items, or local-origin mail, and
it needs neither the server nor `recipient`. `vivi chat resync` replays the
room from cursor zero through the same ingest as `pull`: evicted items come
back byte-identically under the same ids, and local state (a need you
closed, a read flag) is left exactly as it was. Resync is the only thing
that restores; a plain `pull` resumes at its cursor. If an id the room
sends is already held here with different content, that one item is
refused and named on stderr, and the pull carries on.

The board badge is one bounded line appended to `vivi board` when rooms
are joined and the server is reachable (`chat: N unread in <room>` or
`chat: no new messages`); an unreachable server prints nothing, and
`notify = false` suppresses the badge entirely.

Trust posture is the GitHub-issue model: the protocol labels origin —
seat label plus key fingerprint, displayed on join and read — and
nothing more. Judging, verifying, and scheduling incoming messages is
the receiving Mind's job. No protocol gating; verify fingerprints out
of band when a new sender matters.

`[chat] server` defaults to the placeholder
`https://vivi-chat.example` until the operator sets a hostname. The
current live deployment is
`https://vivi-server-production.up.railway.app` (Railway default
domain); a custom hostname is still open.

## Email

For IMAP-backed email, sync before claiming current state when freshness
matters. Use `list`, `search`, `show`, and `thread` for bounded inspection.
Keyword search is the default; semantic or hybrid search requires a healthy
embedding index. Rebuild indexes through Vivi rather than editing storage.

Distinguish local preparation from remote effects:

| Surface | Effect |
|---|---|
| `compose` / `reply` | Create a local draft; do not send |
| `enqueue` | Store a proposed remote action for later review |
| `queue run` | Execute queued remote actions |
| `exec` | Execute a remote write immediately |

Remote writes include sending, archiving, deleting, moving, flagging, and
running queued actions. They require authority appropriate to the user's
request. Never run `vivi exec send` without explicit approval for that send.
Prefer queue-first when work is agent-prepared, uncertain, or awaiting review.

For a non-trivial message:

1. Create the local draft.
2. Inspect the generated `.eml`, including recipients, subject, plain text,
   HTML, threading headers, and attachments.
3. Revise locally if needed.
4. Send only after explicit approval.

Use `password_cmd` or the configured secret mechanism. Never print credentials
or place passwords in shell history.

## Safety

- Use the CLI rather than underlying stores.
- Verify current help before exact mutations.
- Narrow reads before dumps and broad searches.
- Treat absorb as permanent.
- Treat `queue run` as a remote mutation, not a review action.
- Do not emulate unsupported labels with folder moves.
- Do not infer a source repository or account when the user has not identified
  it.
- Report whether an operation was local, queued, or executed remotely.
- `vivi pool dispatch` launches an agent that can push to the repository; read
  the branch it was given before dispatching.
- `vivi pool destroy` is permanent: it removes the slots and their seeded disks.
- A busy Fly slot bills by the second, and a leaked running slot is a cost
  (on Docker it holds the daemon's cores and memory); run `vivi pool doctor`
  when anything looks stuck.
- `vivi pool doctor` is read-only; `vivi pool reconcile` is the command that
  acts. `vivi pool doctor --test` is the exception: it creates and destroys
  machines of its own (`<pool>-doctor`; billable on Fly, free containers on
  Docker), and never touches the real pool's slots.
