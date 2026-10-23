# Work-graph flow: turning a delivery document into dispatched, ordered work

This is the end-to-end procedure a coordinating seat (the Mind of a Tugboat or
Speedboat run, or any host that dispatches Hands) follows with Vivi's native
task-dependency graph and `vivi step`. It is a workflow guide. The command
reference is the parent `SKILL.md`; flags change, so run `vivi <command>
--help` before relying on an exact spelling.

The letter rules below (front matter for `done_when`, `write_scope` and
`depends_on`, the `step` reasons, the audit's edge reading) match Vivi 21
(`src/letter/`, `src/mailspace/step.rs`, `dependencies.rs`, `backlog_audit.rs`).
The rest of the graph and lifecycle material (the non-pool parts of sections 1
to 7) was checked against Vivi 10.0.0 source (`src/mailspace/backlog.rs`,
`graph.rs`, `graph_mutate.rs`) and the installed binary's help text. The lane
and pool material (2b, the pool forks in 4, the two-pool example in 6, and the pool
gaps in 7) was checked against the parent skill's pool section and `src/pool`
as of Vivi 10.2.0.

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
Readiness is not stored. It is derived, and only for open nodes: an open node
is `ready` when every solid-edge prerequisite is `done`, otherwise `blocked`.
Active and done nodes show `n/a`. An open or active node whose work a role
reported done and nobody who may accept has accepted reads `reported`; it is
never ready and its dependents stay `blocked`. A prerequisite that is
`cancelled` or `superseded` is not `done`, so it blocks its dependents
indefinitely; resolve that by hand.

**Edges.** An edge `A -> B` means B waits on A. Solid edges gate; dotted edges
(Mermaid imports only) are evidence and never gate.

**`--depends-on`.** Repeatable on `task send`, `need send`, and `want send`.
Each cited handle must resolve to an open task, need, or want, or to a done item
of any kind. An unknown handle (or a mail/memo handle) fails the send before
anything is created. Each valid citation becomes one solid edge from the cited
item to the new one. If the cited item has no node yet, one is minted for it.
Citing something already done adds an edge that is satisfied at once, so the new
node is ready immediately. The citations are stored in the letter's front
matter `depends_on` list (the flag builds it; there is no header), and the
graph is what readiness uses. A citation to something this mailspace does not
hold yet is kept as a pending reference and its edge is made when that letter
arrives.

**A Hand's `done` is a report, not an acceptance.** `task done` by a role that
is neither the item's author (or `sent`-copy holder) nor of kind `mind` moves
only that role's copy: the node stays open or active, its dependents stay
blocked, no join fires and no event is written. The author, the `sent`-copy
holder, or a mind running `task done <handle> --for <acceptor>` accepts it, and
only then does the node complete and its dependents unlock. **Accepted does
not mean the code is on main.** Vivi has no `merged` state and does not look
at git.

**Blocked work is not hidden.** `task list --for <role>` lists every open task
and shows no dependency information. The graph view (`graph ready`,
`board --graph`, the boot verdict `blocked`) is authoritative for blocked-ness.
`task list --blocked` / `--blocking <handle>` read the front matter
`depends_on` list and do not consult the graph. A dependency counts as met only
when this mailspace holds it and it is done, so a reference that is not held
yet still blocks.

## 2. "On main" is a task: the merge-task gate

Because `done` is not "merged", encode landing as its own task and make
anything that needs the code depend on that task.

- File one **merge task** per unit that others will build on, addressed to the
  merge seat, with `--depends-on <unit handle>`. It becomes ready when the
  unit is accepted.
- The merge seat reports the merge task only after the unit's commit is on
  main, and records the main tip in the receipt
  (`task done --verdict ... --repo <repo> --tip <main sha>`). The coordinator
  confirms with `git merge-base --is-ancestor` before accepting it
  (`task done <merge handle> --for <mind>`), because accepting the merge task
  is what unlocks the dependents.
- Every dependent unit is filed with `--depends-on <merge task handle>`, not
  `--depends-on <unit handle>`. Citing the unit would unlock the dependent while
  the code is still on a branch, and a lane cut for the dependent starts from
  main, so it would not see the code it needs.
- A merge that fails is not accepted. Report through the handle and file the
  repair; the dependents stay blocked, which is the point.

### 2b. Starting a dependent from a base branch

*The uncommon case: the merge gate above is the default.*

A pool dispatch can start a dependent unit at the tip of another unit's branch
instead of at main: pass `--from <base branch> --branch <new branch>`. The new
branch is created at the base branch's tip (the parent skill calls this
stacking); the base branch is never moved, reset or forced. Rules for using it:

- The base unit's job has been collected (`wait` or `reconcile` recorded
  `pushed <branch> <sha>` or `committed <branch> <sha>`) and its handle is
  accepted, so the base tip is final. Never stack on a branch whose job is still
  running.
- The dependent unit is its own handle on its own dispatch (this is not
  stacking work under one handle: one unit per handle, one handle per
  dispatch). It is filed with `--depends-on` the **base unit's handle**, so it
  turns ready when the base is accepted, with no wait for the merge.
- The dependent's **merge task** is filed with `--depends-on` the **base
  unit's merge task**, so the merge seat lands them in order and main receives
  the base before the dependent.
- Anything that needs the code on main (as opposed to on the base branch)
  still depends on a merge task.

A host-spawned seat in a lane from any other lane tool follows the same
rules if that tool can start a lane at another lane's branch tip.

## 3. The unit letter that `vivi step` accepts

Every need, want and task is a letter: TOML front matter between `+++` lines,
then Markdown prose. `step` reads `done_when` and `write_scope` from the front
matter only. Lines such as `done_when: ...` in the prose are free text that
nothing reads. Build the front matter with flags:

```sh
vivi task send --project $ROOT --from mind --to hand \
  --subject 'unit IB-2: reject malformed input' \
  --body-file ib-2.md \
  --done-when 'the validate test target passes' \
  --done-when 'malformed input is rejected with the expected error' \
  --write-scope crates/parser/src/validate.rs \
  --write-scope crates/parser/tests/validate.rs
```

or write the whole letter in the body file. A body whose first non-blank line is
`+++` is taken as the whole letter, and a front matter flag beside it is
refused:

```text
+++
done_when = ["the validate test target passes", "malformed input is rejected with the expected error"]
write_scope = ["crates/parser/src/validate.rs", "crates/parser/tests/validate.rs"]
+++

unit: IB-2
repo: <repo name>
edit: <file + seam + what changes>
sanity: <one cheap check>
do_not: <fences>
```

The prose fields (`unit`, `repo`, `edit`, `sanity`, `do_not`) are for the Hand
who reads the letter; vivi does not parse them. The rules:

- A clause is one entry of the `done_when` list. Write each as one falsifiable
  oracle command or observable. `write_scope` is a list of paths.
- A need without `done_when` is refused at send and nothing is stored
  ("need has no done_when: pass --done-when or add done_when = [...] to the
  front matter"). A task may send without one; `step` then reports
  `no_done_when`.
- A letter with no front matter is history. `show` and `list` read it, `step`
  reports it as `letter_invalid`, and it cannot be executed: send a new letter.
- A task's `from`, `branch`, `with` and `band` have no flags; put them in a
  whole-letter body.

What `step` does with each **ready** backlog node (it never looks at blocked or
active ones, except reported work, below), checked in this order:

| Outcome | Condition |
|---|---|
| exception `item_missing` | no message resolves for the node's id |
| exception `want_requires_promotion` | the item sits in the wants folder (decided by folder, not by the node's kind); wants never dispatch before `want promote` |
| exception `letter_invalid` | the letter is in the old format (no front matter) or its front matter is invalid; the detail carries the reader's error |
| exception `pump_owned` | a task with a `[step]` front matter table: the pump runs and closes it, so `step` never dispatches it |
| exception `lowered_awaiting_units` | units are bound to this node with `need bind`; its completion comes from them |
| exception `no_done_when` | the front matter `done_when` list is empty (in practice a task; a need cannot be stored without one) |
| **dispatch** | none of the above. The row carries the node, item handle, kind, subject, the count of `done_when` entries, and `write_scope: true/false` (true when the front matter `write_scope` list is non-empty) |

`write_scope` is reported, not required, for a dispatch. A project that
requires it enforces that itself.

Reported work is not a dispatch candidate: an open or active node that reads
`reported` comes out of `step` as exception `awaiting_acceptance`, whose
detail names who reported and the `task done <handle> --for <acceptor>`
command that accepts it. It is checked before the rows above, and
`step --apply <handle>` on a reported handle completes nothing and returns it.

Two more exceptions come only from `step --apply <handle>`:

| Exception | Condition |
|---|---|
| `not_settled` | the handle is not in the done folder yet; apply never settles work |
| `untracked_item` | the item is done but has no backlog node (see section 7) |

`step --json` prints `dispatches[]`, `exceptions[]`, and `decisions[]`.
`decisions` is filled only in apply mode.

## 4. Order of operations: delivery document to running seats

For each unit, in this order:

1. **File** the unit task with `--depends-on` for every real prerequisite and
   its `--done-when` / `--write-scope` flags (or a whole-letter body, section 3). Remember that a prerequisite that must be on main
   is the merge task, not the unit. File prerequisites first: you need their
   handles. Capture the handle from the `created` line:
   `H=$(vivi task send ... | awk '/^created /{print $3}')`.
2. **Bind the need** that the delivery serves: `vivi need bind <need> <unit
   and merge task handles...>`. The need completes by itself, node and mailbox
   together, when every bound item is accepted (a report does not count), and
   reopening a bound item reopens the need. Bind units and their merge tasks,
   so the need closes when the work has landed and not merely when Hands
   reported. Bind everything at lowering time: binding needs the need to be
   open, and it can be repeated to add more, but a need whose bound items are
   all accepted has already completed. A need must have kind `need` as a node;
   a promoted want keeps its `want` node kind and `need bind` refuses it until
   `graph audit --repair` corrects the kind (section 5). For a need assigned
   from another mailspace, assignment sync makes the foreign need a local
   backlog node in the receiving mailspace; run the same bind there to lower
   it onto that member's local tasks. The bound letter and join close sync
   back to the authoring mailspace, where the need's folder and graph state
   follow the close event.
3. **Choose how the unit runs, then prepare it.** There are two forks.
   - **A. A host-spawned seat in a lane.** The coordinator spawns the Hand
     itself, as a sub-agent of its own harness, in a lane the project's lane
     tool prepares (a Git worktree manager, for example). Refresh, then claim
     the lane for the handle. Do this before activating, so a refused claim
     leaves the node ready and unstarted. `vivi pool` is not this tool: it
     never hands a seat a path.
   - **B. A pool-dispatched worker.** `vivi pool dispatch --pool <name> <handle>
     --model <slug> --from <ref> [--branch <name>]` claims a slot and starts the
     worker in one step. The worker is the pool's configured `harness.run`
     command, not a sub-agent of the coordinator. A refusal (no free slot, a
     `--model` outside the pool's allowlist, a branch the worktree rules refuse)
     claims nothing and costs nothing, so dispatch first and activate right
     after. Dispatch does not touch the graph, and until activation `vivi step`
     still offers the node: do not also spawn it by hand. The pool itself
     refuses to dispatch a task that has an open dispatch.
4. **Activate**: `vivi graph activate <handle> --task <handle>`. The node must
   be open, not a gate kind, and ready; the task must be in a tasks folder.
   Activation marks the node active (it leaves `step`, so it is not offered
   again), and records the task's content hash. Activating a blocked node is
   refused, so the graph itself prevents dispatching a unit before its
   prerequisites are accepted; a reported node is refused too, with the
   command that accepts it.
5. **Spawn** the seat with the pointer prompt (role + handle only). In fork B
   the dispatch was the spawn: the brief is the task body, the pool's rules file
   (`harness.rules`) plays the role charter's part, and the worker's standard
   output is its receipt.
6. **Settle.** In fork A, when the Hand reports the handle (`task done`, which
   only reports), the coordinator reads the receipt (`vivi task show`), accepts
   the unit with **`vivi task done <handle> --for <mind>`**, and runs **`vivi
   step --apply <handle>`**. Until then the node reads `reported` and `step`
   lists it as `awaiting_acceptance`. In fork B, `vivi pool wait` (run it in
   the background) collects the finished job: it records the receipt and push
   outcome as a reply on the task and frees the slot, and it never closes the
   task. Read the receipt, integrate the branch, accept the task with `vivi
   task done --for <mind>`, then run `vivi step --apply <handle>`. `step
   --apply` is idempotent: it completes the node only if the acceptance somehow
   left it open, otherwise records that it was already settled, and it never
   settles anything itself, a reported item least of all.
7. **When a merge lands**: the merge seat reports the merge task (after main
   has the commit), the coordinator checks ancestry, accepts it with
   `vivi task done <merge handle> --for <mind>`, runs
   `vivi step --apply <merge handle>`, and the dependents of the merge task turn
   ready. Dispatch each one from step 3.

To find the next work, run `vivi step --json` after boot. `dispatches` is the
ready, verifiable work; spawn or dispatch one seat per row, within lane and
capacity limits. `exceptions` is the list of things that need a human or a body fix.

## 5. Command reference

| Command | Use |
|---|---|
| `task send --depends-on H ...` | file a unit; mint its node; wire prerequisite edges |
| `graph connect <dependent> <prereq>` | add a missed prerequisite after the fact. Idempotent. Refuses a self edge, a dependent that is already active or done, and a handle with no node. It does not check for cycles; never connect a prerequisite that already depends on the dependent, or both block forever |
| `need\|task\|want depend <handle> --add H... --remove H... --for ROLE` | change an item's prerequisites after the send. Written as `dependency-added` / `dependency-removed` letters, so every member applies it; the newest letter per dependency wins, over the send-time `--depends-on` too. Author, mind, or operator only; unknown handles, self edges and cycles fail before anything is written. `graph connect` stays local and has no remove |
| `graph ready [graph] [--kind K]` | frontier: ready, blocked, active, awaiting, reported, gates, with a counts line. No argument lists every graph |
| `graph blocked [graph] [--kind K] [--json]` | each blocked node with the prerequisites it still waits on (`blocked_by`); defaults to `backlog` |
| `graph show <graph> [--include-state]` / `graph export` | topology as Mermaid |
| `board --graph` | frontier on the board |
| `graph activate <node> --task <handle>` | bind an attempt; mark the node active. Refuses a blocked, gate, awaiting or reported node. A bare id addresses `backlog`; a named graph needs `graph:source-id` or `--graph` |
| `graph complete <node> [--note]` | mark done by hand (also the way to resolve a gate, and a manual override of an unaccepted report). Prefer accepting with `task done` as author or mind, plus `step --apply`, for work items |
| `step [--json]` | dispatch and exception manifest for ready backlog nodes |
| `step --apply <handle>` | after an acceptance, confirm and complete if still open; a reported handle returns `awaiting_acceptance`; see section 4 |
| `need bind <need> <units...>` | attach units to a need; the need joins when all are accepted |
| `pool dispatch` / `wait` / `reconcile` | run a unit in a pool slot and collect it; neither `wait` nor `reconcile` closes the task. See the parent `SKILL.md`, "Remote lane pool" |
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
- `node_state`: the node and the folder disagree on done versus not done. A
  report awaiting acceptance (done folder, open or active node, no accepting
  `done`) is not a disagreement and is never reported or repaired.
- `missing_edge`: a held open work letter's front matter `depends_on` names
  held work the graph has no edge from (a dependency that arrived after the
  dependent, or a letter filed by a route that did not link it).
- `foreign_held`: a node still open or active for work held under another
  mailspace's role. That work is not this mailspace's backlog.
- `event_state`: an item's folders disagree with the latest `done`/`reopened`
  event letter held for it (a checkout restored from before the event, or an
  event filed by a binary that did not apply it).
- `orphan_node`: a minted node whose message no longer resolves (deleted,
  trashed, or evicted by `sync prune`; a fetch restores the letter and clears
  it). Reported only; never repaired.

**`graph audit --repair`** acts on every class but `orphan_node`. It writes only
graph rows and, in two narrow cases, mailbox copies (a need's, and an
`event_state` item's); it never edits a message's content or headers, never
deletes a node, never moves a task or want between folders except to apply an
event letter it already holds (`event_state`), and never touches wants'
promotion state.

- `missing_node`: creates the node (label = subject, kind taken from the
  folder/letter) and one solid edge per front matter `depends_on` entry that
  resolves to held work. A dependency not held yet is kept as a pending
  reference, not dropped, and its edge is made when that letter arrives. If the
  item is in the done folder and is not a report awaiting acceptance, the new
  node is then completed with `via=repair`, which can unlock its successors.
  It also completes the node's siblings and join parent the same way a normal
  close does.
- `node_kind`: rewrites that node's kind to the folder's kind. Effects: a
  promoted want becomes eligible for `need bind` and for `graph ready --kind
  need`. `step` already reads the folder, so its output does not change.
- `node_state`: the folder wins. Done folder with an open or active node (and
  no report awaiting acceptance): completes the node (`via=repair`). Open
  folder with a done node: sets the node back to open, without cascading to a
  join parent. That reverses an earlier manual `graph complete` and re-blocks
  dependents that were not yet activated. The exception is a need whose bound units are all done: then it
  settles the need's mailbox copies from `needs` to `done` instead (`via=repair`).
- `missing_edge`: makes the edges the front matter names, the way an arriving letter
  would. Dependencies not yet held stay remembered until they arrive.
- `foreign_held`: completes the node (`via=repair`); the letter stays held.
- `event_state`: re-applies the latest event letter: the item's folders move to
  match it and the node follows, as on arrival.

The run is idempotent (a second run finds nothing it already fixed), purely
additive for `missing_node`, and scales linearly in the number of items; it
reads each candidate message once. It is not one transaction: each fix commits
on its own, so an error mid-run leaves the earlier fixes in place and a re-run
continues. Run `graph audit --json` first, read the `node_state` findings
especially (they are the only class that can undo something), then repair.

## 6. Worked examples

Commands assume `R=$ROOT`.

### Three-way fan-out behind one branch

IB-1 is accepted (its Hand reported handle `$IB1`, the Mind accepted it) and
its commit sits on a lane branch. IB-2, IB-3, and IB-4 need IB-1's code and
are independent of one another.

```sh
# 1. merge task for IB-1: depends on the unit, goes to the merge seat
M1=$(vivi task send --project $R --from mind --to merge \
  --subject 'merge: IB-1 into main' --depends-on $IB1 \
  --done-when 'git -C <repo> merge-base --is-ancestor <ib1 tip> main exits 0' \
  --body-file m1.md | awk '/^created /{print $3}')
#   a merge changes no files, so it takes no --write-scope

# 2. the three dependents each cite only the merge task
for U in IB-2 IB-3 IB-4; do
  vivi task send --project $R --from mind --to hand \
    --subject "$U: <short title>" --depends-on $M1 \
    --done-when "$(cat "$U.oracle")" --write-scope "$(cat "$U.path")" \
    --body-file "$U.md"      # the flags build the front matter; $U.md is prose only
done                         # note each created handle: $IB2 $IB3 $IB4

# 3. lower the need so it completes when all of it has landed
vivi need bind --project $R $NEED $IB1 $M1 $IB2 $IB3 $IB4

# 4. check: M1 ready; IB-2..IB-4 blocked on M1
vivi graph ready --project $R --kind task

# 5. dispatch the merge: claim the merge lane, activate, spawn
vivi graph activate --project $R $M1 --task $M1

# 6. merge lands; the merge seat reports M1 with the main tip; then
git -C <repo> merge-base --is-ancestor <ib1 tip> main   # coordinator verifies
vivi task done --project $R --for mind $M1              # accept: dependents unlock
vivi step --project $R --apply $M1
vivi graph ready --project $R --kind task               # IB-2..IB-4 now ready

# 7. three dedicated lanes cut from the new main; per unit, once:
#    (claim lane for $IBn) then
vivi graph activate --project $R $IB2 --task $IB2       # likewise IB3, IB4
#    spawn each seat; after each reports, accept it, then:
vivi task done --project $R --for mind $IB2
vivi step --project $R --apply $IB2
```

### Serial chain: VG-3 after VG-2

Each unit has its own handle and its own lane. The later unit waits on the
earlier unit's merge task.

```sh
V2=$(vivi task send --project $R --from mind --to hand --subject 'VG-2: ...' \
  --done-when '<oracle for VG-2>' --write-scope <path> --body-file vg2.md | awk '/^created /{print $3}')
MV2=$(vivi task send --project $R --from mind --to merge \
  --subject 'merge: VG-2 into main' --depends-on $V2 \
  --done-when '<VG-2 tip is an ancestor of main>' --body-file mv2.md | awk '/^created /{print $3}')
V3=$(vivi task send --project $R --from mind --to hand --subject 'VG-3: ...' \
  --depends-on $MV2 --done-when '<oracle for VG-3>' --write-scope <path> \
  --body-file vg3.md | awk '/^created /{print $3}')
MV3=$(vivi task send --project $R --from mind --to merge \
  --subject 'merge: VG-3 into main' --depends-on $V3 \
  --done-when '<VG-3 tip is an ancestor of main>' --body-file mv3.md | awk '/^created /{print $3}')
vivi need bind --project $R $NEED $V2 $MV2 $V3 $MV3
```

Timeline: V2 is ready and dispatched (claim, activate, spawn). When its Hand
reports, the Mind accepts V2 (`task done $V2 --for mind`), runs `step --apply
$V2`, and MV2 turns ready; the merge lands and the Mind accepts MV2; only then
V3 turns ready, and its lane is cut from a main that contains VG-2.
Add earlier or later links the same way (VG-1's merge task is what VG-2 would
cite). The cost is merge latency on every link of a chain; merge the blocking
branch of a chain promptly, ahead of work nothing waits on.

### Remote lanes for implementation, a local pool for integration

One scalable arrangement uses two named pools in one mailspace: `lanes` (a
remote provider with many slots) runs Hand implementations, and `local`
(`provider = "local"` with `worktree_of`, so each slot is a worktree of the
source repository, with a persistent `build_dir`) runs merges and acceptance
against a warm build. This is a convention, not a feature: Vivi has no
role-to-pool routing, the orchestrator picks `--pool`, and `--pool` is required
once there is more than one pool. A pool's `models` allowlist limits which
slugs that pool accepts.

```sh
# 1. Implementation runs on the remote pool; the worker pushes a branch.
vivi pool dispatch --project $R --pool lanes $IB1 --model <slug> \
  --from main --branch u/ib-1
vivi graph activate --project $R $IB1 --task $IB1
vivi pool wait --project $R --pool lanes $IB1   # prints `pushed u/ib-1 <sha>`
vivi task show --project $R $IB1                # read the receipt
vivi task done --project $R --for mind $IB1 --note 'collected u/ib-1'
vivi step --project $R --apply $IB1             # the merge task turns ready

# 2. The remote is the bus between pools: fetch before integrating locally.
git -C <repo> fetch origin u/ib-1
vivi pool dispatch --project $R --pool local $M1 --model <slug> \
  --from main --branch merge/ib-1               # brief: merge u/ib-1, run the gate
vivi graph activate --project $R $M1 --task $M1
vivi pool wait --project $R --pool local $M1    # prints `committed merge/ib-1 <sha>`

# 3. The orchestrator lands it: verify, fast-forward, publish, close.
git -C <repo> merge-base --is-ancestor u/ib-1 merge/ib-1
git -C <repo> merge --ff-only merge/ib-1
git -C <repo> push origin main
vivi task done --project $R --for mind $M1 --repo <repo> --tip <main sha>
vivi step --project $R --apply $M1              # dependents turn ready

# 4. Dependents dispatch to `lanes` again, now `--from main`.
```

Three things this arrangement must respect:

- **The remote is the bus.** `lanes` resolves `--from` through GitHub; `local`
  resolves it with local Git. Fetch a pushed branch before a local job needs it,
  and push main before a remote job is dispatched `--from main`.
- **A pool job does not land on main.** A push to the default branch is refused,
  and a worktree job is refused the default branch and the primary checkout's
  branch, unless `--allow-default-branch` is passed. So the merge job produces a
  branch and the orchestrator fast-forwards main, then accepts the merge task
  after checking ancestry.
- **Review seats can run read-only.** `vivi pool dispatch --pool lanes <task>
  --model <slug> --read-only --from <sha-or-branch>` mints a `contents: read`
  token and publishes nothing, so a reviewer may have a full shell. The result
  is `read_only_unchanged` or `read_only_discarded <count>` with the paths. The
  guarantee is strongest on Docker and Fly pools; see the parent skill for the
  weaker providers. A worktree pool's read-only mode has no token to withhold:
  the worker shares the repository and could commit with plain Git.

## 7. Known gaps

- **No merged state.** A Hand's `done` is a report; the author's or a mind's
  `done` accepts it and completes the node. Landing is the merge task by
  convention only; Vivi does not verify it, so the coordinator checks git
  before accepting a merge task.
- **Stacking is invisible.** Several units delivered under one handle are not
  separate nodes, so nothing orders or tracks them. Do not stack; give each unit
  its own handle, node, and lane.
- **Vivi records only "accepted" and, by convention, "merged".** Starting a
  dependent before its prerequisite is on main is a lane feature, not a graph
  one: pool stacking (section 2b) does it, and Vivi records the ordering only
  through the `--depends-on` edges above.
- **One handle, one lane.** A pool dispatch is bound to one task and an open
  dispatch blocks re-dispatch until collected; a host lane is claimed against
  one handle. A stacked chain cannot be handed to a second handle's lane, which
  is another reason not to stack.
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
  `missing_node`, `missing_edge`, `node_state` and `node_kind` findings at zero.
  A backlog with drift gives
  step and readiness answers that are quietly wrong. Record the counts in the
  project's own agent docs, not here.
- **Dual handles.** A send creates a sender copy and a recipient copy. Only the
  recipient copy has a node. Edges canonicalize to one copy per content, and a
  multi-recipient item is one work item, but always cite the `created` handle.
