---
name: vivi
description: Use the Vivi CLI for project mailspaces, roles, goals, work graphs, and the remote lane pool (`vivi pool`: machines or containers that run a task and push a branch).
---

# Vivi

Use `vivi` as the interface. Do not inspect or mutate `.vivi` databases, blob
trees, or indexes directly unless the user explicitly requests
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

Email (IMAP and SMTP, the local archive, search, drafts) is not part of this
CLI: it is the separate `vivi-mail` tool. `vivi mail` is project-local mail
only, with no external side effects.

## Embedded skills

This binary carries the `vivi`, `tugboat`, and `speedboat` skills. Their
text matches `vivi --version`. Prefer the embedded copy over a checkout of
`skills/` when the installed binary may be older or newer than that tree.

```sh
vivi skills list
vivi skills show vivi
vivi skills show vivi --file references/work-graph-flow.md
vivi skills install ~/.agents/skills
```

`show` without `--file` prints `SKILL.md` and lists every supporting file
(so relative links can be requested). `install` writes the same layout the
repository uses; `--force` overwrites. Skills do not need a mailspace.

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

Boot counts only this mailspace's own work. Work held for another
mailspace's role, and your own copies of work you handed to one, are not in
the totals or the handle list. Letters held for a role this mailspace lacks
get a `held for unknown roles` section with a count per name (see Project
Sync).

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
command = "scripts/boot-probe"   # executable, resolved against the mailspace root
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
the message and travel with it: whatever files a work letter (local send, pull,
fetch, resync, `pull --refs`, audit repair) builds the edges its headers name
for every dependency this mailspace holds as work, with or without `[sync]`. A
dependency not held yet waits and its edge is made when that letter arrives; a
reference that can never name work here blocks nothing. There is one dependency
substrate — the graph — for small and multi-unit relationships alike.

Closing a task, need, or want records its current disposition, as a letter on
the item (see State as letters). Reopen when the CLI permits and evidence
changes. `want promote` moves deferred work into the
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
superseded unsealed memos (`memo delete` refuses a handle that is not a
memo); absorb memos that should be retained as immutable history.

When a mailspace has a configured archive, absorption exports one Markdown
record to that dedicated Git repository. Vivi does not stage or commit the
archive. Use the archive commands and `--help`; do not write exported files by
hand.

### State as letters

Closing and reopening are letters, not local flags. Three kinds of role may
close or reopen an item for everyone: the role that authored it (or holds its
`sent` copy), any role of kind `mind`, and any role of kind `operator`
(`role add <name> --kind operator`). Its
`done`, `reopen` and, on a want, `drop` (a close) write one small reply on the
item, with `X-Vivi-Event: done|reopened`, `X-Vivi-Event-Of: <item
Message-ID>` and `X-Vivi-Event-Prev: <the event it follows>` (absent for the
first) beside the usual `In-Reply-To`. It is addressed to the item's To and Cc
minus the sender and filed read (it still shows in `mail list` and boot's
unabsorbed mail as `Re: <subject>`); `--note` is its body. Every holder of the
item applies it: the folder moves, the node completes or reopens, and the
assignee's queue empties when the owner closes. A need that closes or reopens
through `need bind` writes the same letter when this mailspace authored it.

Any handle of the item works for a role that may close it. A mind can name the
delivered copy a hand holds, its own copy, or the author's `sent` copy; give
`--for <your identity>` and the event lands on the item, not on the row named.
The command prints `done|reopened|dropped <handle>; event <id>`. A role of any
other kind (hand, auditor, head, ...) reports instead and prints `done
<handle>; reported` (or `reopened`): its own copy moves and the `--note` is a
reply to the author. A report is not an acceptance. It completes no node,
unlocks no dependents, fires no join and writes no event: it never changes the
item's `State:` or any other holder's copy, and the author's tracking node
stays `awaiting` until the author closes. The author, the `sent`-copy holder,
or a role of kind `mind` or kind `operator` accepts it by running `done` (any
handle of the item
works); that completes the node, unlocks dependents and fires a bound need's
join (see Work Graphs). Naming a copy another role holds is refused, and the
error names the holder, the report command to retry with `--for <holder>`, and
who may close. A reporter may reopen its own report; it cannot reopen what the
owner closed (`only the owner can reopen it`; ask in a reply). An event counts
only in the item's own mailspace, so a mind in an assignee's mailspace reports
on an item another mailspace authored: a local report to the author that does
not close it there, though it accepts the item in this mailspace (the node
completes here).

The state of an item is its chain head: the event no other held event names as
`Prev`. A fork (two checkouts acted on the same event) leaves several heads and
the later `(Date, Message-ID)` wins on every checkout; the next event names all
heads. An event counts only when its From domain is the item's From domain.
Anything else is filed as plain mail and counted (`N event(s) ignored: not from
the item's owner` in the pull summary), as is an event for an absorbed item.

This is local-first. It works with no `[sync]`; the server only carries
letters. `pull`, `fetch`, `resync` and `pull --refs` apply every event they
see (`N event(s) applied` in the pull summary), and an event that arrives
before its item is applied when the item arrives. `graph audit` reports a
folder that disagrees with its item's latest event as `event_state`, and
`--repair` re-applies it. `need|task|want show` prints `State: done|reopened`
once an event exists.

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
`want done|drop` run by an acceptor (the author, the `sent`-copy holder, or a
role of kind `mind` or kind `operator`) complete the item's node and unlock
dependents; a
report by any other role completes nothing (State as letters); `reopen`
re-locks them (cascading through join parents and content siblings);
`want promote` never changes node state and wants never dispatch in
`vivi step` before explicit promotion. Lowering is a graph fact:
`vivi need bind <need> <task>...` binds unit tasks to a need, and the need
auto-completes when every bound unit is accepted (a report does not count) —
node and mailbox item together (`via=join` in the event log); the accepting
`done` prints `need <handle> completed (all bound units accepted)` when it
closes the need, and a `done` that fires no join prints no such line.
Reopening a bound unit restores the need to open the same way.
`vivi need show <handle>` prints the bound units with their states. Imported
Mermaid graphs are for non-item topology (pipelines, environment flows);
delivery lowering uses `need bind` in the backlog, so work is never
double-represented.

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
holder, the `node <handle>` line names that copy as the backlog node, and
naming a copy another identity holds fails with the holder
named in the error (the item's author, a mind or an operator may name any
copy; see State as letters). **Settle sequence.** The worker reports with
`task done --verdict/--repo/--tip` (its own copy moves; the node stays open
or active as `reported`, dependents stay blocked). The author, the
`sent`-copy holder, or a mind or an operator accepts with `task done <handle>
--for <acceptor>`: the node completes, dependents unlock, a bound need's join
fires, and a `step_decision via=lifecycle` event records the transition. The
host then runs `vivi step --apply <handle>` — idempotent, never settles
itself — which runs the receipt screen when a judgment provider is
configured and records `via=step-apply` only when the node was still open.

Use `graph show`, `graph ready`, and `board --graph` for inspection. `graph
ready` prints a `counts` line first (`ready`, `blocked`, `active`, `awaiting`,
`reported`, `gates`; `--json` has the same lists as fields) — use it (or
`--kind task|need|want|decision|stub|parked`) instead of reading long id
lists. `reported` lists open or active work a role reported done and nobody
who may accept has: it is never ready, its dependents stay blocked, `step`
reports it as `awaiting_acceptance` (who reported, and the `done` command that
accepts it), and `graph activate` refuses it; an active node that was reported
shows under `active` as well. `awaiting` lists open work this mailspace handed
to another one: it is tracked here so a sender can order it with
`--depends-on` and see it with `need|task|want list|dump --for <sender>` (the
role column reads `sent`), but it is never ready here, its dependents stay
blocked, `step` reports it as `awaiting_remote` naming the recipient, and
`graph activate` refuses it. The receiving mailspace dispatches it; the
author's (or a mind's) `done` on your copy settles the node. `board --graph`
shows no `awaiting` or `reported` row. Work held for another mailspace's role
(see Project Sync) is not this mailspace's backlog: `step`, `graph ready` and
`boot` leave it out; `mail list --to <address> --folder needs|tasks|wants`
shows it. Use `vivi step [--json]` for a mechanical adjudication of the
backlog into `dispatches` (ready, verifiable work with clause counts) and
`exceptions` (reason vocabulary: `want_requires_promotion`,
`lowered_awaiting_units`, `no_done_when`, `item_missing`, `not_settled`,
`untracked_item`, `awaiting_remote`, `awaiting_acceptance`). Wants render as
parked in their exception detail. A clauseless task/need body warns at send
time, and a duplicate work send (same sender, same subject, still open in the
recipient's folder) warns on stderr without blocking. `vivi step --apply
<handle>` completes an already-settled item's node and lists transitions under
`decisions`; applying an item the lifecycle already settled records
`via=lifecycle (already settled; nothing to apply)` instead of silence.
Applying a reported item completes nothing and returns its
`awaiting_acceptance` exception. The coordination host decides which ready
nodes to dispatch.

**Citizenship audit.** `vivi graph audit [--repair]` checks the backlog
invariant — every work item sent while the graph existed has a node in step
with its folder state — and reports `missing_node`, `node_state`, `node_kind`,
`missing_edge`, `foreign_held`, `event_state`, and `orphan_node` findings,
with a one-line count per class. `missing_edge` is a held open work letter
whose `X-Vivi-Depends-On` header names held work the graph has no edge from.
`foreign_held` is a node still open for work held under another mailspace's
role. `event_state` is a folder that disagrees with its item's latest event
letter. A `done` folder under an open or active node is a report awaiting
acceptance, not `node_state` drift: audit neither reports nor repairs it, and
`graph complete` is the manual override. `--repair` mints missing nodes and
edges (from folder state and dependency headers), completes nodes for settled
items, settles need mailboxes whose join fired, settles `foreign_held` nodes,
re-applies the latest event for `event_state`, and corrects kinds.
`orphan_node` is reported, never repaired. `graph activate` / `need bind`
errors point here. Stale vivi binaries silently skip node minting entirely
(observed with an old Homebrew 8.1.0 on PATH): `mailspace status` prints the
running binary's version — check it when nodes go missing, and keep one vivi
on PATH.

### Work-graph flow (dispatching ordered work)

The procedure for turning a delivery document into ordered, dispatched work
lives in [`references/work-graph-flow.md`](references/work-graph-flow.md): read
it before lowering a multi-unit delivery. The load-bearing rules:

- Every unit is its own task send with its own handle; the send mints the node.
  Cite prerequisites with `--depends-on`; an unknown handle fails the send.
- A send prints `created <recipient> <handle>`, `node <handle>` and
  `sent <handle>`: the delivered handle (the `created` / `node` one) is the
  backlog node — the handle to `pool dispatch`, `need bind`, `graph activate`
  and `task done` — while the `sent` handle is the sender's own copy and is
  not a node. (When every recipient is in another mailspace there is no
  delivered copy and the node lives on the sent copy.)
- `done` by the worker is a report; the author's or a mind's `done` accepts
  it. Neither means the code is on main, and Vivi has no merged state. "On
  main" is a **merge task** filed with `--depends-on <unit>`; anything that
  needs the code `--depends-on` the merge task, not the unit.
- `vivi step` dispatches a ready node only if a line of its body begins with
  `done_when:` (a `write_scope:` line is reported, not required). One-line
  compressed forms, bullets, and bold labels do not match.
- Per unit, in order: file with `--depends-on` and a valid body, `need bind`
  the need to its units (and their merge tasks), claim the lane, `graph
  activate <handle> --task <handle>`, spawn the seat, and when it reports
  accept it (`task done <handle> --for <mind>`), then run `step --apply
  <handle>`. A merge landing is accepted the same way and turns the merge
  task's dependents ready.
- Do not stack units on one handle: stacked units are not separate nodes and
  one handle holds one lane lock.
- `graph audit --repair` fixes missing nodes and edges, kind drift, node/folder
  state drift (folder wins; a report awaiting acceptance is not drift),
  `foreign_held` nodes and `event_state` drift; read the audit first, because
  `node_state` repairs can reverse a manual `graph complete`.
  The reference file lists exactly what each finding repairs.

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
key_cmd  = "cat /path/to/judgment.key"
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
Vivi task to one of them. A mailspace holds any number of **named pools**
(`[pools.<name>]` in `.vivi/mailspace.toml`); each is complete on its own.
Five machine hosts exist, chosen by each pool's `provider`: `fly` (the default; Fly Machines, billed by the second),
`docker` (containers on a Docker daemon: the local one, or a remote one over
SSH; see "Docker provider" below) and `ssh` (a plain Linux host reached over
SSH, each slot a directory, no isolation; a minimal form, not the recommended
one; see "SSH provider" below) and `local` (this machine: each slot a
directory, the worker a process Vivi launches as you, no isolation, optionally
a Git worktree of a local repository; see "Local provider" below) and `runpod`
(rented GPU pods, one short-lived pod per dispatch and no resting slots; see
"RunPod provider" below).
`[pools.<name>.fly]`, `[pools.<name>.docker]`, `[pools.<name>.ssh]` and
`[pools.<name>.local]` and `[pools.<name>.runpod]` hold the host-specific
settings. Everything else in this section works the same on all five, except
that a `runpod` pool has no resting slots (`create` and scale-up do not apply). In-place reseed, other hosts,
routing work to a pool by role, a default pool, and shared defaults between
pools do not exist; do not invent them. Which work goes to which pool is the
orchestrator's call; Vivi has no opinion.

```sh
vivi pool create --pool <name> [--slots N] [--base <commit|branch>] [--timeout SECS] [--porcelain]
vivi pool scale --pool <name> <N> [--yes] [--base <commit|branch>] [--timeout SECS] [--porcelain]
vivi pool status [--pool <name>] [--json | --porcelain]
vivi pool destroy --pool <name> (--slot <name-or-id> | --all) --yes
vivi pool dispatch --pool <name> <task> [--model <slug>] [--from <ref>] [--branch <name>] [--budget SECS] [--allow-default-branch] [--ready-timeout SECS] [--porcelain]   # --ready-timeout: RunPod pools only
vivi pool dispatch --pool <name> <task> [--model <slug>] --read-only [--from <ref>] [--budget SECS] [--porcelain]
vivi pool wait [--pool <name>] [<task>...] [--timeout SECS] [--porcelain]
vivi pool reconcile [--pool <name>] [--porcelain]
vivi pool doctor [--pool <name>] [--json | --porcelain]
vivi pool doctor --pool <name> --test [--long] [--allow-spend] [--json | --porcelain]
```

#### Named pools and `--pool`

- A pool is a `[pools.<name>]` table. The name is the table key: lowercase
  letters, digits and dashes, starting and ending with a letter or digit, and
  not ending in `-doctor` (the self-test pool is `<pool>-doctor`). There is no
  `name` setting. The old top-level `[pool]` table is gone (a clean break):
  a mailspace that still has one gets an error naming `[pools.<name>]` on the
  first `vivi pool` command; nothing is rewritten.
- Every pool table is complete on its own. There are no defaults shared between
  pools and no inheritance: write `repo`, `github`, `harness` and `committer`
  again in each pool that needs them.
- `--pool <name>` is on every pool command. With exactly one pool it may be
  omitted everywhere. With several: `create`, `destroy`, `dispatch`, `scale` and
  `doctor --test` require it (the error lists the pool names); `status`,
  `doctor` (without `--test`), `wait` and `reconcile` cover **all** pools when it
  is not given and one pool when it is. An unknown name lists the known ones.
- Each dispatch record carries its pool, so `wait` and `reconcile` find the right
  host. A record whose pool is no longer in the config is reported (`wait`
  prints an issue and leaves it open) and `reconcile` closes it as `lost`; so is
  a record made before pools were named.
- **`scale --pool <name> <N>`** brings a pool to N slots. Up: creates and seeds
  the missing ones (like `create --slots N`; it needs the pool's repository, or
  `worktree_of` on a local pool).
  Down: destroys **free** slots only, highest index first, and requires `--yes`;
  a busy, seeding, finished-uncollected or unseeded slot is never destroyed and
  is printed as `skipped` with its state, so the pool may end above N (run it
  again once they are free; the exit code is still 0). N above the pool's
  configured `slots` is refused. `scale 0` empties the pool of free slots.
- **Slot identity belongs to the mailspace and the pool.** Marks and labels
  carry both the mailspace's slug and the pool name, and discovery, claim,
  status, destroy and the self-test filter on both: two mailspaces may each have
  a pool called `local` on one Docker daemon, SSH host or Fly app without seeing
  each other's slots. Names: `<mailspace>-<pool>-slot-<n>` (Fly machine,
  Docker container) and `<root>/<mailspace>/<pool>/slot-<n>/` (SSH and local; the slot id
  is `<mailspace>/<pool>/slot-<n>`). The mailspace slug is the name lowercased
  with every other character turned into a dash. The SSH and local destroy
  guards check the pool mark and the mailspace mark.
- **`build_dir` is optional and has no default.** A pool without one has no
  memory disk, no pack or restore, and the runner's clean names no directory.
  The clean is `git clean -fd` (no `-x`) on every provider: it deletes
  untracked files that are not gitignored and leaves gitignored ones alone, so
  a normal `target/` survives between jobs with or without `build_dir`. With
  `build_dir` on a pool whose build directory persists (ssh, local, runpod) the
  clean additionally names it (`-e /$BUILD_DIR`), so it survives even when not
  gitignored; on fly and docker it is a memory disk packed at seed and
  restored before every job instead. Fly and Docker work with or without it.
- **A pool with only provider settings.** `repo`, `github`, `harness` and
  `committer` are optional in a pool table (`github` goes with `repo`). Such a
  pool is for testing the provider: `status`, `destroy`, `reconcile`, `wait`,
  `doctor` and `doctor --test` work (`doctor` prints one `skipped` Repository
  line, "no repository configured for this pool"). `create`, `scale` (when it
  would create) and `dispatch` fail naming the missing setting, and `dispatch`
  also needs `harness` and `committer`.
- **The MCP bridge is a per-pool table too.** `[pools.<name>.bridge] enabled
  = true` has every dispatch on the pool mint the job one project API key,
  delivered only inside the job archive, so the slot reads its task from the
  server and reports through `vivi mcp` as the minting member; the key is
  revoked at collect and `pool destroy --all` sweeps the pool's leftovers.
  See "MCP bridge" under Project Sync.

#### Fetch depth

A pool fetches shallow and never the full history. The default fetch depth is
50: at create the slot's seed fetch is `git fetch --depth 50`, and at dispatch
the job's fetch of its `from` ref is `--depth 50`. A `depth` setting in the
`[pools.<name>]` table (1 to 100000) overrides the default for both. A
worktree pool (`local.worktree_of`) fetches nothing, so `depth` has no effect
there.

#### Restoring regenerated files

A build that rewrites a tracked file (a lock file regenerated by a build
tool, say) would otherwise ride into every job's commit and conflict at
merge. A `restore` setting in the `[pools.<name>]` table lists
repository-relative file paths (at most 100; each non-empty, no leading `/`,
no `..` component): after the agent ends, the runner puts exactly those
tracked files back to the commit the job started from, before it commits.
Nothing else is restored: a path that is not tracked at the start, or that
did not change, is left alone, and the list is the pool's, never a guess.
The setting applies on every provider, worktree pools included; a read-only
job publishes nothing, so nothing applies there. When a restore happened the
task's result names the paths (`restored N path(s) before commit: ...`; in
the `pool_collected` JSON the array `restored`).

#### Read-only dispatch (reviewers and auditors)

`vivi pool dispatch --pool <name> <task> [--model <slug>] --read-only [--from <ref>]
[--budget SECS]` runs a job that publishes nothing, so a review role can have a
full shell (git, builds, tests) safely. `--from` (or a `from:` body line) is
required; `--branch` and `--allow-default-branch` are refused with it, and a
`branch:` body line is ignored (the human output says so). Per-dispatch flag
only; no config setting.

- The GitHub token is minted with `contents: read`: the slot cannot push.
- The runner checks `from` out detached (no branch made or moved), runs the
  harness as usual, then never commits and never pushes. If the tree differs
  from `from` (tracked changes, commits, untracked files; not the build
  directory) it records the paths (up to 50; the count is exact) and restores
  the tree. Changes are visible, not a failure.
- Outcomes (the `DETAIL` of `collected`): `read_only_unchanged`, and
  `"read_only_discarded COUNT"` (quoted: it holds a space). They never reuse
  `pushed`, `committed`, `no_changes` or `push_refused`. The paths and the
  receipt are on the task's result record (`task show` / the thread); in the
  `pool_collected` JSON they are the array `discarded`.
- Porcelain: `dispatched TASK POOL SLOT JOB MODEL FROM START - BUDGET read-only ABANDONED COST`
  (BRANCH is `-`; MODE is `write` otherwise); `slot ... CREATED MODE` shows
  `read-only` for a busy slot with a read-only dispatch (`status --json`:
  `read_only`).
- Guarantee by provider: Docker and Fly are the strong form (confined worker,
  no credentials). SSH and local fetch mode: the runner publishes nothing and
  the token cannot push, but the worker is a real user account with its own
  access that Vivi cannot stop; a one-line warning goes to standard error. Local
  worktree mode: no token and no push, no commit and no branch from the runner,
  but the worker shares the repository and could commit with plain Git; the
  dispatch warns of that.

Example: a small local pool for everyday work and a larger rented pool to scale
up for a big push and back down afterwards.

```toml
[pools.local]
provider = "docker"
repo = "owner/name"
slots = 4
image = "rust:1.97-bookworm"
build_dir = "target"
seed = ["cargo build"]
[pools.local.docker]
memory = "8g"
[pools.local.github]
app_id = 123456
private_key = "~/secrets/app.private-key.pem"
[pools.local.harness]
run = "pi -p --model {model} @{brief}"
models = ["provider/model"]
env = ["MODEL_API_KEY"]
[pools.local.committer]
name = "lane-bot"
email = "lane-bot@users.noreply.github.com"

[pools.lanes]
provider = "fly"
repo = "owner/name"
slots = 32
image = "rust:1.97-bookworm"
build_dir = "target"
seed = ["cargo build"]
[pools.lanes.fly]
app = "my-unique-app"
# ...[pools.lanes.github], [pools.lanes.harness], [pools.lanes.committer] written out again
```

`vivi pool scale --pool lanes 32 --porcelain` before the push, then
`vivi pool scale --pool lanes 0 --yes --porcelain` after.

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
`\u{hex}` escapes; times are epoch seconds. `POOL` is the pool's name: the
first field after the line kind, except on the lines that lead with `TASK`
(`dispatched`, `collected`, `reconciled`), where it follows `TASK`.

```
create     home POOL TEXT                                          only when the app was made
           created POOL NAME ID STATE SECONDS EXIT OUTCOME ERROR   OUTCOME: seeded|timeout|failed|unmarked
           exists POOL COUNT                                       nothing to create
           issue POOL KIND SUBJECT MESSAGE FIRST CLEARED           KIND list-error, SUBJECT -
scale      created ... | home ... | exists ... | issue ...         as create
           destroyed POOL NAME ID
           skipped POOL NAME ID STATE TASK                         not free, so kept
           scaled POOL TARGET COUNT                                COUNT: slots the pool holds now
status     slot POOL NAME ID STATE TASK RUNNING BUDGET BASE ARCH CREATED MODE   MODE write|read-only, - with no open dispatch
dispatch   dispatched TASK POOL SLOT JOB MODEL FROM START BRANCH BUDGET MODE ABANDONED COST   BRANCH - when read-only; MODE write|read-only; ABANDONED COST: see below
wait       collected TASK POOL SLOT STATE DETAIL                   STATE: finished|timeout|lost
           nothing-open                                            already collected
           issue POOL KIND SUBJECT MESSAGE FIRST CLEARED           KIND poll-error, SUBJECT the task
reconcile  reconciled TASK POOL SLOT STATE DETAIL                  STATE: running|finished|timeout|lost|error|stale-claim|orphan-stopped
           nothing-open                                            no dispatch was open
doctor     check POOL STATUS GROUP NAME DETAIL FIX MILLIS          STATUS: ok|warn|fail|skipped
           summary POOL OK WARN FAIL SKIPPED                       one per pool
```

**Changed from the single-pool formats:** every line gained `POOL`. `home`,
`created`, `slot`, `issue`, `check` and `summary` carry it as the first field
after the kind; `dispatched`, `collected` and `reconciled` carry it right after
`TASK`; `exists` already led with the pool name. `POOL` is `-` only where it is
not known (a dispatch recorded before pools were named). `scale` adds
`destroyed`, `skipped` and `scaled`. Slot names (`NAME`, `SLOT`) now include the
mailspace. In `--json`, each slot of `status` and each check of `doctor` gained
`pool`. Human output names the pool on each slot or result line as well.

`ABANDONED` and `COST` are the two trailing fields of `dispatched`, always
present. `ABANDONED` is the number of pods a RunPod dispatch rented and
terminated because they never answered on SSH, before the one that worked (`0`
on every other host). `COST` is their approximate total charge in dollars
(`0.12`), or `-` when none were abandoned or no price was known. The first ten
fields are unchanged.

Examples: `collected 3f2a lanes ms-lanes-slot-2 finished "pushed work/x 0123456789"`,
`created local ms-local-slot-1 m1 stopped 42 0 seeded -`,
`scaled lanes 0 0`,
`issue lanes poll-error 3f2a "Fly unreachable" 1760000000 1760000090` (CLEARED `-`:
the problem lasted to the end). `wait` and `create` print their `issue` lines
even when they then fail, so a slow wait explains itself. Warnings (such as
missing harness environment) stay on standard error.

### doctor

`vivi pool doctor [--pool <name>] [--json]` checks every pool (or the one named),
each under headings that name it, and is read-only: it creates, starts, stops, marks and
pushes nothing, and repairs nothing. It prints one line per check (`ok`, `warn`,
`fail`, or `skipped` when an earlier check it depends on failed) under three
headings, so you can tell which side a problem is on, and each `warn` or `fail`
carries a `fix:` line naming the command that fixes it. Exit code is nonzero
when any check fails; warnings alone exit zero. `--json` gives the same data
(`ok`, `counts`, and `checks` with `pool`, `group`, `name`, `status`, `detail`, `fix`).

- **Provider (Fly)**: `fly` is installed and logged in, an API token can be
  obtained, the app exists, and every `harness.env` name is an app
  secret (names only; a name the image provides can be ignored).
- **Provider (RunPod)**: see "RunPod provider" below (read-only).
- **Provider (Docker)**: the `docker` binary runs; the daemon (local, or
  `[pools.<name>.docker].host`) answers, with its version and platform; the image is
  present or the registry has it (nothing is pulled by `doctor`); the daemon
  has at least as much memory as one slot may use (`warn` otherwise); every
  `harness.env` name is set in this process's environment (`warn` names
  the unset ones: slots created now would lack them).
- **Provider (SSH)**: the local `ssh` client runs; the host answers without a
  prompt (`BatchMode=yes`; a key in an agent, a known host key, a POSIX login
  shell); it is Linux and has `git`, `tar`, `setsid`, `timeout`, `sha256sum`,
  `ps` (a missing tool is a `fail` naming it; nothing is installed for you); the
  root exists and is writable or can be created under a writable directory;
  free disk space is reported (`warn` under 5 GiB); the architecture is
  reported; every `harness.env` name is set in the login environment of
  the host (`warn` names the unset ones). Nothing is created on the host.
- **Provider (local)**: this is macOS or Linux (`ok`, with a reminder that there
  is no isolation); `bash`, `git`, `tar`, `ps`, `df` and `date` are on PATH;
  `ps` lists processes (the slots' processes are found and ended with it); the
  root exists and is writable or can be created under a writable directory
  (`doctor` creates nothing); free disk space is reported (`warn` under 5 GiB);
  the source is reported: fetch mode, or worktree mode with the repository's
  path, worktree count, primary branch and default branch (`fail` when
  `worktree_of` is not a Git repository); every `harness.env` name is set in
  this process's environment (`warn` names the unset ones).
- **Repository (GitHub)**: `openssl` is present, the App key file is readable, a
  one-hour token mints for the repo (never printed), the default branch
  resolves, and whether it is protected (whether the App may bypass the
  protection is not checked).
- **Pool (Vivi)**: `[pools.<name>]` validates; slot count against `slots`; each slot's
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
  on the same host (the same Fly app, the same Docker daemon, the same SSH
  host and root, or the same local root), seed command
  `true`, no setup commands. It never touches the real pool's slots (nor another pool's or another mailspace's). **On Fly
  it creates billable machines for a few minutes** (about 3 minutes; `--long`
  adds about 7 more). **On Docker it costs nothing**: two containers on your
  daemon, removed afterwards, about 1.5 minutes with a local daemon and an
  image that is present (`--long` adds about 7 more, the job really sleeps past
  six minutes); a daemon reached over `ssh://` takes a little longer, one SSH
  round trip per call. On a local Docker daemon it is the free check to run after any
  change to the pool code, whichever host the real pool uses. **On SSH it
  makes two slot directories** under `<root>/<mailspace>/<pool>-doctor/` and removes them
  (with the root, if Vivi made it): about 1 minute (`--long` adds about 7). **On local it makes two slot directories** under
  `<root>/<mailspace>/<pool>-doctor/` on this machine, runs stand-in jobs there
  as you, and removes them: about 1 minute (`--long` adds about 7). A worktree
  pool's test runs in fetch mode with the embedded remote and never touches the
  configured repository. **On RunPod it rents a billable GPU pod and refuses
  unless `--allow-spend` is passed** (see "RunPod provider").
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
  once it has stopped. On SSH a running slot is read by `exec` and a stopped
  one by reading its files over SSH; no step differs. On local both are plain
  files in the slot directory. A step that cannot apply
  to a host is reported `skipped` with the reason, never `ok`; none is skipped
  today.
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

### Requirements and facts (special hardware)

For a pool on machines with something particular (a GPU, a toolchain, a
device) whose results are only evidence if they record what produced them. Two
optional lists of one-line shell commands in the pool table. Vivi knows nothing
about what they query.

```toml
[pools.gpu]
provider = "ssh"
slots = 1                       # one slot per device, so two jobs never share it
requires = ["command -v gpu-query", "gpu-query --list"]
facts    = ["gpu-query --name --driver --memory --csv", "uname -srm"]
```

- `requires` runs at **seed** time, in the checkout, after `setup` and before
  `seed`, **as the worker's user** (`hand` on Fly and Docker, the login user on
  SSH and local), two-minute limit each. A failing command **fails the seed**:
  `create`/`scale` print `exit 7 (a `requires` command failed)` plus, on hosts
  that can read a stopped slot's disk (SSH, local, Docker), the command, its
  exit code and the last line of its output; the slot ends `unseeded`.
  `pool doctor` reports per slot that the requirements passed at seed time (a
  mark the boot script wrote), or `not checked on this slot; re-create it` when
  `requires` was set or changed after the slot was seeded. Doctor is read-only:
  a running slot, or Fly (cannot read a stopped disk), is shown as not read.
- `facts` runs **before every job's harness** (normal and `--read-only`, every
  provider), in the checkout, as the worker's user, clean environment, 10
  seconds each, output capped to the first 40 lines / 4096 bytes. Recorded per
  command: the command, its exit code, its stdout (stderr when it failed, or
  stdout if that is empty). **A failed or timed-out fact does not fail the
  job**; it is recorded as failed (`timed out` for a time-out). Shown on the task
  result under `--- facts`, before the receipt, and in the `pool_collected`
  JSON as `facts: [{command, exit, output}]`. Not in `--porcelain` lines.
  Prefer commands that print what the machine already holds: a fact that makes
  a toolchain manager download (a rustup shim resolving `rustc --version` on a
  rust image) meets the clean environment and the limits, and puts a failure
  in every receipt; `cargo --version` prints without one.
- Both are trusted configuration; their output is data (control characters
  dropped, escaped in the result file, never evaluated). `doctor --test`
  ignores both.

### Slots, create, destroy

- A slot is a Fly Machine with a persistent root disk (on Docker, a container;
  on SSH, a directory; see below). On first boot it runs
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
- The Fly app is `[pools.<name>.fly] app`, which is **required** (no default;
  there is no prefix or suffix). Fly app names are unique across ALL Fly
  accounts and Vivi does not try to make the name unique. Several pools and
  mailspaces may share one app: machine names and marks keep them apart.
  `create` makes the app when it does not exist, in
  `[pools.<name>.fly].organization` (default `personal`). If Fly says the name
  is taken, `create` says so and names the setting to change. Model keys are app secrets:
  `create` and `dispatch` list the app's secret NAMES and warn, naming the
  harness `env` entries with no secret and the command shape
  `fly secrets set -a <app> NAME=<value>`. Vivi never reads or sets a secret
  value; a name the image itself provides can be ignored. Setting one on a
  machine-only app: `fly secrets set` wants `NAME=VALUE` arguments, and
  staging may then print an error about finding an image or a current
  release (flyctl offering a Launch deployment the app does not use) — the
  secret has landed anyway; `doctor` confirms. Piping `NAME=VALUE` lines to
  `fly secrets import -a <app>` keeps values off the command line.
- `destroy` requires `--yes` and only touches this pool's slots.
- A slot can only fetch what the remote repository already has: push the base
  (the commit or branch passed as `from`) before dispatching.
- `--model` is resolved by the caller, in the harness's own vocabulary; Vivi
  only checks it against the pool's `[harness].models`.
- Slots made by the first iteration (before dispatch existed) lack the seed
  mark and the new boot script: destroy and re-create them.
- Requires `fly` (flyctl, logged in; or `FLY_API_TOKEN`) and `openssl` on PATH,
  and a GitHub App with contents access to the repo. Tokens are one hour,
  narrowed to the one repo, and never printed.

### Docker provider

`provider = "docker"` runs each slot as a container. Use the `docker` CLI; no
client library is involved, so a local and a remote daemon go through the same
code: `[pools.<name>.docker] host = "ssh://user@server"` is passed to the CLI as
`DOCKER_HOST`; omit it for the local daemon (or whatever your own
`DOCKER_HOST` names). An `ssh://` host can be a bare name your ssh config
resolves (`ssh://server`); every shape the docker CLI accepts works. (A plain
SSH host without Docker is the separate `ssh` provider below; to use Docker on
a remote host, set `[pools.<name>.docker].host`.)

- A slot is a container. A stopped container is a slot at rest and its writable
  layer is the persistent disk; its main process is the boot script, so it
  stops when the script exits. Containers are created with `--init`, a memory
  limit, an optional CPU limit, and never privileged.
- The memory disk over `build_dir` is a tmpfs mount declared when the container
  is created (`/work/repo/<build_dir>`, with `exec`), because the script cannot
  mount without privileges. The boot script skips its own mount when
  `BUILD_MOUNTED` is set. The tmpfs counts against the container's memory
  limit. Size `tmpfs` from the build output the seed actually produces
  (measure the repository's build directory on a machine that has one) plus
  headroom: a full tmpfs fails the build, and resizing means re-creating
  the slots.
- Labels cannot change after creation, so they hold only fixed facts:
  `vivi_pool` (pool name), `vivi_pool_mailspace` (mailspace slug), `vivi_pool_slot`, `vivi_pool_base`,
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
- Harness environment: the names in the pool's `harness.env` reach the container
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
  provider secret store; slots are namespaced by mailspace and pool, so two
  mailspaces with the same pool name on one daemon do not see each other's
  slots. The image needs `bash`; the boot script installs
  `git`, `zstd` and `curl` (and `ca-certificates` with them) with `apt-get` when they are missing, which needs
  network. With those tools already in the image, the whole pool, `doctor
  --test` included, runs offline.
- Slot ids are the first twelve hex digits of the container id; slot names are
  the container names (`<mailspace>-<pool>-slot-<n>`). `destroy` removes the containers,
  their claims and anonymous volumes.

### Local provider

`provider = "local"` runs the pool on the machine `vivi` runs on: a slot is a
directory under a root, and the worker is a process Vivi launches itself,
detached, as the current user. No rented machine, no container, no `ssh`.

```toml
[pools.here]
provider = "local"          # no `image`: setting one is an error
slots = 4
# repo + [github], OR worktree_of below (both together is an error)
# setup, seed, build_dir (optional), harness, committer as for the other hosts

[pools.here.local]
# root = "~/vivi-pool"      # default; each slot is <root>/<mailspace>/<pool>/slot-<n>/
# worktree_of = "~/src/repo"   # slots are Git worktrees of this local repository
```

- **No isolation.** The scripts, `setup` and `seed` commands and the worker run
  as the user who runs `vivi` with that user's whole account: all its files,
  credentials and network. Slots share this machine's cores, memory and disk.
  `setup` runs as you, not root. Say so when you recommend it; use Docker or Fly
  when that matters.
- **Fetch mode** (no `worktree_of`): like an SSH slot, the slot fetches from `repo`
  (with `[github]`) or, for `doctor --test`, from the embedded remote; the job
  ends in `pushed`.
- **Worktree mode** (`worktree_of`): the slot checkout is a Git worktree of that
  local repository, made with `git worktree add --detach` at the base. No repo,
  GitHub App, token or network. The job result is a commit on `branch` in that
  repository: outcome `committed <branch> <sha>` (not `pushed`); `no_changes`
  and the failure outcomes are as before. Nothing is pushed anywhere.
- **Branch rules (worktree mode).** Nothing is forced (no `-B`, `--force`,
  `branch -f`); an existing branch is never moved or reset. Decided at dispatch,
  before a slot is claimed:

  | `branch` | `from` | Result |
  |---|---|---|
  | new | a commit | created at that commit |
  | new | another branch | created at that branch's tip (stack) |
  | exists | the same branch | continue: checked out as it is, commit goes on top |
  | exists | anything else | **refused** (never reset); continue with `--from <branch>` or pick a new name |
  | checked out in another worktree | any | **refused**, naming the path |
  | the primary checkout's branch | any | **refused** |
  | the default branch | any | **refused** unless `--allow-default-branch` |

- **Detached at rest.** A slot never holds a branch while idle: it returns to a
  detached HEAD after a finished job, after the hard limit kills one, and when
  the slot is stopped or freed. A new branch is created only when the job has
  something to commit, so `no_changes` leaves none. The tree is cleaned back to
  `from` before the worker starts, sparing the build directory.
- **The source repository** is touched only by `git worktree add`, `remove` and
  `prune` (serialised per repository by a lock on its Git directory, across
  `vivi` processes) and read-only queries. `destroy` runs `git worktree remove`
  then removes the directory, under the SSH guards (inside the root, carries
  this mailspace and pool marks, not a link), and never deletes a branch.
- **Processes.** Start launches the boot script detached in its own process
  group; "running" means that process (id plus start time) is alive. Stop
  freezes (`STOP`) the group, its descendants and, on Linux, tagged processes
  until no new one appears, then ends them with `TERM` and `KILL`. macOS and
  Linux, `bash` and `ps` only; macOS lacks `timeout`, `sha256sum` and `nproc`, so
  the slot carries small stand-ins. Limit: a process that double forks out of
  both the group and the tree is not found on macOS (the hard time limit still
  applies).
- **Build output.** The `build_dir` (when named) is a plain persistent directory
  per slot, so a fixed pool of N slots holds at most N build directories.
- **State** is the marks (small files in the slot directory) and the claim (a
  hard link of a complete file, atomic across processes); nothing else is kept.
  Harness `env` values are read from the environment of the `vivi` process when
  the job runs; `doctor` names the unset names and no value is stored. The
  slot's `PATH` is the `vivi` process's too, so a harness installed under a
  user-only path (an nvm bin) resolves inside jobs whenever the dispatching
  `vivi` can see it; `doctor`'s tool check reads the system `PATH` only — a
  `facts` entry for the harness proves the job-side view.

### Bringing up a new pool

The order that reaches a working pool with the least rework. Everything here
is read-only or free except `doctor --test`, which spends only on Fly
(billable machines for a few minutes) and RunPod (which refuses without
`--allow-spend`).

On Fly the order shifts by one step: the app is a Fly-side object, `doctor`
fails while it does not exist, and only `create` makes it — so the step 1
self-test cannot run first. Start with `vivi pool create --pool <name>
--slots 1` (it makes the app and proves the provider with one machine), set
the app secrets, then `doctor`, `doctor --test`, and `scale` to the full
size. `scale` from an empty pool cannot make the app (it fails on the
machine list first).

1. Configure the pool with **provider settings only** — no `repo`, `[github]`,
   `[harness]`, `[committer]`. That much is valid: `status`, `doctor` and
   `doctor --test` all run, so the host is proven before anything else exists.
   On Docker the self-test is two throwaway containers on the daemon.
2. Add `repo` and `[github]`, then `vivi pool doctor` again: it mints a real
   repository token and resolves the default branch. Later steps depend on
   this passing.
3. Decide the harness and its key names **before** creating slots. `setup`
   installs the harness once per slot at create, and on Docker the
   `harness.env` values are read from your environment at create and live in
   the container's config; changing either later means destroy and re-create.
   A `facts` entry printing the harness `--version` names what each job ran.
4. `vivi pool create --pool <name>` (RunPod has no create: dispatch rents),
   then `vivi pool doctor` once more: every slot should read seeded and free.
5. Smoke both dispatch modes, one throwaway task each: a `--read-only` recon
   job (expect `read_only_unchanged`; read the receipt) and a one-file write
   job to a scratch branch (expect `pushed`; verify the branch on the remote,
   then delete it — `committed <branch> <sha>` on a worktree pool: verify in
   the source repository, then delete the branch there). Collect with
   `vivi pool wait` — it returns when the first named dispatch finishes;
   re-run it for any still open.
6. Close the smoke tasks (`task done` by the author or a mind) so no backlog
   node stays open.

### Using the pool (orchestrator procedure)

1. `vivi pool doctor --porcelain` (add `--pool <name>` to check one); fix what it names.
2. Push the base commit or branch to the remote.
3. `vivi pool dispatch --porcelain --pool <name> <task> --model <slug> [--from <ref>] [--branch <name>]`.
4. Run `vivi pool wait --porcelain <task>` in the background.
5. When it returns, read the receipt with `vivi task show <task> --latest`,
   which prints only the newest reply (the result) and not the brief.
6. Integrate the pushed branch.
7. Delete the remote branch.
8. Close the task (`wait` and `reconcile` never do).

**On a local worktree pool** (`worktree_of` set) steps 2 and 7 do not exist: there
is no remote and no base to push (the worktrees are seeded at `create --base`,
by default the tip of the repository's default branch, so check that commit
first), and the result is a local branch with no remote branch to delete. Step 6
integrates `committed <branch> <sha>` (merge or cherry-pick it in the source
repository); Vivi never deletes the branch. A refusal at step 3 (see "Local
provider") costs nothing: no slot was claimed.

**Integration branch without git access** — the route for an orchestrating Mind
that may not run git, and it needs no setup step for its integration branch.
Dispatch each unit with `--from <base commit> --branch <unit branch>`. Dispatch
the first merge job with `--from <base commit> --branch mind/<name>` (the
integration branch) to merge the first approved unit branch: that job brings
the integration branch into existence, and later merge jobs continue it with
`--from mind/<name> --branch mind/<name>`. A merge job on a remote pool (Fly,
Docker, SSH, local fetch mode) names the branches to merge with `--with` — for
example `vivi pool dispatch <task> --from mind/<name> --branch mind/<name>
--with pool/<unit-a> --with pool/<unit-b>` — and those branches must be on the
remote the pool fetches from; the seat finds them already fetched as
`refs/remotes/origin/<branch>` and merges with plain Git (on a worktree pool
`--with` works too and fetches nothing — the branches are already local refs,
seen by name). The one remaining cost: the integration branch does not exist
on the remote until the first merge has landed, so until then no job can start
from it; units that need another unit's code start `--from` that unit's branch
(stacking) instead.

### dispatch

`vivi pool dispatch --pool <name> <task> [--model <slug>] [--from <ref>] [--branch <name>]
[--budget SECS]` hands an open task to a free slot and returns at once.

- The brief is the task body, read from the mailspace. `--model` is optional:
  without it the model (and reasoning) come from the task's role through the
  pool's band table (see "Dispatching by role and band" below); with it the
  slug must be one of the pool's `[harness].models`. Every model, band and
  read-only check runs before any machine is touched.
- `from` is what to start from (a commit or a branch); `branch` is where to
  push. Each comes from its flag, else from a `from: <ref>` / `branch: <name>`
  line in the task body; both are required one way or the other. One rule covers
  three cases: a new branch (`from` = commit), continuing a branch (`from` =
  `branch`), and stacking (`from` = one branch, `branch` = another). On a pool
  that fetches from a remote, an abbreviated commit hash is resolved to the
  full hash at dispatch (through GitHub), and the job and the dispatch record
  carry the full hash. The push
  is never forced: if the branch moved or holds other work, the result is
  `push_refused`.

  A `branch` name that does not exist yet is created from the starting point on
  every provider: the job's published commit brings it into existence (a
  `no_changes` job creates none). An existing `branch` with a different `from`
  is where the hosts differ: a worktree pool refuses at dispatch, before a slot
  is claimed (the table under "Local provider"); fly, docker, ssh, runpod and
  local fetch mode do not check the branch when dispatching — the job runs, and
  its final push, never forced, ends `push_refused`.
- Extra refs a job must see besides `from` travel with the dispatch:
  `--with <ref>`, repeatable, at most 8, or `with: <ref>` lines in the task
  body. Each is resolved to a commit before a slot is claimed, so an unknown
  ref refuses the dispatch and claims nothing (a worktree pool resolves in its
  source repository the same way). Body lines follow the same trimming and
  list-dash rules as `from:`/`branch:` and every `with: <ref>` line counts — a
  line whose value is empty or holds a space is not a `with` line; when any
  `--with` is given, the flags win and the body lines are ignored. Duplicates
  are dropped keeping the first; a value that is not a plain ref name, or that
  names `from` or `branch`, is refused. An abbreviated hash is resolved to the
  full hash like `from`, and the job's `with.list` and the dispatch record
  carry the full hash. A `--read-only` job accepts `--with` too.
- The runner fetches the listed refs before the agent starts. On a pool that
  fetches from a remote, a branch name lands at `refs/remotes/origin/<ref>`
  and a 40-hex commit at `refs/vivi/with/<commit>`, each fetched at the pool's
  fetch depth; a ref the remote does not have ends the job `fetch_failed`.
  The fetches are shallow, so the runner then deepens — doubling the depth and
  fetching again, `from` by its commit and every extra ref — until each extra
  ref has a merge base with the start commit, capped at a total depth of 5000;
  never `--unshallow`, never the whole history. A ref that still has no merge
  base at the cap is only recorded: merging it is the agent's move. On a
  worktree pool nothing is fetched — the refs are local refs of the shared
  repository and the agent sees them by name.
- The result names the list: `with_count`, then `with_N_ref`, `with_N_local`
  and `with_N_sha` for each ref, and on a fetching pool also
  `with_N_merge_base` (the merge base with the start commit, `none` when
  deepening found none) and `fetch_depth`, the depth the history reached. The
  human receipt in the result reply says the same in plain text: an
  `extra ref <ref> -> <local> at <commit12> (merge base <base12 or none>)`
  line per ref, then `fetched at depth <depth>`. The
  agent holds no credential — the token never reaches its environment — but
  carries the pool's committer as its Git author and committer identity, so a
  plain `git merge` works unconfigured, and a merge the agent commits, or
  prepares with `git merge --no-commit` and leaves staged, is published on the
  job's branch with both parents. The dispatch record on the task names the
  resolved refs as `ref@commit12` entries, and the human dispatch line
  appends `with <ref>@<commit12> ...`.
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
  GitHub); `--allow-default-branch` overrides that. (A worktree pool applies
  the table of "Local provider" instead, before claiming a slot.)
- A task with an open dispatch cannot be dispatched again until it is collected.
  After collection it can be dispatched again (for example to continue its
  branch).

#### Dispatching by role and band

A task addressed to a known role is dispatched from that role.
`vivi pool dispatch --pool <name> <task>` looks the addressee up; an absent or
unknown addressee is no role at all, and the dispatch then needs `--model` or
a `band:` line. From the role it takes:

- **The charter as the worker's standing prompt.** The rules file the job runs
  under is composed charter first, then the pool note, then the pool rules
  (which bound the charter). The pool note tells the worker how the slot
  reaches the mailspace. Without the MCP bridge the slot has no `vivi`
  command, so a charter line that tells the role to settle its own handle
  cannot be followed: the note tells the worker to end with its receipt on
  standard output instead, and the Mind settles the handle. With
  `[pools.<name>.bridge] enabled` the pool note instead tells the worker to
  read and settle its handle through the vivi tools. A role with no
  charter leaves the pool rules unchanged.
- **Read-only mode from the kind.** A role whose kind is `auditor`, `triage`
  or `cadence` (case-insensitive) is dispatched read-only whatever the flags
  say; a `hand` role writes.
- **The model and reasoning from the band.** The role's `model` field holds a
  band string such as `P2-S0` (set it with `vivi role set <role> --model P2`),
  not a model slug. The pool's `bands` table says which model-and-reasoning
  pair serves each band, and the job runs at that pair.

Bands: `P0` to `P5` is the intelligence axis (P0 the most capable); the
optional `-S0` to `-S3` speed suffix is parsed and ignored for now. The
plain-language gloss: P0 can make complex decisions, P1 understands anything
inside its area, and P2 executes a tightly constrained scope. A `band:` line
in the task body overrides the role's band for this one assignment (the first
one wins; the same trimming rules as `from:`). `--model` and `--read-only`
remain the overrides: `--model` wins over any band and takes the band's
reasoning only when the pool's choice for that band names the same model;
`--read-only` forces read-only on any role.

The bands table is per pool:

```toml
[pools.local.bands]
P1 = [{ model = "provider/model-a", reasoning = "xhigh" }]
P2 = [{ model = "provider/model-b", reasoning = "low" }]
```

Keys are `P0` to `P5`, each holding an ordered list of `model` + `reasoning`
choices. The **first** entry of a band's list serves the band; later entries
are reserved for in-band failover, which is **not built**: no failover, no
health tracking, no retry on another model — the first choice is the only one.
When the harness template uses a `{reasoning}` placeholder beside `{model}`,
a job dispatched by band runs at its band's reasoning; a job dispatched with
`--model` that is not the band's model has no reasoning, and a `{reasoning}`
harness refuses it.

A pool with nothing for the band refuses before a slot is claimed, naming the
band and the pool (and the bands it does serve): send that job to a pool that
has the band, or pass `--model`. No refused check consumes a slot. The human
dispatch line names what was used (`role <name> band P1 -> <model> <reasoning>`,
with `read-only (role kind <kind>)` appended when the kind, not the flag,
forced read-only).

Also not built: `vivi step` does not yet offer tasks addressed to `auditor` or
`merge` roles to those roles.

### wait and reconcile

- `vivi pool wait [--pool <name>] [<task>...] [--timeout SECS]` blocks until at least one open
  dispatch (the named ones, or all) has finished, collects each finished one,
  prints one line per task (`<task>  <pool>  <state>  <slot>  <detail>`; with `--porcelain` the `collected` line above), and exits
  0. On timeout (default 3600) it exits nonzero and says nothing finished.
  Run it in the background to be woken.
- `vivi pool reconcile [--pool <name>]` handles every open dispatch once: finished ones are
  collected and their slots freed; one still running past its budget plus a
  15-minute grace is stopped and recorded as a timeout; one whose machine
  stopped without a result, or no longer exists at all, is recorded as lost
  (and the task can be dispatched again); one still within budget is
  reported `running`. It also frees stale claims (a held slot with no open
  dispatch, claimed more than five minutes ago; a RunPod pool uses a longer
  grace, see "RunPod provider").
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
  token from `fly auth token` stops working after about six minutes).
  `reconcile` also stops any running slot that no task holds and no dispatch
  explains. Read the receipt
  with `vivi task show <handle> --latest`, which prints only the newest reply
  (the result) and not the brief.
- Push outcomes: `pushed <branch> <sha>` (a worktree pool says
  `committed <branch> <sha>`: a commit on the local branch, nothing pushed),
  `push_refused`, `no_changes`,
  `fetch_failed`, `checkout_failed`, `commit_failed`. A result with
  `state=killed` means the slot's hard limit stopped the runner.

### On the slot

A generic runner, embedded in the binary and delivered with each job, returns
the checkout to a clean tree at `from`, runs the harness as the unprivileged
user `hand` with a clean environment (only `HOME`, `PATH`, and the names in
the pool's `harness.env`; the Git token is never among them and is readable only by
root), then commits what changed as the configured bot identity, with a
`Pool-Run:` trailer recording slot, model, start ref, exit code, and duration,
and pushes without force. The harness's standard output is the receipt;
standard output and error are kept as files. A finished job lingers for two
minutes so `wait` can collect without a restart; a boot with nothing to run
stops itself after 60 seconds; the job directory survives a
stop and start until the next dispatch replaces it.

### SSH provider

`provider = "ssh"` is the minimal form: a host reachable by SSH where each slot
is a subdirectory, using the tools the host already has. It is supported but it
is **not the recommended host**: pick Docker or Fly unless a bare SSH host is
all there is.

```toml
[pools.box]
provider = "ssh"
# repo, slots, setup, seed, build_dir as for the other hosts; NO `image`
# (setting one is an error: a slot is a directory, not a container)

[pools.box.ssh]
host = "user@server"          # required; anything `ssh` accepts, including a Host alias
# root = "~/vivi-pool"        # default; each slot is <root>/<mailspace>/<pool>/slot-<n>/
```

- **Needs on the host**: SSH without a prompt (`BatchMode=yes`, connect timeout
  on every call), a POSIX login shell, Linux (`/proc`), and `bash`, `git`, `tar`,
  `setsid`, `timeout`, `sha256sum`, `ps`. Nothing else: no root, no `zstd`, no
  `curl`, no systemd, no Docker. Missing tools are a `doctor` failure; the slot
  scripts install nothing (no `apt-get`, `useradd`, `runuser`, `mount`, `chown`).
- **What it deliberately lacks**: **no isolation.** The scripts and the agent run
  as the SSH login user with that user's access to everything it can reach, and
  the Git token of a job is deleted once read but the agent is the same user. No
  image. Slots share the host's cores, memory and disk with each other and with
  everything else there. `setup` commands run once per slot **as the
  login user, not root**, so they cannot install packages; the toolchain must
  already be on the host.
- **Mapping.** A slot is the directory `<root>/<mailspace>/<pool>/slot-<n>/`
  holding the checkout, build directory, job directory, boot log and marks
  (files in `marks/`, which carry the pool and the mailspace). The slot id in
  `status --json` is `<mailspace>/<pool>/slot-<n>`. The `created` time that
  `status` shows is a `created` file written when the slot is made (slots made
  by an older Vivi show it empty). `/work` in the
  scripts means that directory (`WORK`). The build directory is a plain
  directory that persists: no memory disk, nothing packed or restored; a seeded
  slot keeps its warm build, and the runner's `git clean` spares `build_dir`.
- **In-memory slots.** The root may sit on a memory filesystem the host
  already mounts: `/dev/shm` (tmpfs, present on mainstream Linux by default,
  usually half the RAM) makes every slot fully in-memory with no extra
  setup — Vivi mounts nothing, the filesystem must already exist, and
  `doctor` reports free space on it. `destroy` unlinks at once (the pages
  return to RAM immediately); a reboot or unmount clears the whole root,
  after which the slots are re-created. Collect finished jobs promptly: an
  uncollected result leaves no disk residue. Size the mount from a fresh
  seed, not from a long-lived workspace — a clean checkout plus build is
  often several times smaller than a developer's accumulated build
  directory.
- **Start/stop.** Start launches the boot script detached (`setsid`, a pid
  file), so it outlives the SSH session. A slot "runs" while that process is
  alive and carries `VIVI_SLOT_ROOT=<slot dir>` in its environment (a reused pid
  is not mistaken for it). Stop kills every process carrying the tag, then
  escalates to KILL, so nothing a job started outlives a stop; a process that
  scrubs its environment can escape. The hard job time limit, the idle stop and
  the linger work as on the other hosts.
- **Claim.** Atomic on the host and safe for two `vivi` processes on two
  machines: the claim is a hard link of a complete file (task, time, nonce);
  `ln` fails if it exists. It is also the task/time mark; `unclaim` removes it.
- **Delivery and results.** The job archive is copied over SSH into the slot and
  unpacked by the boot script; results are read as files over SSH whether or not
  the slot runs (no restart).
- **Harness environment.** The `harness.env` names take their values from
  the remote login user's environment at job time (a login shell: put
  `export NAME=...` in `~/.profile`, or in `~/.bash_profile` when the host
  has one — bash reads it first and skips `~/.profile`; `doctor` runs a real
  login shell, so it proves whichever file you chose). Values never pass
  through Vivi or the mailspace. `doctor`, `create` and `dispatch` name the
  ones unset on the host.
- **Destroy** removes the slot directory and is guarded on the host: only
  `<mailspace>/<pool>/slot-<n>`, only a plain directory inside the root, only
  with Vivi's own marks naming the pool **and** the mailspace, never with an empty or `/` root; anything else is
  refused with a message. A pool directory, a mailspace directory and a root Vivi made are removed
  with their last slot.
- **Connection.** Calls share one SSH connection (a private control socket in
  the temporary directory, removed at exit); about 40 ms per call once warm,
  and a fresh connection costs one ordinary SSH login.
- Several mailspaces and pools may share one root: each has its own directory
  and marks and never sees the others'. `cargo test --test pool_ssh_live -- --ignored` with
  `VIVI_SSH_TEST_HOST` set proves the process handling and the claim.

Config is one `[pools.<name>]` table per pool in `.vivi/mailspace.toml`.
Everything provider neutral sits in the pool table; the Fly settings sit in
`[pools.<name>.fly]`, the Docker settings in `[pools.<name>.docker]`, the SSH
settings in `[pools.<name>.ssh]`, the local settings in `[pools.<name>.local]` and the RunPod settings in `[pools.<name>.runpod]` (shown under "RunPod provider"). Only the table of the chosen `provider` is
used and checked beyond its key names. A top-level `[pool]` table is rejected.
Everything below is per pool and nothing is inherited:

```toml
[pools.lanes]
# provider = "fly"            # default; or "docker", "ssh", "local" or "runpod"
repo = "owner/name"           # optional for a provider-only test pool; needed by create/scale-up/dispatch (with [github])
slots = 4                     # required; maximum pool size
image = "rust:1.97-bookworm"  # required on fly, docker and runpod (Debian-based: needs apt for zstd/git); an error on ssh and local
setup = ["npm install -g some-harness"]   # root, once, before the checkout (login user on ssh)
seed = ["cargo build"]                    # in the checkout, once, warms build_dir
# requires = ["command -v some-tool"]     # must exit 0 on a slot, at seed time (see Requirements and facts)
# facts = ["some-tool --version"]         # recorded in every job's result, before the harness starts
# build_dir = "target"        # optional, relative to the repo; NO default: without one there is no memory disk, no pack or restore, and the clean (git clean -fd, no -x) leaves gitignored output like target/ alone on every provider

[pools.lanes.fly]
app = "my-fly-app"            # REQUIRED, no default; created by `pool create` if missing; a taken name is for you to change
# organization = "personal"   # owner of a newly created app; default personal
# region = "iad"              # default iad
# size = "performance-8x"     # default
# memory = 32768              # MB; default 32768
# tmpfs = "16g"               # memory disk over build_dir; default 16g

# [pools.lanes.docker]        # only with provider = "docker"; every key optional
# host = "ssh://user@server"  # a remote daemon; omit for the local one
# memory = "4g"               # per-slot limit (includes the memory disk); default 4g
# cpus = 4                    # per-slot CPU limit; default none
# tmpfs = "2g"                # memory disk over build_dir; default 2g

# [pools.lanes.ssh]           # only with provider = "ssh"
# host = "user@server"        # required
# root = "~/vivi-pool"        # slot directories live in <root>/<mailspace>/<pool>/; default ~/vivi-pool

# [pools.lanes.local]         # only with provider = "local"; every key optional
# root = "~/vivi-pool"        # slot directories live in <root>/<mailspace>/<pool>/; default ~/vivi-pool
# worktree_of = "~/src/repo" # slots are worktrees of this local repository (then no repo/github)

[pools.lanes.github]          # with repo
app_id = 123456
private_key = "~/secrets/app.private-key.pem"

[pools.lanes.harness]         # required to dispatch; not needed to test
name = "pi"
run = "pi -p --model {model} --append-system-prompt {rules} @{brief}"
models = ["provider/model"]            # --model must be one of these
env = ["MODEL_API_KEY"]       # names only; values come from the machine's environment
# rules = "docs/worker-rules.md"          # default: a short embedded rules file

[pools.lanes.committer]       # required to dispatch; not needed to test
name = "lane-bot"
email = "lane-bot@users.noreply.github.com"
```

`run` is a shell command; `{model}`, `{brief}` and `{rules}` are the only
placeholders (the brief and rules are files inside the slot). Its standard
output is the receipt. Use a print mode that writes only the final answer.
The harness a slot's `setup` installs is what its package source serves at
create time — possibly newer than the copy on the machine you dispatch from —
so a `facts` entry printing the harness `--version` names what each job
actually ran. Validation runs when a `vivi pool` command starts, and rejects misspelled keys.
The config holds a key path and variable names, never a key or a value.

### RunPod provider

`provider = "runpod"` rents one pod per dispatch (RunPod, via `runpodctl`)
and reaches it over SSH: GPU pods by default, or CPU pods with
`runpod.compute = "cpu"`. **A pod bills by the second from the moment it exists
and a stopped pod keeps nothing, so there are no resting slots.** `slots` is
the most pods that may exist at once. Do not invent a warm pool, a reseed, an
idle pod or a "keep it for the next job" option; none exists.

```toml
[pools.gpu]
provider = "runpod"
slots = 2                       # ceiling on simultaneous pods
image = "runpod/base:1.0.2-ubuntu2404"   # an image that starts sshd itself (see below)
# repo, github, harness, committer, setup, seed, requires, facts as usual

[pools.gpu.runpod]
api_key_file = "/path/to/runpod.key"   # required; mode 600; Vivi never reads it
gpu_types = ["NVIDIA A100 80GB PCIe", "NVIDIA H100 PCIe"]   # required, best first; `runpodctl gpu list` spells them
gpu_count = 1                   # 1 to 8
cloud = "secure"                # or "community"
container_disk_gb = 40
# compute = "gpu"               # or "cpu": a CPU pool (then no gpu_types, gpu_count or
                                # min_cuda_version, and container_disk_gb is capped at 20)
# ready_timeout = 120           # seconds a new pod gets to answer on SSH; 30 to 1800; default 120
# min_cuda_version = "12.8"; data_centers = ["EU-RO-1"]
```

A different GPU tier is a different pool. The GPU type a pod actually got is in
the dispatch record and the `dispatch` output.

**When to use it.** RunPod is for work that needs a card you cannot get
elsewhere. It is not a general lane pool: every dispatch pays for a fresh pod
and a full seed. Use an image RunPod already caches (its own public images)
that publishes SSH. Measured on 2026-10-11: a working pod answered on SSH 35 to
45 seconds after the create call, on three runs. CPU pods (`compute = "cpu"`)
are small — 2 vCPU was observed — and cheap ($0.06/h observed): a mid-size
Rust crate seeds on them in a few minutes, which is fine for everything that
is not a million-line build.

- **The image must publish SSH itself, on both tiers.** A stock registry image
  (a `rust:*` image, say) rents fine and then never answers: on CPU pods the
  REST path runpodctl uses cannot request RunPod-managed SSH at all, and on
  GPU pods the managed request still left the SSH port unallocated for the
  whole `--wait` window (observed live 2026-10-12, both tiers; the pod was
  terminated, ~$0.02, and the next GPU type was tried before the dispatch
  refused). Use an official RunPod image that starts sshd, such as the
  `runpod/base:<tag>-ubuntu*` family, and install the toolchain and harness
  through `setup`.
- **A root-installed toolchain is invisible to the worker user.** `setup` runs
  as root, but the facts and the agent run as `hand`: a rustup under `/root`
  cannot be reached (its home is mode 700, and the rustup proxies need
  `RUSTUP_HOME` besides). Install with `RUSTUP_HOME=/opt/rustup
  CARGO_HOME=/opt/cargo`, `chmod -R a+rX` both, and symlink the **real**
  toolchain binaries (`/opt/rustup/toolchains/*/bin/*`) into `/usr/bin`; a
  `cargo --version` entry in `facts` proves the worker-side view.

- **`ready_timeout`**: the longest a newly rented pod gets to answer on SSH
  (default 120 seconds, 30 to 1800). **A pod that exists but never becomes
  reachable is billed** (observed 2026-10-11: one RTX 4090 pod never answered).
  On expiry Vivi terminates that pod, confirms it is gone from RunPod's list,
  and tries the next entry of `gpu_types`; when every type fails, the dispatch
  fails. `vivi pool dispatch --ready-timeout <secs>` overrides it for one
  dispatch (same range; a usage error on Fly, Docker, SSH and local pools). Each
  abandoned pod is reported on standard error with its id, GPU type, seconds
  waited and an approximate charge (hourly price x time); the dispatch line, the
  task reply and the dispatch record carry the count and the total. Seeding has
  its own 30-minute limit, counted from the pod answering on SSH.

- **Commands**: `create` and scale-up are refused with an explanation (nothing
  is made ahead of a job). `scale N` and `destroy` terminate pods and wait until
  RunPod's list no longer shows them. `status` prints `N pod(s) of at most M`.
  `dispatch` takes minutes (rent, wait for SSH, pin the host key, seed, claim,
  deliver, start), then returns and tells you the pod is billing. `wait` and
  `reconcile` collect the result and then **terminate the pod and confirm it is
  gone from the list**. A result is a pushed branch or a receipt; there is no
  artifact or output-file collection today, so a job that must keep something
  has to push it.
- **Cost safeguards, with limits.** (1) `pod create --terminate-after` at
  dispatch + setup worst case + budget + 15 min, where the setup worst case is
  (number of `gpu_types` x the ready timeout in force) + 30 min seeding. **Do
  not rely on it**: on 2026-10-11 it fired on neither pod tested. A CPU pod was
  still running 12 minutes past that time, and a GPU pod rented by a Vivi
  dispatch 20 minutes past it; each ran until something terminated it.
  (2) Collection terminates the pod: needs Vivi to run, so **always collect a
  RunPod dispatch promptly** (`wait` in the background, or `reconcile`).
  (3) A failed dispatch terminates its own pod; if that fails, the error says
  the pod is still billing, its deadline, and the command that ends it.
  (4) `reconcile` terminates a pod of the pool that no dispatch explains, once
  it is older than the setup worst case plus one minute, computed with the
  pool's `ready_timeout` (35 minutes for two GPU types at the default). A
  dispatch that passed a longer `--ready-timeout` is not covered by that grace. (5) `doctor` lists the pool's pods with run time
  and hourly price, fails on pods past their deadline, and fails on stray pods
  (Vivi's name prefix, a pool this mailspace no longer has, or a malformed
  name) as "billing until terminated". It never touches another mailspace's
  pods, only counts them. Nothing here terminates a pod whose name is not Vivi's.
- **Identity and claim**: the pod name is the record,
  `vivi-rp1-<mailspace>-<pool>-<mailspace length>-<index>-<task>-<claimed>-<deadline>-<nonce>`
  (at most 191 characters, checked at config time). There are no labels or
  leases. A dispatcher takes a file lock per mailspace and pool on this machine,
  picks the lowest unused index, refuses at `slots` pods, and after its pod
  exists keeps it only if no earlier pod (by RunPod's creation time, ties by id)
  holds its index or fills the cap; it checks again just before the job starts.
  A loser terminates itself. Limit: if RunPod's list lags so that neither of two
  racing pods sees the other at either check, two pods can briefly share an
  index or exceed `slots`; two jobs never share a pod, and `doctor` and
  `reconcile` show and clear the excess. Dispatchers on different machines have
  no lock between them. Tested against a simulated race only.
- **SSH**: the host key is captured at first contact (two `ssh-keyscan`s that
  must match), pinned in a per-pod file under `.vivi/runpod/pins/` (mode 600),
  then checked strictly; a changed key fails closed. The account needs an SSH
  public key registered with RunPod. The pod runs the same remote, boot and job
  scripts as the `ssh` host, as root in the container, the agent as `hand`.
- **The API key**: `api_key_file` is required; a file readable by group or
  others is refused with `chmod 600`. Vivi never reads it: each call starts
  `runpodctl` through a fixed `sh` wrapper that reads the file and passes the
  key in the environment (`RUNPOD_API_KEY`). The key is never an argument, never
  in Vivi's output, never sent to the pod; the CLI's own error text is scrubbed
  of key-shaped words. `harness.env` values are read from the `vivi` process at
  dispatch and sent over the pinned SSH connection into a root-only file in the
  pod; they do not go through RunPod's API.
- **doctor** is read-only: `runpodctl` and version; key file and mode; the API
  accepts the key (identity and balance only); SSH keys on the account; local
  `ssh` and `ssh-keyscan`; each GPU type against the live market; image name;
  the pool's pods; strays; the ready timeout in force and the worst-case wait
  across all GPU types.
- **`doctor --test` does not spend by default.** It refuses, says it would rent
  a pod, quotes the first type's hourly price when readable, and says
  `--allow-spend` is needed. With `--allow-spend` (valid only with `--test`) it
  rents one pod, runs a stand-in job and terminates it. **Never pass
  `--allow-spend` unless the operator asked for a billable test.**
- **Verified live (read-only, empty account)**: the output shapes of `user`,
  `gpu list`, `datacenter list`, `ssh list-keys` and `pod list`; that
  `RUNPOD_API_KEY` overrides the CLI's saved login; `pool doctor`, `pool
  status`, and the `doctor --test` refusal. **Verified live by one
  `doctor --test --allow-spend` run (2026-10-11, one pod, 51 seconds)**: pod
  creation, name acceptance, host key capture and the SSH scripts on a real
  pod, root-mode boot on a RunPod image, collection, termination plus the
  absence check. **Verified live by one dispatch (2026-10-11)**: fallback to
  the second GPU type when the first pod never became reachable (before
  `ready_timeout` existed, after a ten-minute wait). **Verified live by one
  read-only dispatch against a real repository (2026-10-11)**: token, fetch,
  `facts` recorded from the rented card, and `reconcile` collecting the
  finished job from the still-running pod 55 minutes later, then terminating
  it and confirming it gone. **Verified live, CPU tier (2026-10-12)**:
  `compute = "cpu"` create, self-test, a read-only dispatch
  (`read_only_unchanged`) and a write dispatch that pushes a branch, all
  against the real repository on `runpod/base` pods (2 vCPU, $0.06/h, whole
  dispatches 4-7 minutes each); the API's CPU disk cap surfaced as
  `Container Disk must be less than or equal to 20` (validation now enforces
  it). **Verified live, GPU tier (2026-10-12)**: self-test plus read-only and
  write dispatches on an L4 with the same base-image setup, the harness
  environment reaching the agent, and an sshd-less image failing to answer on
  SSH on both tiers (see the image rule above). **Checked live and found not to work**: `--terminate-after` (see the cost safeguards). **Still not
  verified live**: the `ready_timeout` path itself (the id in `runpodctl`'s
  timeout error, the termination and absence check that follow, the reported
  cost: tested against a fake account only; if the error names no pod and the
  list does not show it yet, the pod stays until something terminates it), the
  no-capacity failure text, creation-time ordering and list lag, and state words of
  stopped or terminating pods. The self-test does not run a pool's `requires` or `facts`.

## Watches and Cycle Intake

Project-local watch commands observe mailspace events. Prefer `--once` for
fail-fast cycles or an explicit timeout for waiting. Use cursor or watermark
files when repeated cycles must not replay old events. Add graph event filters
only when node readiness or state changes are relevant. Letters filed by
`sync pull`, `fetch`, `resync` or `pull --refs` raise the same events as a
local send, so a watcher wakes for them; without a cursor file or `--since` a
watcher replays existing events first.

`cycle intake` collects pending tasks, needs, mail, and recent memos for one
identity. It is a bounded intake surface, not a replacement for inspecting the
specific handle selected for work.

## Project Sync

`vivi sync` replicates a mailspace with a project on a server, so several
mailspaces on different machines share needs, tasks, wants, and mail. The unit
of sync is the letter: the exact `.eml` file. It travels byte for byte, its
`sha256` is its identity on every machine, and it is immutable once written.
The server stores and serves letters and never rewrites one. Verify the exact
flags with `vivi sync --help`.

A mailspace works with no `[sync]` and no server, and its state is a pure
function of the letters it holds. Sync only moves letters; `pull` applies the
close and reopen events among them (see State as letters).

```sh
vivi sync                # push, then pull (the pull runs even when the push stopped)
vivi sync push           # send local letters the server does not have
vivi sync push --drain   # wait out the rate limit and keep sending until nothing sendable is left
vivi sync push --since 1d      # only letters dated in the window; older ones stay unsent for a later push
vivi sync pull           # file what other members pushed
vivi sync pull --since 7d      # first pull only: start at a time, not at the beginning
vivi sync pull --refs [--depth N]  # then look up the letters the new ones point at
vivi sync resync         # reset cursor and floor to zero, replay everything
vivi sync fetch <handle>...    # file letters by their 8-hex handle
vivi sync prune --older-than 30d [--dry-run]  # evict old letters, locally
vivi sync keep-local <handle>...  # stop pushing a stuck letter; it stays here; --undo tries it again
vivi sync key            # print this mailspace's public-key PEM (mints the key file)
vivi sync create [--name <creator>]  # new project on the server; prints its id, writes nothing
vivi sync vouch <name> --key-file <pem>  # vouch for a member's key (or --key <pem>)
vivi sync join           # register [sync] name (the key must be vouched for)
vivi sync directory      # members: name, address domain, fingerprint
vivi sync status         # fingerprint, server, project, address, cursor, epoch, history, unsent, not uploaded, held for unknown roles; not the PEM — `vivi sync key` prints it
vivi sync api-key mint --label <LABEL> [--out <FILE>]  # project API key (label 1 to 64 characters). Without --out: prints key_id, label, and the raw key once; Vivi never stores the key. With --out: writes the raw key to a new 0600 file (it must not exist; its directory must) and prints only key_id, label, and the file path. Automation and agents: always pass --out and read the key from the file — never capture a mint's stdout without it
vivi sync api-key list   # live project API keys: key_id, label, created_by, created_at; never the secret
vivi sync api-key revoke <KEY_ID>  # retire a live key
vivi mcp                 # stdio MCP bridge to the project server's tools (see MCP bridge below)
```

**Draining.** The server allows 30 letters a minute per member, so a large
push stops at the first 429 with the rest unsent. `vivi sync push --drain`
waits it out: it prints one `sync push: pass N` line per pass, waits 61 s
after a rate limit, and keeps sending until nothing sendable is left. It
still stops non-zero on a 401, a network failure, or another server error,
and it holds the sync lock across all passes. It composes with `push
--since`, and bare `vivi sync --drain` drains its push pass.

**Addresses.** Every mailspace has a UUID `id`, minted once into its
`mailspace.toml`. Its first 8 characters are the domain of every address it
mints: the `mind` role of the mailspace whose id begins `3f9a1c7e` is
`mind@3f9a1c7e.local`. The address sits inside the letter, so sender and
recipient are part of the hashed object. Treat the file as machine-local
(`.vivi/` ignores itself, so nothing in it is committed): do not commit it,
edit the `id`, or copy a `.vivi` directory to another machine (the server
refuses a second member under a prefix already claimed). Letters
written before the id existed keep their old `name.local` domain; they travel
but are not routable, so address a reply to one explicitly.

**Configuration** is the `[sync]` table: `server`, `project` (the server-side
project id), and `name` (this checkout's registered member name). Identity is
transparent: one ED25519 keypair per mailspace, minted on first use at
`.vivi/chat/identity.ed25519` (0600). The directory keeps the name of the
retired `vivi chat` so names already registered stay valid, and writes its own
`.gitignore`, so the key is never committed. A member's key must be vouched
for by an existing member before `vivi sync join` is accepted. There is no recipient or name map: the address carries both. The
server binds a name to the first key admitted under it and has no re-key, so a
checkout that loses its key cannot take its name back; only the project creator
can help. If a join says another member already holds this mailspace's address
prefix, remove both the `id` line in `.vivi/mailspace.toml` and the file
`.vivi/mailspace.id` (the copy that restores the id) so a new id is minted;
letters sent after that carry the new prefix. The two copies must agree: a
config id that disagrees with `.vivi/mailspace.id` is refused by every command
with a message naming both files and the one edit that settles it; nothing is
changed for you.

**Linking a mailspace to a server project.** This works for a new or an
existing mailspace; an existing one keeps its id and history. Never copy a
`.vivi` folder between machines.

1. Add `[sync]` with `server` (e.g.
   `https://vivi-server-main-production.up.railway.app` for staging),
   `project` (the server project id), and a new `name` for this checkout.
   Names bind to the first key admitted forever, so pick one you will keep.
   A brand new project is made with `vivi sync create [--name <creator>]`:
   it prints the project id and writes nothing — put that id in `[sync]`
   and run `vivi sync join` as the creator.
2. Run `vivi sync key`. This mints `.vivi/chat/identity.ed25519` if it is
   missing and prints the public-key PEM (`-----BEGIN PUBLIC KEY-----` …).
   Send that PEM, never the private seed file, to an existing member.
   (`vivi sync status` shows the fingerprints; it does not print the PEM —
   `vivi sync key` does.)
3. The existing member runs `vivi sync vouch <name> --key-file <pem-file>`
   (or `--key <pem>`) with the PEM you sent. `vouch` composes and sends
   the vouch letter itself: one governance letter to the project's
   reserved `postmaster` address. `vivi mail send --to postmaster` is
   still refused as an unknown local role; `vouch` is the verb for it. The
   project server also has an MCP surface whose `send` tool can carry a
   vouch; `vivi sync vouch` is the CLI route.
4. Run `vivi sync join`, then `vivi sync`. The push uploads existing needs,
   tasks, wants, and plain mail (never memos, drafts, trash, or rules).
   Letters written before the mailspace had an id keep their old
   `name.local` domain; they sync, but address replies to them explicitly.

The vouch letter is addressed to `postmaster@<own address domain>`, and
members' pulls file it under no role and hold nothing for it (see
Governance letters below), so no role folder and no backlog sees it.

Errors: `admission_required` means no vouch yet, or the name was admitted
with another key: the join error prints the two commands to run — `vivi sync
key` for the new member and `vivi sync vouch <name> --key-file <pem>` for an
admitted member — with this mailspace's own `[sync] name` and the wire
fingerprint filled in (the server binds a name to the first key admitted and
has no re-key; only the project creator can help). A missing `[sync] project`
is answered with `vivi sync create`, which makes a new project and prints the
id to put in `[sync] project`. `mailspace_prefix_taken` means a copied id;
see Addresses above.

**One sync at a time.** `push`, `pull`, `resync`, `fetch` and `prune` take an
exclusive lock under `.vivi/sync/`, so a cron run and an interactive run
serialize. The second waits up to 10 seconds, then exits 1 with `another vivi
sync is running`. A corrupt or unreadable `.vivi/sync/state.json` makes
`status`, `pull` and `fetch` fail with a message naming the file; `vivi sync
resync` rebuilds it.

**Sending to another mailspace.** Address the role directly, taking the prefix
from `vivi sync directory`:

```sh
vivi need send --from mind --to hand@ab12cd34.local --subject ... --body ...
```

A remote recipient gets no local delivered copy. The letter is held as a sent
copy, the command prints `queued for sync: ...`, and the next push sends it.
`mail deliver` takes remote `To`/`Cc` addresses the same way (a letter with no
local recipient is kept as the `From` role's sent copy, so `From` must be a
role here). Without a `[sync]` table the command prints a note instead:
the letter stays in this checkout. A reply to a pulled letter is addressed to
its `From` with no `--to`, so a remote sender is answered through the server.

**Letters that cannot go.** With `[sync]` configured, a letter over 64 KiB is
refused when it is composed (`mail send`, `mail reply`, `need`/`task`/`want
send`, `mail deliver`) and nothing is stored. Push never uploads a letter whose
`From` claims another mailspace's prefix (every puller would refuse it), and
it lists any stored letter that is too large or has such a `From` by handle and
exits non-zero, each with `vivi sync keep-local <handle>`. That command keeps
the letter in this mailspace — nothing is moved, trashed, or rewritten — and
the next push exits 0; `vivi sync keep-local --undo <handle>` reverses it.
`vivi sync status` counts them on a `not uploaded` line beside `unsent` and
repeats the keep-local command per letter. The server does not check that a
recipient prefix belongs to anyone: compare it with `vivi sync directory`.

**What pull does.** Each record is verified before it is filed: the bytes hash
to the stated `content_sha256`, the envelope recipients equal the letter's own
`To` and `Cc`, and a `From` address in prefix form carries the prefix the
server registered for its sender. A letter that fails is refused, named, and
makes the command exit non-zero; nothing partial is filed. Filing is by
address: one delivered row for every local `To`/`Cc` role, plus a `sent` row
when `From` is a local role. A letter for a role this mailspace lacks (a typo, a
name never added) is **held** under that name, not refused: the pull exits 0 and
its summary line says `N held for unknown role(s): ghost` beside a stderr
warning. Held work mints no backlog node, so `step` and `boot` do not dispatch
or count it; `vivi sync status` and `vivi boot` list each unknown name with its
letter count, and `vivi mail list --to <name>` (`--folder tasks|needs|wants` for
work) shows them. `vivi role add <name>`, `role rename` or `mailspace identity
add|rename` to that name **adopts** them and says `adopted N held letter(s)`:
work gets its node and dependency edges then. A `retired` role still receives
(with a node); the pull warns `role X is retired` once. A need, task, or want
addressed to another mailspace's role is held under its address, so every
member holds every work item without it landing in a local queue or the local
backlog.

**Governance letters.** A letter addressed to the reserved `postmaster` (any
domain, case aside, by `To`, `Cc`, or envelope) is project governance — the
vouch letter travels this way. A pull files it under no role: no inbox or sent
copy, nothing held for an unknown name, and no unknown-role warning. It is not
silent: the pull summary counts it as `N governance letter(s)`, and the cursor
moves past it.

**A push window.** `vivi sync push --since <age|date>` (the same forms as the
pull window) sends only letters dated at or after that time. A push goes oldest
first and a member may send 30 letters a minute, so a mailspace with a long
history that joins late would otherwise make its newest letters wait behind all
the old ones. Older unpushed letters are counted on a line of the report, stay
in `sync status` as unsent, and go on any later push without `--since`. A
letter with no readable date is never held back. A reply to a letter that was
held back arrives unlinked on the far side until the older letter is pushed.

**Windows and the floor.** A pull reads the project in pages of 500 and saves
the cursor after every page, so an interrupted pull resumes where it stopped.
A new mailspace need not take the whole history: `vivi sync pull --since <age>`
(`7d`, `12h`, `90m`, an RFC 3339 time, or `YYYY-MM-DD`) starts at that time. It
is refused once the mailspace has a cursor for the project. The **floor** is
where this mailspace's history starts: the first letter received (sequence and
server time), or the requested time when the window held nothing. A plain
first pull records full history. `vivi sync status` prints it as
`history from: ...`; a mailspace that pulled before windows existed reads
`unknown (pulled before windows)` and is never guessed to be full. A letter
older than the floor is not here, so a reply to one stays unlinked until it
is fetched.

**Server epoch.** The client accepts an optional `epoch` (an integer, 1 to
4294967295) on every list response; it is recorded in the sync state and
carried by new remote bindings. When it changes, the next pull says
`server epoch changed (A -> B)`, resets the cursor and floor, and re-reads
from sequence 0, filing nothing twice; a pull gives up after 2 resets in one
run. A server that sends no epoch behaves as before, and `vivi sync status`
shows the epoch. Wire contract: the list response carries a field named
`epoch`, an integer from 1 to 4294967295, in the same JSON object as
`records`, `next_cursor`, and `has_more`, and the server must change it
whenever its log is wiped or restored.

**Getting older letters.** `vivi sync resync` resets the cursor and floor to
zero and replays everything; letters already held are left alone, evicted ones
come back byte for byte with their local state. `vivi sync fetch <handle>...`
asks the server for letters by the 8 hex characters of their id (100 per
request; a handle that names nothing visible is reported as missing, not an
error) and files them through the same checks as a pull, without moving the
cursor or the floor. `vivi sync pull --refs [--depth N]` follows references
after a pull: an `X-Vivi-Depends-On` value, or a whole-word 8-hex token in the
subject or body, that names a letter this mailspace does not hold is looked up
and filed. Each extra hop (`--depth`, 1 to 3, default 1) scans only what the
previous hop filed; one invocation looks up at most 500 handles. A token that
matches nothing is ignored.

**Pruning.** `vivi sync prune --older-than 30d [--dry-run]` evicts letters
bound to the project that are dated before the cutoff, freeing the files. It
never evicts open needs, tasks, or wants (including your own `sent` copy of
open work handed to another mailspace), starred or absorbed rows, or letters
that never travelled. The rows and local state stay; `resync`, `fetch`, or
`pull --refs` restores a letter byte for byte. It needs no server or key and
runs only when asked. Until restored, `show` on an evicted handle reports
`message not found` and `graph audit` lists a done item's node as
`orphan_node`.

Not synced: memos, drafts, trash, rules, marks, and read flags. Closing and
reopening are not in this list: they travel as event letters (State as
letters). A letter is limited to 64 KiB, and a member may send 30 letters per
minute: a push that hits the limit stops, prints how many letters are left
unsent, exits non-zero, and the next run resumes. Mail is visible only to its
sender and the addressed mailspace; work items and mail to `postmaster` are
visible to every member.

### MCP bridge

`vivi mcp` is a thin stdio bridge to the project server's MCP endpoint
(`POST /v1/projects/{id}/mcp`). It speaks MCP on stdin/stdout
(newline-delimited JSON-RPC 2.0, the standard stdio transport) and forwards
each message to that endpoint unchanged. It is a pass-through: it does not
know the tool list, so `tools/list` and `tools/call` go to the server as they
are, and new server tools need no new bridge. Nothing but protocol messages is
written to stdout; diagnostics go to stderr; a notification gets no reply. A
pool slot needs only the binary, a server URL, a project id, and a credential:
no local mailspace, no sync, and the bridge never creates `.vivi/`. When the
server refuses a request after the one allowed renewal (or at once for an API
key), the bridge answers that request with a JSON-RPC error (code `-32000`,
`data.http_status`) instead of hanging, and keeps serving.

The server's Project MCP tools: `overview`, `list`, `show`, `thread`, `send`,
`reply`, `done`, `reopen`, `memo`, `graph`. Writes share the server's
30-per-minute-per-identity send budget; a tool failure is a result with
`isError: true`.

**Flags and environment.** `--server <URL>`, `--project-id <ID>`,
`--name <NAME>`, and `--key-file <PATH>`, each with a variable:
`VIVI_MCP_SERVER`, `VIVI_MCP_PROJECT_ID`, `VIVI_MCP_NAME`, `VIVI_MCP_KEY_FILE`.
A flag beats its variable, a variable beats the `[sync]` table. The global
`--project <ROOT>` still names a mailspace root (used only to find a `[sync]`
table); it is not the server project id. `VIVI_MCP_API_KEY` is read from the
environment only — there is no flag — so it does not show in a process list.

**Credentials** (either one):

1. A member name and a key file: `--name`/`VIVI_MCP_NAME` plus
   `--key-file`/`VIVI_MCP_KEY_FILE`, the 32-byte seed file a mailspace keeps
   at `.vivi/chat/identity.ed25519`. The bridge mints the identity token by
   the challenge, sign, token route (as `vivi sync` does), renews it once
   when the server refuses it (401), and renews ahead of time when
   `token_expires_at` is within 30 seconds.
2. A project API key in `VIVI_MCP_API_KEY`: sent as the bearer as it is; no
   minting, no renewal, a 401 is final.

Giving both is an error. With neither, inside a mailspace that has a `[sync]`
table, `vivi mcp` with no flags uses the `[sync]` server, project, and name
and the mailspace's own key, so a person can try it.

**Client configuration.** Claude Code takes a JSON file via `--mcp-config`:

```json
{"mcpServers":{"vivi":{"command":"vivi","args":["mcp"],"env":{"VIVI_MCP_SERVER":"https://<server>","VIVI_MCP_PROJECT_ID":"<project id>","VIVI_MCP_API_KEY":"<key from vivi sync api-key mint>"}}}}
```

The member-name variant:

```json
{"mcpServers":{"vivi":{"command":"vivi","args":["mcp","--name","<name>","--key-file","<path>"],"env":{"VIVI_MCP_SERVER":"https://<server>","VIVI_MCP_PROJECT_ID":"<project id>"}}}}
```

pi connects to stdio MCP servers through its `mcp.json`; the same `command`,
`args`, and `env` work there: `pi mcp add vivi --env
VIVI_MCP_SERVER='https://<server>' --env VIVI_MCP_PROJECT_ID='<project id>'
--env VIVI_MCP_API_KEY='<key from vivi sync api-key mint>' -- vivi mcp`.

**The pool bridge.** A pool can hand each dispatched worker a credential, so
the slot reads its task from the server and reports through the server's own
tools instead of publishing only a Git branch. Turn it on per pool with
`[pools.<name>.bridge] enabled = true`; there are no other settings — the
server and the project the grant is for come from the mailspace's own `[sync]`
table. A dispatch on a bridge pool mints exactly one project API key, labelled
`pool-<pool>-<job>`, through the dispatching mailspace's member identity,
after every refusal check and before a slot is claimed: a refused dispatch
mints nothing, and a dispatch that fails after the mint revokes its key at
once. The mint does talk to the `[sync]` server, so a dispatch on a bridge
pool needs it reachable and is refused before a slot is touched when it is
not. Only the key id is ever named: the raw key is never printed, never
stored, and never in the mailspace config.

Delivery is the job archive, like the Git token: the key is one root-only
file in the archive (`bridge.key`), not a member of the harness `env` list,
so it never appears on a command line. The runner reads it and hands the
harness process three environment variables — `VIVI_MCP_API_KEY` plus
`VIVI_MCP_SERVER` and `VIVI_MCP_PROJECT_ID` — and the agent sees the key only
there; no file it can read holds it. The archive also carries an
agent-visible `mcp.json`, the client configuration above with the key named
only by reference (`${VIVI_MCP_API_KEY}`), so the generated file holds no
secret; a harness template reaches it with the `{mcp_config}` placeholder,
and a template that uses the placeholder on a pool whose bridge is off is
refused. With a grant the brief is not the task text: it names the task
handle and the tools — `show` for the task, `thread` for the conversation,
`reply` with the receipt, `done` when the task says so — and the task text
stays on the server.

Two requirements. The `vivi` binary must exist on the slot's PATH, since the
generated `mcp.json` runs `vivi mcp`: a Docker or Fly image installs it in
`setup`, which runs as root once at seed time. And the task must have reached
the server before dispatch — run `vivi sync --drain` first; the dispatch
prints a reminder, since the worker reads the task from the server, not from
the archive.

The key does not outlive its job: it is revoked when its dispatch is
collected — exactly the job's own key, never a sibling job's, another pool's,
or a hand-minted one — and `pool destroy --all` revokes every live key still
labelled for the pool, so nothing a lost or uncollected job minted survives
the pool. A revoke that cannot be done only warns and names the manual
command (`vivi sync api-key revoke <key_id>`); it never fails the collect or
the destroy.

Say plainly what identity the bridge works under: every slot action through
it carries the minting member's identity, because the key was minted by that
member — the server cannot tell a hand's report from the Mind's close — and
per-role vouched members are a later refinement. The runner's own result is
recorded on the task at collect whether or not the worker reports through
the tools, so the fallback record exists even when the worker never reached
the server.

## Safety

- Use the CLI rather than underlying stores.
- Verify current help before exact mutations.
- Narrow reads before dumps and broad searches.
- Treat absorb as permanent.
- Do not infer a source repository when the user has not identified it.
- Report whether an operation was local or executed remotely.
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
