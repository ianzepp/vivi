# Work-graph flow: turning a delivery document into dispatched, ordered work

This is the end-to-end procedure a coordinating seat (a Tugboat Mind or any
host that dispatches Hands) follows with Vivi's native task-dependency graph and
`vivi step`. It is a workflow guide. The command reference is the parent
`SKILL.md`; flags change, so run `vivi <command> --help` before
relying on an exact spelling. Everything below was checked against Vivi 10.0.0
source (`src/mailspace/backlog.rs`, `backlog_audit.rs`, `step.rs`,
`graph.rs`, `graph_mutate.rs`) and the installed binary's help text.

Throughout, `$ROOT` is the project root and every command carries
`--project $ROOT`.

## 1. Data model

**One graph holds the work.** Each mailspace has one graph named `backlog`.
Every `task send`, `need send`, and `want send` mints one open node in it. The
node's id is the handle of the message that was delivered to the recipient (the
`created <recipient> <handle>` line the send prints). The `sent <id>` line is the
sender's own copy; it gets no node. Cite the `created` handle everywhere.
Nothing needs importing: sending the unit task is the node-creation step.

**Node kinds.** Work nodes are `task`, `need`, and `want`. Three gate kinds
(`decision`, `stub`, `parked`) exist for imported or hand-added topology; they
never dispatch and are resolved with `graph complete --note`.

**Node states.** `open`, `active`, `done`, plus `cancelled` and `superseded`.
Readiness is not stored. It is derived, and only for open nodes: an open node is
`ready` when every solid-edge prerequisite is `done`, otherwise `blocked`.
Active and done nodes show `n/a`. A prerequisite that is `cancelled` or
`superseded` is not `done`, so it blocks its dependents indefinitely; resolve
that by hand.

**Edges.** An edge `A -> B` means B waits on A. Solid edges gate; dotted edges
(Mermaid imports only) are evidence and never gate.

**`--depends-on`.** Repeatable on `task send`, `need send`, and `want send`.
Each cited handle must resolve to an open task, need, or want, or to a done item
of any kind. An unknown handle (or a mail/memo handle) fails the send before
anything is created. Each valid citation becomes one solid edge from the cited
item to the new one. If the cited item has no node yet, one is minted for it.
Citing something already done adds an edge that is satisfied at once, so the new
node is ready immediately. `X-Vivi-Depends-On` headers stay on the message as
evidence, but the graph is what readiness uses.

**`done` means the Hand closed the handle. It does not mean the code is on
main.** `task done` completes the node and unlocks its dependents the moment the
seat closes it. Vivi has no `merged` state and does not look at git.

**Blocked work is not hidden.** `task list --for <role>` lists every open task
and shows no dependency information. The graph view (`graph ready`,
`board --graph`, the boot verdict `blocked`) is authoritative for blocked-ness.
`task list --blocked` / `--blocking <handle>` read the dependency headers and
check only whether the cited task is in the done folder; they do not consult the
graph.

## 2. "On main" is a task: the merge-task gate

Because `done` is not "merged", encode landing as its own task and make
anything that needs the code depend on that task.

- File one **merge task** per unit that others will build on, addressed to the
  merge seat, with `--depends-on <unit handle>`. It becomes ready when the Hand
  closes the unit.
- The merge seat closes the merge task only after the unit's commit is on main,
  and records the main tip in the receipt
  (`task done --verdict ... --repo <repo> --tip <main sha>`). The coordinator
  confirms with `git merge-base --is-ancestor` before trusting it, because
  closing the merge task is what unlocks the dependents.
- Every dependent unit is filed with `--depends-on <merge task handle>`, not
  `--depends-on <unit handle>`. Citing the unit would unlock the dependent while
  the code is still on a branch, and a lane cut for the dependent starts from
  main, so it would not see the code it needs.
- A merge that fails is not closed. Report through the handle and file the
  repair; the dependents stay blocked, which is the point.

### 2b. Starting a dependent from a base lane (when available)

*Not released yet: this arrives with the next `hand-packet` release (the lane
tool of the Faber workspace, not part of Vivi). Until the coordinator confirms
it has landed, treat everything in this subsection as planned and use the merge
gate above.*

`hand-packet init <lane> --base-lane <other>` creates a lane for a dependent
unit whose repositories start at the tip of another lane's branch instead of
at main. Rules for using it:

- The base lane's Hand has closed its handle (`done`), so the base tip is
  final. Never base a lane on a lane whose Hand is still working.
- The dependent unit is its own handle on its own lane (this is not stacking:
  one unit per handle, one handle per lane). It is filed with `--depends-on`
  the **base unit's handle**, so it turns ready when the base Hand closes, with
  no wait for the merge.
- The dependent's **merge task** is filed with `--depends-on` the **base
  unit's merge task**, so the merge seat lands them in order and main receives
  the base before the dependent.
- Anything that needs the code on main (as opposed to on the base branch) still
  depends on a merge task.

## 3. Brief body format that `vivi step` accepts

`step` reads the first matching lines of the body text. Write each field on its
own line, label first:

```text
unit: IB-2
repo: radix
write_scope: crates/foo/src/a.rs, crates/foo/src/b.rs
edit: <file + seam + what changes>
done_when: <one falsifiable oracle command or observable>
sanity: <one cheap check>
do_not: <fences>
```

Matching rules, from `step.rs`:

- A clause is a line that, after leading whitespace is ignored and case is
  ignored, begins exactly with `done_when:` (or `write_scope:` for the scope
  flag). Several `done_when:` lines are several clauses; each counts.
- A bullet (`- done_when: ...`), bold (`**done_when**:`), a space before the
  colon, or a label that is not first on its line does not match. The compressed
  one-line form `write_scope / edit / done_when / sanity / do_not: ...` does not
  match either, because the line does not begin with `done_when:`.
- `task send` and `need send` print `note: body declares no 'done_when:'
  clause` to stderr when none is found. Treat that note as a defect to fix
  before dispatch.

What `step` does with each **ready** backlog node (it never looks at blocked or
active ones), checked in this order:

| Outcome | Condition |
|---|---|
| exception `item_missing` | no message resolves for the node's id |
| exception `want_requires_promotion` | the item sits in the wants folder (decided by folder, not by the node's kind); wants never dispatch before `want promote` |
| exception `lowered_awaiting_units` | units are bound to this node with `need bind`; its completion comes from them |
| exception `no_done_when` | the body has no `done_when:` line |
| **dispatch** | none of the above. The row carries the node, item handle, kind, subject, the count of `done_when:` clauses, and `write_scope: true/false` |

`write_scope:` is reported, not required, for a dispatch. A project that
requires it (the Faber workspace does) enforces that itself.

Two more exceptions come only from `step --apply <handle>`:

| Exception | Condition |
|---|---|
| `not_settled` | the handle is not in the done folder yet; apply never settles work |
| `untracked_item` | the item is done but has no backlog node (see section 7) |

`step --json` prints `dispatches[]`, `exceptions[]`, and `decisions[]`.
`decisions` is filled only in apply mode.

## 4. Order of operations: delivery document to running seats

For each unit, in this order:

1. **File** the unit task with `--depends-on` for every real prerequisite and a
   body in the format above. Remember that a prerequisite that must be on main
   is the merge task, not the unit. File prerequisites first: you need their
   handles. Capture the handle from the `created` line:
   `H=$(vivi task send ... | awk '/^created /{print $3}')`.
2. **Bind the need** that the delivery serves:
   `vivi need bind <need> <unit and merge task handles...>`. The need completes
   by itself, node and mailbox together, when every bound item is done, and
   reopening a bound item reopens the need. Bind units and their merge tasks, so
   the need closes when the work has landed and not merely when Hands finished.
   Bind everything at lowering time: binding needs the need to be open, and it
   can be repeated to add more, but a need whose bound items are all done has
   already completed. A need must have kind `need` as a node; a promoted want
   keeps its `want` node kind and `need bind` refuses it until
   `graph audit --repair` corrects the kind (section 5).
3. **Prepare and claim the lane** with the project's lane tool (in the Faber
   workspace: refresh, then `hand-packet claim <lane> --handle <handle>`). Do
   this before activating, so a refused claim leaves the node ready and
   unstarted.
4. **Activate**: `vivi graph activate <handle> --task <handle>`. The node must
   be open, not a gate kind, and ready; the task must be in a tasks folder.
   Activation marks the node active (it leaves `step`, so it is not offered
   again), and records the task's content hash. Activating a blocked node is
   refused, so the graph itself prevents dispatching a unit before its
   prerequisites are done.
5. **Spawn** the seat with the pointer prompt (role + handle only).
6. When the Hand closes the handle, run **`vivi step --apply <handle>`**. It
   is idempotent: it completes the node if the lifecycle close
   somehow left it open, otherwise records that it was already settled, and it
   never settles anything itself.
7. **When a merge lands**: the merge seat closes the merge task (after main
   has the commit), the coordinator checks ancestry, runs
   `vivi step --apply <merge handle>`, and the dependents of the merge task turn
   ready. Dispatch each one from step 3.

To find the next work, run `vivi step --json` after boot. `dispatches` is the
ready, verifiable work; spawn one seat per row, within lane and capacity
limits. `exceptions` is the list of things that need a human or a body fix.

## 5. Command reference

| Command | Use |
|---|---|
| `task send --depends-on H ...` | file a unit; mint its node; wire prerequisite edges |
| `graph connect <dependent> <prereq>` | add a missed prerequisite after the fact. Idempotent. Refuses a self edge, a dependent that is already active or done, and a handle with no node. It does not check for cycles; never connect a prerequisite that already depends on the dependent, or both block forever |
| `graph ready [graph] [--kind K]` | frontier: ready, blocked, active, gates, with a counts line. No argument lists every graph |
| `graph show <graph> [--include-state]` / `graph export` | topology as Mermaid |
| `board --graph` | frontier on the board |
| `graph activate <node> --task <handle>` | bind an attempt; mark the node active. A bare id addresses `backlog`; a named graph needs `graph:source-id` or `--graph` |
| `graph complete <node> [--note]` | mark done by hand (also the way to resolve a gate). Prefer the lifecycle close plus `step --apply` for work items |
| `step [--json]` | dispatch and exception manifest for ready backlog nodes |
| `step --apply <handle>` | after a close, confirm and complete; see section 4 |
| `need bind <need> <units...>` | attach units to a need; the need joins when all are done |
| `graph audit [--json]` | read-only check of backlog citizenship |
| `graph audit --repair` | repair drift; see below |
| `graph import --code C --file F.mmd [--check]` / `graph apply <C> --file` | Mermaid topology for non-item graphs only |
| `graph node add` / `graph edge add --graph G ...` | per-node edits of an imported graph |

**Mermaid import and apply** are for topology that is not work items (a
pipeline, an environment flow). Delivery units are task sends with
`--depends-on` and `need bind`, never an imported graph, so that one piece of
work is not represented twice. Imported graphs do not appear in `step`.

**`graph audit`** compares every work item (tasks, needs, wants, and done,
content-deduplicated to its lowest message id) that was updated since the
backlog graph was created against the graph. It reports:

- `missing_node`: the item exists and has no node (sent by a binary that did
  not mint, or the mint failed).
- `node_kind`: the node's kind differs from the folder's kind (open items
  only), commonly a promoted want whose node is still `want`.
- `node_state`: the node and the folder disagree on done versus not done.
- `orphan_node`: a minted node whose message no longer resolves (deleted or
  trashed). Reported only; never repaired.

**`graph audit --repair`** acts on the first three. It writes only graph rows
and, in one narrow case, a need's mailbox copies; it never edits a message's
content or headers, never deletes a node, never moves a task or want between
folders, and never touches wants' promotion state.

- `missing_node`: creates the node (label = subject, kind taken from the
  folder/header) and one solid edge per `X-Vivi-Depends-On` header that still
  resolves. Unresolvable dependency citations are dropped silently. If the item
  is in the done folder, the new node is then completed with `via=repair`, which
  can unlock its successors. It also completes the node's siblings and join
  parent the same way a normal close does.
- `node_kind`: rewrites that node's kind to the folder's kind. Effects: a
  promoted want becomes eligible for `need bind` and for `graph ready --kind
  need`. `step` already reads the folder, so its output does not change.
- `node_state`: the folder wins. Done folder with an open or active node:
  completes the node (`via=repair`). Open folder with a done node: sets the node
  back to open, without cascading to a join parent. That reverses an earlier
  hand completion with `graph complete` and re-blocks dependents that were not
  yet activated. The exception is a need whose bound units are all done: then it
  settles the need's mailbox copies from `needs` to `done` instead (`via=repair`).

The run is idempotent (a second run finds nothing it already fixed), purely
additive for `missing_node`, and scales linearly in the number of items; it
reads each candidate message once. It is not one transaction: each fix commits
on its own, so an error mid-run leaves the earlier fixes in place and a re-run
continues. Run `graph audit --json` first, read the `node_state` findings
especially (they are the only class that can undo something), then repair.

## 6. Worked examples

Commands assume `export PATH=/opt/homebrew/bin:$PATH` and `R=$ROOT`.

### Three-way fan-out behind one branch

IB-1 is done by its Hand (handle `$IB1`) and its commit sits on a lane branch.
IB-2, IB-3, and IB-4 need IB-1's code and are independent of one another.

```sh
# 1. merge task for IB-1: depends on the unit, goes to the merge seat
M1=$(vivi task send --project $R --from mind --to merge \
  --subject 'merge: IB-1 into radix main' --depends-on $IB1 \
  --body-file m1.md | awk '/^created /{print $3}')
#   m1.md carries, at line starts:
#     write_scope: (none; merge)
#     done_when: git -C radix merge-base --is-ancestor <ib1 tip> main exits 0

# 2. the three dependents each cite only the merge task
for U in IB-2 IB-3 IB-4; do
  vivi task send --project $R --from mind --to hand \
    --subject "$U: <short title>" --depends-on $M1 \
    --body-file "$U.md"      # bodies have done_when: and write_scope: lines
done                         # note each created handle: $IB2 $IB3 $IB4

# 3. lower the need so it completes when all of it has landed
vivi need bind --project $R $NEED $IB1 $M1 $IB2 $IB3 $IB4

# 4. check: M1 ready; IB-2..IB-4 blocked on M1
vivi graph ready --project $R --kind task

# 5. dispatch the merge: claim the merge lane, activate, spawn
vivi graph activate --project $R $M1 --task $M1

# 6. merge lands; the merge seat closes M1 with the main tip; then
git -C <repo> merge-base --is-ancestor <ib1 tip> main   # coordinator verifies
vivi step --project $R --apply $M1
vivi graph ready --project $R --kind task               # IB-2..IB-4 now ready

# 7. three dedicated lanes cut from the new main; per unit, once:
#    (claim lane for $IBn) then
vivi graph activate --project $R $IB2 --task $IB2       # likewise IB3, IB4
#    spawn each seat; after each closes:
vivi step --project $R --apply $IB2
```

### Serial chain: VG-3 after VG-2

Each unit has its own handle and its own lane. The later unit waits on the
earlier unit's merge task.

```sh
V2=$(vivi task send --project $R --from mind --to hand --subject 'VG-2: ...' \
  --body-file vg2.md | awk '/^created /{print $3}')
MV2=$(vivi task send --project $R --from mind --to merge \
  --subject 'merge: VG-2 into radix main' --depends-on $V2 \
  --body-file mv2.md | awk '/^created /{print $3}')
V3=$(vivi task send --project $R --from mind --to hand --subject 'VG-3: ...' \
  --depends-on $MV2 --body-file vg3.md | awk '/^created /{print $3}')
MV3=$(vivi task send --project $R --from mind --to merge \
  --subject 'merge: VG-3 into radix main' --depends-on $V3 \
  --body-file mv3.md | awk '/^created /{print $3}')
vivi need bind --project $R $NEED $V2 $MV2 $V3 $MV3
```

Timeline: V2 is ready and dispatched (claim, activate, spawn). When its Hand
closes, `step --apply $V2` and MV2 turns ready; the merge lands and MV2 closes;
only then V3 turns ready, and its lane is cut from a main that contains VG-2.
Add earlier or later links the same way (VG-1's merge task is what VG-2 would
cite). The cost is merge latency on every link of a chain; merge the blocking
branch of a chain promptly, ahead of work nothing waits on.

## 7. Known gaps

- **No merged state.** `done` is the Hand's close. Landing is the merge task by
  convention only; Vivi does not verify it, so the closer of a merge task is
  trusted until the coordinator checks git.
- **Stacking is invisible.** Several units delivered under one handle are not
  separate nodes, so nothing orders or tracks them. Do not stack; give each unit
  its own handle, node, and lane.
- **A dependent cannot start before main has its prerequisite, today.** The
  only sequencing Vivi records is "closed" (`done`) and, by convention, "merged"
  (a merge task). Starting a dependent from a base lane is a lane-tool feature
  (section 2b), planned and not yet released; Vivi records it only through the
  `--depends-on` edges above.
- **One handle, one lane lock.** A lane is claimed against one handle. A
  stacked chain cannot be handed to a second handle's lane, which is another
  reason not to stack.
- **`graph connect` has no cycle check**, and a cancelled or superseded
  prerequisite blocks its dependents forever. Both are manual repairs.
- **Promotion does not update node kind.** After `want promote`, the node is
  still a `want` until `graph audit --repair`; `need bind` refuses it meanwhile.
- **Items without nodes.** Sends made by an older binary (or when the mint
  failed) have no node. `graph activate`, `need bind`, `graph connect`, and
  `step --apply` then fail or report `untracked_item`, and each error names
  `graph audit --repair`. A stale `vivi` earlier on `PATH` is the usual cause;
  `mailspace status` prints the running binary's version.
- **Board hygiene.** Run `graph audit` periodically and keep the counts of
  `missing_node` and `node_kind` findings at zero. A backlog with drift gives
  step and readiness answers that are quietly wrong. Record the counts in the
  project's own agent docs, not here.
- **Dual handles.** A send creates a sender copy and a recipient copy. Only the
  recipient copy has a node. Edges canonicalize to one copy per content, and a
  multi-recipient item is one work item, but always cite the `created` handle.
