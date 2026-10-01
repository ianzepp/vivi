---
name: vivi
description: Use the Vivi CLI for project mailspaces, roles, goals, work graphs, local email search and drafts, queues, and explicitly authorized email mutations.
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
open handles with verdicts, registered goals with register tallies, memos,
charter heads, and the backlog sliced into seat-sized groups. It is read-only,
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
promoted, dropped, reprioritized, or deleted. A later reply or derived task is
a new record. Absorb only when the identity is finished with the item and its
state should remain frozen.

Memos preserve durable role context. They are not routing, assignment, or
completion evidence. Delete superseded unsealed memos; absorb memos that should
be retained as immutable history.

When a mailspace has a configured archive, absorption exports one Markdown
record to that dedicated Git repository. Vivi does not stage or commit the
archive. Use the archive commands and `--help`; do not write exported files by
hand.

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

## Watches and Cycle Intake

Project-local watch commands observe mailspace events. Prefer `--once` for
fail-fast cycles or an explicit timeout for waiting. Use cursor or watermark
files when repeated cycles must not replay old events. Add graph event filters
only when node readiness or state changes are relevant.

`cycle intake` collects pending tasks, needs, mail, and recent memos for one
identity. It is a bounded intake surface, not a replacement for inspecting the
specific handle selected for work.

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
