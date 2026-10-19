# The pump's contract, part one

This is the contract of `vivi pump` as it behaves. It was ported from a Python
predecessor; this document describes the Rust engine itself. It is part one of
the contract; part two is [`pump-protocols.md`](pump-protocols.md) (merge, remote merger,
gate, board, plan files, collect rules). Where this document needs those
details it links there instead of duplicating them.

The design decisions are decided in this contract; the contract states them as
decided and does not reopen them. `vivi pump` is generic: nothing in the
engine assumes one site's machines, pools, toolchain or file names. Site values
live only in a project's config and in the clearly marked examples here.

## 1. Purpose and non-goals

The pump is a Mind written as a state machine. It reads planned units from
`plans/*.json`, dispatches each ready unit to a pool slot, merges what comes
back into the integration branch, runs the unit's proof on the merged tree,
retries failures on another model, and writes what needs a person to
`exceptions.txt`.

- It never reads a receipt to decide anything except the literal marker
  `VERDICT: no-defect`. Every other receipt text is used
  only as failure detail to feed back into the next attempt or to file an
  exception.
- It is a generic engine. Project specifics (which project, which branch, which
  pools, which hand rules, which checks) are config in the `[pump]` tables of
  `.vivi/mailspace.toml`, not code. The hand rules text and the gate command are config
  (`hand_rules`, gate `command`); the engine ships only a project-agnostic
  default for the rules.
- The `pump.db` schema (§5) is versioned. The `state.json` export shape is
  stable: a state file written by an earlier pump still loads through the
  import path, and unknown keys are preserved.

## 2. Surface

`vivi pump run | status | once | pause | resume | stop | start`, with the global `--project` flag naming the
project root; every `vivi` call the pump makes appends
`--project <project>`:

| mode | behaviour |
| --- | --- |
| `run` | loop: one `tick`, then sleep `tick` seconds, until stored mode is `stopped` |
| `once` | exactly one `tick`, then exit |
| `status` | read and print `status.txt` and control mode without taking the pump lock or migrating the database |
| `log` | print the tail of `log.txt` (or `exceptions.txt` with `--exceptions`): `-n N` (default 50, counted after filtering), `-f` follow, filters `--lane L` (lines whose text after the stamp starts `lane L:` or `lane L `; `2` is `default/2`), `--unit U` (whole-token match), `--grep TEXT`, ANDed; missing file: `no pump log at <path>`, exit 1; no lock, no database access |
| `pause` / `resume` | set stored control mode to `paused` / `running` |
| `stop` / `start` | set stored control mode to `stopped` / `running` |

Control commands take effect at tick boundaries. Paused mode keeps the process
alive and preserves the existing early-return behavior: board, ingest, collect
and already-fed merges continue, while dispatch and gate work are skipped.
Stopped mode completes the current tick and exits. A registered pump service
does not start stopped projects and starts them again after `start`.

Single instance: `run`/`once` open `<home>/lock` and take an exclusive
non-blocking flock before doing anything. A second instance exits non-zero
with the message `another vivi pump is running`.

Exit codes: 0 on a clean exit (stopped mode, service shutdown, or `once` finished); non-zero on
lock contention as above. A crashing tick is not an exit: tick exceptions are caught
and logged (§7).

`--project` is how the pump finds the mailspace; the pump runs its own binary
(`vivi`, through the command-line seam below) with `--project` appended.

The command-line seam. Every external command runs through `sh`:

```
bash -c "<shell_prefix>; <cmd>"
```

with `shell_prefix` the configured prefix (for example
`. /srv/pump/env.sh`; empty means none), an optional cwd, and a timeout (default
120 s). It returns the exit code and combined stdout+stderr. On timeout it
returns exit code 124 plus the text `[timeout after Ns]` appended to whatever
partial output was captured. This seam is also the test seam:
tests script a fake host that answers command lines. Detached processes
(planner, split, gatefix, sync push) run detached with redirected output
files instead.

`start` enables a pump. The service launches a registered project; for a
standalone project, run `vivi pump run` after enabling it.

## 3. The home directory

`[pump] home`, default `<project>/.vivi/pump` (an operator
may point it elsewhere, for example `/srv/pump`). One table, every file and
directory:

| path | written by | read by | format |
| --- | --- | --- | --- |
| `pump.db` | pump (one transaction per save point) | pump (load), readers (`need show`, `board`, `boot`) | SQLite, schema in §5; WAL, one writer |
| `state.json.imported` | one-time import, then left alone | operator (rollback) | the predecessor's JSON shape (§5) |
| `pump.db-wal`, `pump.db-shm` | SQLite | SQLite | WAL sidecars |
| `pump_control` in `pump.db` | pump control commands and service | pump and service | durable mode and transient service shutdown request |
| `log.txt` | pump `log` | operator | one line per event: `HH:MM:SS msg`, UTC; a merge lane thread's lines start `lane <target>/<n>: ` (see below) |
| `exceptions.txt` | pump `exception` | operator | `HH:MM:SS kind unit text`; newlines in text become ` \| `; text capped at 600 chars |
| `needs-ready.txt` | pump on need completion (merge_apply) | operator | `need tip10 unit,unit` per line |
| `status.txt` | pump `write_status` | `pump status` | timestamp, `units k=v …`, `plans k=v …`, `merges N needs-ready N gates N baseline …`, the three stall lines below, then one line per merge lane |
| `lock` | pump opens for flock | none (lock only) | empty |
| `skip.txt` | operator | board pass | need handles to never plan, whitespace separated |
| `plans/` | planner writes `<need>.json`, `<need>.r<N>.json` (a revision) and `<uid>.split.json` | pump ingest | plan JSON; see [`pump-protocols.md`](pump-protocols.md) |
| `plans/superseded/` | re-plan moves old plans here | operator | `<need>.<epoch>.json` |
| `needs/` | board dumps the need thread | planner (the plan prompt) | JSON `{need, subject, triage, thread}` |
| `board/` | pump writes `<need>.prompt`; planner shell writes `<need>.out`; the spawn reaper writes `<need>.out.rc` | operator | prompt text; planner stdout+stderr; the exit receipt the board reads to notice a dead planner |
| `resplit/` | pump writes `<unit>.json` and `<unit>.prompt`; planner writes `<unit>.out` | split planner | JSON + prompt text |
| `gatefix/` | pump writes `<need>.prompt`; planner writes `<need>.out` | gatefix planner | prompt text |
| `merge-jobs/` | pump writes `<bid>.job.json`; remote merger writes `.claimed.json`/`.result.json` | pump reads the result | batch protocol; see [`pump-protocols.md`](pump-protocols.md) |
| `repo/` | git operations (reset_repo and all merge work) | pump | legacy single-target integration clone; named targets use `targets/<id>/repo` |
| `planner-charter.md` | operator | planner command lines (`{charter}`) | the planner's system prompt |
| `sync-push.log` | detached `vivi sync push` | operator | shell redirect |
| operator scripts, notes and their logs | operator | operator | not engine state |

## 4. Flag files

Operator and pump flags live in `home`:

| flag | exact effect | removed by |
| --- | --- | --- |
| `HOLD-MERGE` | `merge_feed` feeds only units whose id starts with the fix prefix, or whose need is in the priority file; once every fix unit has settled and merged, `hold_cycle` writes `FORCE-GATE`; a clean gate newer than the hold lifts it (removes `HOLD-MERGE`) | pump (`hold_cycle`) or operator |
| `FORCE-GATE` | `start_gate` treats the gate as due now, whatever the counters say | pump, when the gate dispatches; also written by `hold_cycle` |
| `REMOTE-MERGE` | merge batches are offered to a remote merger first (file protocol under `merge-jobs/`); this host merges only if the offer is not claimed in time | operator |

On first database initialization, old `PAUSE` and `STOP` markers migrate to
`pump_control`; `STOP` wins if both exist. The database transaction commits
before the old marker files are removed. Later starts use the stored mode.

## 5. The state store

`pump.db` (SQLite) stores the pump's durable work state and operator control
mode. Work state is read at startup and written in one transaction at each
save point. The schema is `PRAGMA user_version = 4`.

| table | meaning |
| --- | --- |
| `units` | every unit ever ingested, one row per unit; `status` is the state-machine state |
| `unit_edges` | `after` prerequisites, one row per edge |
| `unit_events` | attempt history (`attempt`, `model`, `why`, `detail`, `at`) |
| `unit_order` | dispatch and merge order (fix units sorted first at dispatch) |
| `needs` | one row per planned need: `verdict`, `reason`, `units_json`, `revision`, `thread_sha256`, `thread_seen_at`, `amendment_pending` |
| `plan_revisions` | every accepted plan revision: `need`, `revision`, `verdict`, `reason`, `body_json`, `created_at` |
| `merges` | one row per merged unit: `commit_sha`, `at`, `outside_json` |
| `gates`, `gate_failures` | every finished gate, unreadable ones included, plus one row per failing name |
| `exceptions` | the same rows as `exceptions.txt`, queryable |
| `closed_needs` | need handles whose every unit is satisfied |
| `plans_seen` | plan file names already ingested (dedup) |
| `promotions` | fast-forwards of main (`tip`, `at`) |
| `red`, `red_reported` | failures owned by a gate-fix plan, and the ones already reported once |
| `meta` | `merges` counter, `baseline`, `last_gate`, `board`, and unknown top-level keys (`extra`) |

Each unit stores a `target` ID. Existing rows migrate to `default`. Named
target gate state (baseline, last gate, gate history, promotions, and owned
failures) is stored per target in `meta.target_states`; need and unit lifecycle
remain workspace-wide.

The `meta.board` key holds the board state: `at` (last pass), `planning`
(handle → planner start), `replied`, `replan`, `revision_planning`, and the
planner supervision keys — `planner_pids` (handle → pid of the running
planner), `planner_fails` (consecutive planner exits without a plan),
`planner_retry_at` (earliest epoch seconds a handle may relaunch), and
`planner_failed` (handles parked after the retries ran out). A handle never
relaunches while it is before its `planner_retry_at` deadline, and a parked
handle never relaunches at all.

Per-unit fields are the columns of `units` (all units carry the ingest-time
fields; runtime fields appear as marked):

Per-unit fields (all units carry the ingest-time fields; runtime fields appear
as marked):

| field | type | meaning |
| --- | --- | --- |
| `need` | string | the owning need handle |
| `target` | string | configured `[pump.targets.<id>]`; legacy state maps to `default` |
| `title` | string (≤150 chars) | unit title, from the plan |
| `brief` | string | the unit's work description |
| `write_scope` | array of path strings | files the unit may touch |
| `after` | array of unit ids | prerequisites; must be satisfied before dispatch |
| `proof` | array of command strings | commands that must exit 0 on the merged tree |
| `tools` | string | the unit's kind: a key of `[pump.dispatch.kinds]` (§9); empty means `default_kind`. Selects the pool, the in-flight cap, and whether the merger runs the unit's own proof |
| `status` | string | the state machine state (§6) |
| `attempts` | int | dispatches so far; incremented at dispatch |
| `history` | array of `{attempt, model, why, detail, at}` | one entry per failure or split request (fail, split); `detail` capped at 2500 chars |
| `task` | 8-hex handle or null | the pool task while `inflight`, else null |
| `branch` | `unit/<uid>-aN` or null | the worker branch |
| `model` | string or null | the model of the current/last attempt |
| `since` | epoch seconds | set at ingest and at each state change |
| `pool` | string | runtime: pool dispatched to |
| `since_dispatch` | epoch seconds | runtime: dispatch time, used for the overdue check |
| `commit` | string | runtime (merged): integration-branch tip the merge landed on |
| `merged_at` | epoch seconds | runtime (merged): when the merge landed; also the `merges.at` value |
| `outside` | array of files | runtime (merged): files the branch touched outside `write_scope` |
| `replaced_by` | array of unit ids | runtime (replaced): the split's successor ids |
| `verdict_text` | string | runtime (no-defect): tail of the no-defect receipt |
| `held_on` | handle or null | runtime: the dependency currently holding the unit back; the first tick of each hold is logged |
| `requeued` | bool, absent when false | runtime: the unit was requeued once by the blocking-stuck response, so a second stall parks it again instead of requeueing it again |
| `quarantine` | quarantine id or null | runtime: set on every unit of a quarantined batch and on the repair unit that works on it. `pump.db` keeps it (like `base` and `solo`) as a key of `units.extra_json` |
| `base` | ref or null | runtime: dispatch this unit `--from` this ref instead of `dispatch.from`; a repair unit carries `pump/quarantine/<id>` |
| `solo` | bool, absent when false | runtime: merge this unit in a batch of its own; set when its quarantine repair went stuck |

Compatibility rule: unknown top-level keys are preserved in `meta.extra`, and
unknown per-unit keys in `units.extra_json`. At load, a state older than
`plans_seen` gets it backfilled from `needs`. Before the first tick,
`run`/`once` ingest existing plan files, reset every `merging` unit to
`delivered`, and check pending units' next-attempt branches on the configured
remote. A pushed branch is recovered as `delivered` instead of being
dispatched a second time. If the remote cannot be checked, startup fails
closed; retry after connectivity returns. Schema upgrades hold the same pump
lock as the controller, so a second binary cannot migrate a live pump's state.
`status` reads control state without migrating the schema. A state-save failure
stops the controller with an error instead of continuing from volatile memory.

### 5.1 Migration and export

On startup, when `pump.db` has never been written and `state.json` exists, the
pump imports the file in one transaction, sets imported needs and units
without a revision to revision 1, and renames the file to
`state.json.imported`. The import is one-way. A `pump.db` whose schema is
newer than the binary is refused, not rewritten.

`vivi pump export --json <path>` writes the live database in the
`state.json` shape so a 12.x pump can read it. The export is a snapshot, not a
second live store; the pump never writes it.

### 5.2 Complete-but-open needs

The pump never closes a Vivi need on its tick. When every unit of a need is
satisfied it appends a `needs-ready.txt` line and posts the `merged` reply;
the registry record stays open until a person closes it.

`vivi pump closeout` joins the two records and lists every need whose units
are all satisfied while its Vivi copy is still open, with the unit count, the
newest integration tip and the completion time. `vivi pump closeout --close
<handle>` performs the lifecycle move as the item's holder (or `--for`), then
accepts it as the author. The `--json` form is the same rows for a script.
A need with `amendment_pending` is not listed: the addendum has to be planned
before the need can close.

### 5.3 Amendments and plan revisions

A need is a living Vivi record. The pump records `thread_sha256` and
`thread_seen_at` when a plan is accepted. Each board pass compares the newest
open-need event from `board --json` with `thread_seen_at`; only when it moved
does the pump fetch `need show --json` and hash the thread. A different hash
sets `amendment_pending` and starts a revision planner.

The planner writes `plans/<need>.r<N>.json` with `"revision": N`, the complete
unit set and an optional `withdraw` list. Applying a revision follows these
rules:

- a merged or no-defect unit is immutable; a changed field raises
  `revision-conflict` and the current unit stays;
- an inflight, delivered, merging or splitting unit is immutable; the addendum
  becomes a new unit;
- a pending unit is amended from the plan;
- a unit absent from the plan is kept; `withdraw` names the pending units to
  withdraw;
- a new id is minted `pending` at the new revision;
- the old revision stays in `plan_revisions` and the old file on disk.

Each accepted revision posts a `revised-N` reply once. The `planned`, `revised-N`
and `merged` replies sync, so a reader on another host sees the plan revision
and completion state in the thread even without `pump.db`.

## 6. The unit state machine

States:

| state | meaning | terminal | satisfied |
| --- | --- | --- | --- |
| `pending` | waiting to dispatch | no | no |
| `held` | configured repository target is missing; returns to `pending` when it is restored | no | no |
| `inflight` | dispatched to a pool slot; a task handle exists | no | no |
| `delivered` | the worker's branch exists on the remote; waiting to merge | no | no |
| `merging` | queued to or being handled by the merge thread | no | no |
| `merged` | merged into the integration branch, proof passed, pushed | yes | yes |
| `quarantined` | merged into a batch whose shared check failed; the batch sits on `pump/quarantine/<id>` and a repair unit works on it. Not fed, not dispatched; restart leaves it alone | no | no |
| `no-defect` | the receipt carried `VERDICT: no-defect`; nothing merged | yes | yes |
| `stuck` | attempts exhausted (or the split planner never answered); a person is needed | yes | no |
| `splitting` | a split planner is writing a replacement plan | no | no |
| `replaced` | superseded by split successor units | yes | only if every successor is satisfied (satisfied, recursion depth capped at 6) |
| `withdrawn` | taken out of the queue by hand; the pump leaves it alone | no | no |

`satisfied` is the readiness predicate used everywhere:
`merged` and `no-defect` are directly satisfied; `replaced` delegates to its
successors; everything else, `stuck` included, is not. A stuck unit therefore
blocks its dependents and keeps its need open forever; only a person unblocks
it. Record as-is.

Transitions (from, to, trigger, guard, action):

| from | to | trigger | guard | action |
| --- | --- | --- | --- | --- |
| — | `pending` | a new plan file in `plans/` | each unit has `id`, `brief`, `write_scope`, a list `proof`; no duplicate id, except a revision may name an existing id | unit record minted with `since=now` and `revision`, appended to `order`; need recorded |
| `pending` | `pending` (amended) | a revision plan with a higher `revision` | the unit is still pending | brief, scope, proof, tools and `after` replaced; `revision` updated |
| any | `withdrawn` | a revision's `withdraw` list | the unit is still pending | removed from the need's unit list; kept for history |
| `pending` | `inflight` | dispatch loop reaches it | `ready` (§8); a free pool slot; `pick_model` returns a model | task sent, pool dispatched, unit updated (`attempts+1`, `task`, `branch`, `model`, `since`, `since_dispatch`, `pool`) |
| `inflight` | `delivered` | collect: the task is not running and not reported, or reported `finished`, and the worker branch exists on the remote | receipt does not contain `agent exit 124` / `agent_rc 124` | `since=now`, log `delivered` |
| `inflight` | `no-defect` | collect: no remote branch, and the receipt contains `VERDICT: no-defect` | — | `verdict_text` kept, `no-defect` exception filed |
| `inflight` | `pending` or `stuck` | collect: no branch, no verdict marker | — | `fail`: history entry `{attempt, model, why, detail≤2500, at}`; `task=null`; `stuck` when `attempts >= max_attempts` (plus a `stuck` exception), else back to `pending` |
| `inflight` | `splitting` | timeout: reconcile state `timeout`, or still running past `budget + overdue_grace`, or the receipt shows agent exit 124 | — | `split`: `resplit/` files written, split planner spawned detached, `task=null`, history entry `why="timeout"`, `since=now` |
| `splitting` | `stuck` | split planner wrote no plan in `split_timeout` | — | `split-no-answer` exception |
| `splitting`/`pending` | `replaced` | a plan with `replaces: <uid>` is ingested | the plan passes the same unit checks | old unit `status="replaced"`, `replaced_by=<ids>`; empty `units` files a `split-empty` exception; successors minted as `pending` |
| `delivered` | `merging` | merge feed after collect | not held by an unlanded need dependency; not `HOLD-MERGE`, or the id starts with the fix prefix, or the unit's need is in the priority file | job put on the merge queue (state change only; the merge thread touches the repo, never the state) |
| `merging` | `merged` | merge result `ok=true` | — | `commit`, `merged_at`, `outside`, `merges+=1`; a `merges` row; `scope-warn` logged when `outside` is non-empty; need closed and a `needs-ready.txt` line appended when every unit of the need is satisfied |
| `merging` | `delivered` | merge result `ok=null` (`push-failed`, `merge-error`, `merge-undecided`) | — | back to `delivered`; the next feed retries |
| `merging` | `pending` or `stuck` | merge result `ok=false` (`fetch-failed`, `merge-conflict`, `proof-failed`) | — | `fail` as above, with the merge's `why`/`detail` |
| `merging` | `quarantined` | merge result `ok=null, why="quarantined"` carrying a `quarantine` record | a shared check failed on a batch of several units | `quarantine=<id>`, `since=now`; the first result of the batch also opens the repair unit (below) |
| `quarantined` | `merged` | the repair unit `<fix_prefix>-<id>` merges | — | every unit still `quarantined` under that id merges with the repair's tip: `commit`, `merged_at`, `merges+=1`, need closing, exactly as a merge result `ok=true` |
| `quarantined` | `delivered` (solo) | the repair unit goes `stuck` (including `split-no-answer`) | — | `solo=true`, `quarantine=null`; `quarantine-stuck` exception; the next feed puts each solo unit in a batch of its own, and a solo unit that fails a shared check fails normally (`proof-failed`, back to a hand) |
| `quarantined` | `delivered` | the repair unit ends `no-defect` (the failed check passes on its checkout) | — | `quarantine=null`; an ordinary batch retries them |

**Quarantine.** When a shared check fails on a batch of several merged units
there is no culprit and bisecting for one would cost a full check run per unit,
so the pump does not look for blame: the lane pushes its merged `HEAD` (the
batch and its style commit) to a new branch `pump/quarantine/<id>` on the
target's remote (a plain push, never forced; `<id>` is `q<epoch>-<6 hex>`),
resets the lane, and reports every cleanly merged unit as quarantined.
Units that conflicted in that batch keep their normal `merge-conflict`
failure. If the push does not land, the units come back undecided
(`quarantine-push-failed`) and return to `delivered`. A batch of one unit
keeps the old behaviour: that unit fails with `proof-failed` and a hand retries
it with the failure lines. Unit proof failures still isolate their unit; only
shared checks quarantine. The pump logs `quarantine <id>: <n> units, <cmd>
rc=<code>` and the last 40 lines of the distilled failure output (for
single-unit shared failures too).

Applying the first quarantined result opens exactly one **repair unit**:
uid `<fix_prefix>-<id>` (so it ranks with the fix units), its own need of the
same handle (verdict `units`, one unit), the quarantined units' write scopes
joined as `write_scope`, the failed shared command as its only `proof`, and a
brief naming the command, exit code, failure text and the quarantined units
with the instruction to make the check pass with the smallest change while
keeping every unit's feature (never reverting one; a unit that cannot pass
without losing its feature is named in the reply, and the repair fails to the
stuck path). A repair unit is never split: a timeout counts as a failed
attempt. Its `base` is `pump/quarantine/<id>`, so dispatch uses
`--from pump/quarantine/<id>` and the hand starts on the merged batch. A
`quarantine <id>` exception announces it.


The merge batch itself (which units go together, the remote merger, promotion)
is [`pump-protocols.md`](pump-protocols.md); the state machine above only records what
each merge outcome does to the unit.

## 7. The tick

`tick` runs these steps in order. Dispatch runs before the periodic board pass so
a slow mailspace read does not keep ready worker slots idle:

1. `merge_apply`: drain finished merge results into state.
2. `ingest`: read plan files already present in the pump home.
3. `collect`: reconcile pool state against inflight units and the gate task.
4. `merge_apply` again.
5. `merge_feed`: queue delivered units for merging.
6. `save` — save point one.
7. Control check: if the mode is paused, run the board pass, ingest any new
   plan files, save, write status and return. Dispatch and gate work are skipped;
   already-fed merges continue on the independent merge thread.
8. `dispatch`.
9. `save` — save point two.
10. `hold_cycle`.
11. `external_gates`: take finished results from `gate.results_dir`, if set.
12. `start_gate`.
13. `merge_apply` — lets gate/merge results land in state.
14. `merge_feed`.
15. `board`: sync pull, re-plan answered parked needs, plan new needs and reply
   on threads. A failure logs `BOARD ERROR` and the tick continues.
16. `ingest`: read plan files written by the board pass.
17. `save` — save point three.
18. `write_status`.

Error handling:

- Board errors are caught inside `tick` (step 2, above).
- Any other tick exception propagates to the `run`/`once` loop, which logs
  `TICK ERROR` with the panic note and saves the state before continuing. The loop does not exit on tick errors.
- Merge thread errors are caught inside `merge_thread`: every job in the batch
  gets an `ok=null` `merge-error` result, and any job the results did not name
  gets `merge-undecided`. `merge_apply` turns both into
  `delivered`, so nothing is lost or double-merged.
- State is saved at the three tick save points, after a tick error, and before exiting on stopped control state. The merge thread
  never saves; it hands results to `merge_apply` through the queue.

## 8. Scheduling

Readiness (`ready`):

- The unit must be `pending`.
- Its need must have no unlanded dependency. One `graph blocked backlog
  --kind need --json` read per pass lists every blocked need with the
  prerequisites still unfinished; the unit waits while any of them is
  unfinished. A prerequisite whose need the pump has already delivered —
  every unit satisfied — counts as landed even while its Vivi node is still
  open. The first tick of each hold logs
  `hold <uid>: need <need> waits on dependency <dep> not landed`, and the
  release is silent. The same rule keeps a `delivered` unit out of the merge
  feed, so a dependent cannot merge before its dependency.
- Every id in `after` must be `satisfied`.
- Write-scope serialization: an overlapping `inflight` unit blocks dispatch
  while an agent is editing that scope. `delivered` and `merging` units no
  longer occupy workers, and the merge thread serializes their integration, so
  they do not block new dispatches. Fix units (ids starting with `fix_prefix`)
  and priority units (their `need` has a priority level in `pump.db`) ignore
  overlap against non-fix units.

- The unit's write scope must not overlap any path in the reserved file
  (`reserved_file`, read afresh on every check; blank lines and lines starting
  with `#` are skipped).

Overlap (`overlaps`, in `src/pump/plan.rs`) is read segment by segment. A
scope is its path split on `/`, with leading `.` and `/` characters and a
trailing slash stripped. A scope covers what it names and everything under it,
so `src/a` overlaps `src/a/b.rs`. `*`, `?` and `[...]` match within one
segment, `**` matches any number of segments, and a glob with no `/` matches at
any depth (as in `.gitignore`). Two globs at the same position are assumed to
meet. An empty scope covers the whole tree and overlaps everything. Two scopes
overlap when some file could match both.

Model choice (`pick_model`):

- Count inflight units per model.
- If `attempts >= max_attempts - 1`, return `last_resort`.
- Walk the `lanes` in order, skipping the model of the last attempt if another
  is free under its cap; then walk again without the
  not-the-last-model preference. Lane order alternates with
  attempt parity: even attempts walk the configured order, odd attempts walk it
  reversed.
- No model under any cap returns `None`, which breaks the dispatch loop for
  this tick.

Pool choice: a unit's `tools` value names its kind (empty means
`[pump.dispatch] default_kind`), and the kind's `pool` is where the unit is
dispatched. When the kind has a `max`, a unit of that kind is skipped — not
failed, not counted as started — while at least `max` units of the kind are
already inflight; same-tick starts count toward the cap, and the skipped unit
is simply retried next tick. A unit naming a kind that is not defined is never
dispatched; the pump files one `unknown-kind` exception for it (once per unit,
remembered in `pump.db`) so an operator sees it. Free slot counts come from
`pool status --porcelain`. At most `per_tick` units start per tick, and fix
units are offered first.

Brief composition (`brief_for`), in order:

1. `hand_rules` — the configured rules file, or the embedded project-agnostic
   default text.
2. `UNIT <id>: <title>`.
3. The unit's `brief`.
4. `WRITE SCOPE:` and the one-line-per-path list.
5. `PROOF (each must exit 0):` and the command list, or `- (no unit proof; the shared checks only)` when empty.
6. When history exists: the `A PREVIOUS ATTEMPT … FAILED` block with the last
   entry's `why` and the last 1800 chars of its `detail`.
7. The `done_when:` line: the PROOF commands pass, or the reply ends with
   `VERDICT: no-defect` and evidence.

Task send and dispatch command lines:

```
vivi task send --from operator --to hand --subject "[<uid> a<N>] <title>" \
    --body-file /tmp/pump-brief-<uid>.txt        # subject truncated to 170 chars
vivi pool dispatch --pool <pool> <task> --model <model> --from <branch> \
    --branch unit/<uid>-a<N> --budget 600 --porcelain [--target <id>]
```

with `--from`/`--to`/budget from config (§9). The created handle is read with
the regex `^(?:node|created)\s+(?:\S+\s+)?([0-9a-f]{8})\s*$` in multiline mode; no match logs `task send failed` and skips the unit. A failed
pool dispatch logs `dispatch failed`, marks the task done with
`dispatch failed; superseded`, and zeroes the pool's free count when the output
says `no free slot`.

## 9. Config reference

All keys live in the `[pump]` tables of `.vivi/mailspace.toml` and are optional
unless marked.

`[pump]`:

| key | type | default | controls |
| --- | --- | --- | --- |
| `home` | path | `<project>/.vivi/pump` | the home directory (§3) |
| `repo` | path | `<home>/repo` | the legacy single-target integration clone; named targets use `<home>/targets/<id>/repo` |
| `remote` | string | `origin` | the git remote (name) the integration branch and main live on; the name given to the clone's remote |
| `remote_url` | string | none | URL or path the missing integration clone is created from (`git clone -o <remote> --branch <branch>`) |
| `committer_name` | string | none — **required** | `user.name` of every commit and merge the pump makes, passed as `git -c user.name=…` |
| `committer_email` | string | none — **required** | `user.email`, passed as `git -c user.email=…` |
| `branch` | string | none — **required** | the integration branch |
| `main` | string | `main` | main branch merged into the integration branch each batch; `""` disables both merging main in and promotion |
| `promote` | bool | `true` | a gate with no new failure and no owned failure still red fast-forwards main to the gated tip |
| `tick` | int seconds | `20` | sleep between ticks |
| `shell_prefix` | string | `""` | prepended to every command line (§2) |
| `hand_rules` | path | embedded generic text | file whose text opens every unit brief; a relative path joins the project root like `home`; `validate` rejects a file that cannot be read |
| `reserved_file` | path | `reserved.txt` | paths held by another session; units overlapping them are not dispatched; a relative value joins `home` |
| `priority_file` | retired | | accepted and ignored so an old `mailspace.toml` still parses. Need priority is set per need in `pump.db`: `vivi pump priority set <need> <level>` (integer, higher is more urgent, 0 removes), `clear <need>`, `list`. The running pump reads the levels every tick. Dispatch and merge-feed order is fix units first, then units by their need's level (highest first), then the rest (stable); a need with a level above zero is also fed under `HOLD-MERGE` and gets the fix-unit scope bypass. A leftover `priority.txt` is imported once at startup as level 1 and renamed to `priority.txt.imported` |
| `fix_prefix` | string | `pumpfix` | unit ids with this prefix are fix units: dispatched first, merged even under `HOLD-MERGE`, scope-checked only against in-flight units |
| `from` | role | `operator` | role the pump sends unit tasks from |
| `to` | role | `hand` | recipient role of a unit dispatch |
| `reply_from` | role | `mind` | role that replies on need threads |

`[pump.targets.<id>]` defines workspace repositories while pool capacity and
pool definitions stay shared at the mailspace level. `path` is relative to the
mailspace root. Each target owns `remote`, `remote_url`, `branch`, `main`,
`promote`, optional `github_repo`, and optional `integration_repo`; per-target
`[pump.targets.<id>.merge]` and `.gate` override shared `[pump.merge]` and
`[pump.gate]` defaults. A missing `remote_url` is read from the target path's
configured Git remote. The planner receives target IDs and paths. Every unit
in a named-target plan must select one target, and its `write_scope` is relative
to that repository. One need may contain units for several targets; an unknown
or omitted target is rejected before dispatch. Pool jobs receive `--target
<id>` and isolate their checkout under the shared slot. Legacy single-repo
configuration remains the implicit `default` target.

`[pump.board]`:

| key | type | default | controls |
| --- | --- | --- | --- |
| `from` | list of senders | none | senders whose open needs addressed to `reply_from` are plannable |
| `since` | ISO datetime | none | needs dated before this are skipped, unless marked or re-planned |
| `mark` | string | `pump` | approval mark: a marked open need is plannable whatever sender or date |
| `every` | int seconds | `300` | minimum delay after one board pass completes before the next |
| `planners_max` | int | `3` | concurrent planners |
| `plan_timeout` | int seconds | `1800` | seconds a planner has to write `plans/<need>.json` before a `plan-no-answer` exception |
| `reply_to` | address | first of `from` | `--to` of thread replies |
| `pull` | bool | `true` | run `vivi sync pull` at the start of a pass |
| `push_since` | duration | `30m` | after replying: detached `vivi sync push --since <push_since> --drain` |

`[pump.planner]`:

| key | type | default | controls |
| --- | --- | --- | --- |
| `run` | command template | none — **required** while `board.from` or `board.mark` is set | the planner command; prompt file on stdin, stdout+stderr to the `.out` file, cwd `home`; `{charter}` is replaced by `charter` |
| `charter` | path | none | the file substituted for `{charter}` in `run` |
| `split_timeout` | int seconds | `1500` | seconds a split planner has to answer before the unit is `stuck` |

`[pump.dispatch]`:

| key | type | default | controls |
| --- | --- | --- | --- |
| `lanes` | list of `{model, cap}` | none — **required** | model lanes and per-model inflight caps (§8) |
| `last_resort` | string | none | model used when `attempts >= max_attempts - 1`; empty means the final attempt uses the normal lanes |
| `max_attempts` | int | `3` | attempts before `stuck` |
| `budget` | int seconds | `600` | `--budget` of a unit dispatch |
| `overdue_grace` | int seconds | `900` | running longer than `budget + overdue_grace` means split |
| `per_tick` | int | `6` | units started per tick at most |
| `from` | string | the `branch` value | `--from` of a unit dispatch |
| `default_kind` | string | none — **required** when `kinds` is non-empty | the kind of a unit with no `tools` value; must be a defined kind |
| `kinds` | table of tables | none | the unit kinds, below |

`[pump.dispatch.kinds.<name>]` defines one unit kind. A unit's `tools` field
(in its plan) names its kind; a unit with no `tools` takes `default_kind`.

| key | type | default | controls |
| --- | --- | --- | --- |
| `pools` | list of pool names | none — **required**, non-empty | the `[pools.<name>]` tables units of this kind are dispatched to; each unit goes to the listed pool with the most free slots (ties: first listed), and waits when none has a free slot. `max` counts the kind's inflight units across all its pools |
| `max` | int | none (no cap) | units of this kind allowed in flight at once; at the cap a further unit waits (skipped, not failed) |
| `skip_proof` | bool | `false` | units of this kind skip their own `proof` commands at merge (for example because the worker already proved them on a toolchain the merge host lacks); the shared checks still run |

A unit naming an undefined kind is not dispatched, and the pump files one
`unknown-kind` exception for it. There are no other per-kind keys and no
built-in kind names: any name a project chooses is as good as any other.

Example (one site's setup; kind names and pools are the project's own). Two
kinds: `plain` units run on a pool of small workers, `full` units on a pool of
full-toolchain machines, at most two at a time so that one stays free for
gates, and their own proof is skipped at merge:

```toml
[pump.dispatch]
default_kind = "plain"

[pump.dispatch.kinds.plain]
pools = ["fly4"]

[pump.dispatch.kinds.full]
pools = ["flyfull"]
max = 2
skip_proof = true
```

A plan unit with `"tools": "full"` goes to the `flyfull` pool; one with no
`tools` goes to `fly4`.

`[pump.alarm]` (stall alarms):

| key | type | default | controls |
| --- | --- | --- | --- |
| `fix_wait_minutes` | int minutes | `30` | minutes a fix unit may sit `delivered` or `merging` before a `stall-fix-unit` exception |
| `promote_hours` | int hours | `3` | hours without a promotion, while at least one unit has merged since the last one, before a `stall-promote` exception |
| `merge_idle_minutes` | int minutes | `45` | minutes units may sit `delivered` with nothing merged before a `stall-merge` exception |
| `blocking_hold_units` | int units | `10` | transitive held units at which a stuck or parked root raises `blocking-stuck` |
| `blocking_replan_minutes` | int minutes | `120` | minutes a blocking stuck root waits before the pump re-plans it |

Each tick the pump checks the stall signals and files `stall-fix-unit <uid>`
(a `<fix_prefix>` unit waiting past `fix_wait_minutes`), `stall-promote
<target>` (text: time since the last promotion, merges awaiting promotion, the
last gate's failing count; measured from the first unpromoted merge when the
target never promoted; only for targets with promotion enabled),
`stall-merge pump` (delivered units, oldest wait, last merge) and
`blocking-stuck <uid|need>` (a stuck unit, or a need parked on a verdict, whose
dependency closure holds at least `blocking_hold_units` units; text: the state,
the age, the transitive held count, the number of dependent needs, and the top
held needs as `handle (n)`). A subject (`<kind> <unit>`) is filed at most once
per hour, so a persisting block repeats on that cadence; the raised times are
kept in `state.json` under `alarm_raised`. The automatic response runs right
after the alarms: for each `stuck` root holding at least
`blocking_hold_units` units the pump raises the blocking need to priority 1
(unless it already has a level), requeues each of its stuck units whose last
attempt failed transiently (`timeout` or an agent exit 124 — a plain exit 1
flake stays on the re-plan path) exactly once, back to `pending` with attempts
unchanged and `requeued` set, and hands a stuck unit that is still stuck
`blocking_replan_minutes` in to a fresh split planner (`resplit/<uid>.json`,
status `splitting`); a repair unit is never split. Parked-verdict roots are
only alerted here, never re-planned, and a starved `pending` root surfaces in
`status.txt` only. `status.txt` carries three stall
lines, `last promote <age> ago (<tip10>)` or `last promote never`, `oldest
waiting fix <uid> <age>` or `oldest waiting fix none`, `delivered waiting <n>,
oldest <age>`, and then up to three `blocker <need> <state> age <age> held <n>`
lines, largest hold first; ages read `2h05m` or `14m`.

Merge lane visibility. Every `log.txt` line written from a lane thread starts
with `lane <target>/<n>: `, so parallel lanes can be told apart; lines from the
main thread stay unprefixed. A lane logs each step as it starts or ends:
`clone start: <dir>` / `clone done <s>s`, `merge <uid> <k>/<n> ok|conflict|
already integrated|integration check failed|fetch failed: <text>`, `format:
<cmd>` / `format done <s>s` / `style commit`, `check <i>/<n> start: <cmd>`
(then the existing `batch N shared rc=…` end line), `proof <uid> start: <cmd>`
(then `proof <uid> rc=… <s>s: <cmd>`), `publish: fetch`, `publish: merging
moved <branch> tip <sha10>`, `publish: push attempt <k>`, `publish: pushed
<sha10>` or `publish: failed <why>`, and the quarantine lines. After the stall
lines, `status.txt` carries one line per lane, in lane order:
`lane <target>/<n> busy <units> units <batch age>: <current step> (<step age>)`,
`lane <target>/<n> idle`, `lane <target>/<n> backoff <time> left` or
`lane <target>/<n> gone`; durations read `45s`, `2m10s`, `1h05m00s`.

`[pump.merge]`:

| key | type | default | controls |
| --- | --- | --- | --- |
| `batch` | int | `8` | jobs merged together at most |
| `format` | command | none | run after merging; a change is committed `style: format after <ids>`; empty skips the step |
| `shared_checks` | list of commands | none; must be non-empty while promotion is enabled | run after every merge, before the units' own proof |
| `proof_timeout` | int seconds | `300` | seconds per check command |
| `remote_wait` | int seconds | `90` | seconds a remote merger has to claim a batch before this host merges it |
| `remote_limit` | int seconds | `1800` | seconds a claimed batch may take |

`[pump.gate]`:

| key | type | default | controls |
| --- | --- | --- | --- |
| `command` | command | none — **required** | run by a read-only gate worker; it must list failing tests between FAILING-BEGIN / FAILING-END |
| `every_merges` | int | `8` | merges since the last gate that make one due |
| `every_seconds` | int seconds | `1800` | also due when merges > the last gate's and this long has passed |
| `pools` | list of pool names | none | pools a gate worker may use; the first with a free slot wins |
| `model` | string | none | the gate worker's model |
| `budget` | int seconds | `1800` | the gate dispatch budget |
| `results_dir` | path, optional | unset | directory an external runner drops `<tip>.json` gate results in; relative paths join the project root like `home`. Unset: pool gates only |
| `results_wait_seconds` | int seconds | `0`; must be >= 1 when `results_dir` is set | how long a due gate waits for an external result before a pool gate is dispatched instead |
| `max_replans` | int | `2` | how many times a name that still fails after every unit of its owner settled is handed to a fresh gate-fix planner (counted per name in `state.extra["replans"]`); `0` never re-plans |
| `ignore_file` | path, optional | unset | test names dropped from every gate's failing set: one per line, blank lines and `#` comment lines skipped; relative paths join the project root |

External gate results: when `results_dir` is set, every tick takes each
`*.json` there, oldest first, as a finished gate. The file is
`{"tip", "rc", "names", "build_failed", "tail", "seconds", "host"}`: `names` is
the failing test names or `null`, `build_failed: true` finishes the gate as a
build failure carrying `tail`, `seconds` and `host` are only logged (`host` is
free text and optional), and the other keys are not read. The gate then goes
through the same classification as a worker's reply, and the consumed file moves to
`<results_dir>/done/`. A file that does not parse stays where it is. A due gate
that finds no result starts a wait (`external_wait_since` in `pump.db`);
only after `results_wait_seconds` does it dispatch a pool gate, and a consumed
result clears the wait.

One constant has no config key: the 2400-second gate collection grace — a gate
task absent from reconcile is finished from the receipt after that long. It is
not configurable.

Validation rules (`vivi pump` refuses to start when one fails):

- `branch` is required.
- `committer_name` and `committer_email` are required.
- `gate.command` must not be empty (always).
- `merge.shared_checks` must not be empty while promotion is enabled
  (`promote = true` and `main` non-empty): with no checks a merge would land
  unchecked.
- `hand_rules`, when set, must be a readable file (relative to the project
  root).
- `tick`, `max_attempts`, `budget`, `per_tick`, `merge.batch` and
  `gate.every_merges` >= 1.
- `lanes` is non-empty and every lane's `cap` >= 1 and `model` non-empty.
- `[pump.planner] run` must be set while `board.from` or `board.mark` is set.
- Every kind needs a `pool` naming an existing `[pools.*]` table, and `max`,
  when set, is >= 1. When `kinds` is non-empty, `default_kind` must be set and
  must be a defined kind.
- Every gate `pools` entry must exist in `[pools.*]`.

## 10. Rulings the pump keeps

1. **A red gate never stops merging.** New failures are owned by a gate-fix
   plan (`red[name] = <fix need>`) and later gates subtract owned names.
   Merging, dispatch, and feeding continue while red.
2. **Main moves continuously.** Every merge batch first merges main into the
   integration branch, and a gate with no new failure and no owned failure
   still red fast-forwards main — never forces (a gate that still shows an
   owned failure logs `promote held` and main waits for a clean one: main never
   gets redder); a main with commits the tip lacks waits for the next batch
   (`merge-base --is-ancestor` check). NOTE: a single-job batch skips the batch
   path and goes straight to `merge_work`, which does not merge main first;
   record as-is.
3. **Hands run only the unit's proof plus what the hand rules ask for.** The
   merger runs the shared checks plus the unit's proof on the merged result
   (the proof is skipped for a `skip_proof` kind). A timeout means split the
   unit, never retry it.
4. **A need is the pump's work only when it is addressed to `reply_from` or
   held for it.** The sender path (`board.from`) lists with `--to
   <reply_from>`, and the marked path already narrows to held or to-mind. A
   need addressed to another recipient is not a candidate.
5. **An open need carrying the approval mark is plannable** whatever its sender
   or date: `marked_rows` lists held and to-mind open needs carrying `mark`,
   and the board adds them past the `since` cutoff.
6. **A thread reply is never a candidate.** A work send with `--reply-to` is a
   captured reply on an existing thread; work lists show a thread by its root,
   so the board never sees the reply as new work however it was sent.
7. **A parked need is re-planned when the operator replies** on its thread
   after the plan file was written: verdict in `operator|design|hold`, no
   units, plan superseded, need deleted, handle queued for re-planning.
8. **No environment variables for configuration:** config lives in `[pump]` and
   CLI flags only; the pump reads no environment variable for configuration.
7. **No speculative hooks.** The pump has no decision script. The one place
   config cannot express a decision today is gate-reply name extraction when
   the reply has no marker block; see §11.

## 11. Not built, and the first decision-hook event

Not built, as decided in this contract:

- The decision hook.
- `vivi trace` does not show pump state. `need show`, `boot`, `need list
  --pump` and `board --pump` read `pump.db` read-only on the pump host; the
  file stays private to the pump's directory and is never written by those
  commands.
- Units and plans are plan files, not Vivi graph nodes.
- Project-specific proof rewriting: the planner charter should demand one
  filter per proof command.

The first decision-hook event (the one config cannot express): `gate_result` —
classifying a gate worker's reply into failing test names when it carries no
FAILING-BEGIN/FAILING-END block. The built-in fallback matches
`\b([a-z][a-z0-9_-]+) ((?:[A-Za-z0-9_]+::)+[A-Za-z0-9_]+)\b`, which is specific
to one test runner's output. Set arithmetic (new = names − baseline − owned,
after the ignore file is dropped) is generic and stays in the engine; only name
extraction from an unmarked log is project-specific. Proposed closed verdict
set: `{"failing": ["<crate> <test>", …]}` or `{"build_failed": true}`; a
nonzero exit, invalid JSON, or an unknown verdict fails closed (exception, no
action). Not built.

Behaviours the decisions above do not list:

- Residual re-plan: after the owned-red release, each owner whose units (ids
  `<owner>-*`) all settled and whose names still fail at this gate has those
  names released, counted in `state.extra["replans"]`, and re-owned by a new
  gate-fix need planned at this tip together with any new failures. The need
  is `<fix_prefix>-g<tip8>`, or `<fix_prefix>-g<tip8>-r<k>` (first free `k`)
  when a plan or planner prompt for the base id exists. The prompt gives
  residual names their own section naming the old owner. One log line per
  owner: `replan: N residual failures of <owner> -> <need>`. A name past
  `max_replans` is not re-planned.
- The once-only `red-after-fix` report: an owned failing name still failing
  after its fix units all settled, and not re-planned (re-plans exhausted or
  `max_replans = 0`), raises one exception per `owner:name`, then never again.
- The reconcile-miss re-check: a task absent from reconcile output is first
  given a 90-second grace, then re-checked against `pool status --porcelain`
  before the pump decides from the remote branch.
- The receipt scan for `agent exit 124` / `agent_rc 124` on the finished and
  failed paths, which routes to `split` rather than `fail`.
- Single-job batches bypass `batch_round` and its merge-main-first step; see the
  NOTE on ruling 2.
- Quarantine covers a repair unit that merges, goes stuck, or ends no-defect.
  A repair unit is never split. One withdrawn by hand leaves its units
  `quarantined`; a person settles them.
- A failed pool dispatch closes the just-sent task with
  `task done … --note 'dispatch failed; superseded'` and, on `no free slot`,
  zeroes the pool's free count for the rest of the tick.
- The split history entry carries `why="timeout"` with an empty `detail`.
- A fetch of a unit branch that does not exist on the remote (git's "couldn't
  find remote ref") is a decided `branch-missing` failure, counted as a failed
  attempt; any other fetch error stays undecided (`fetch-failed`) and is
  retried.

## 12. Implementation map

### Build status

| module | status | proven by |
| --- | --- | --- |
| `config.rs` | built | `config_test.rs` |
| `state.rs` | built | `state_test.rs` |
| `plan.rs` | built | `plan_test.rs` |
| `ready.rs` | built | `ready_test.rs` |
| `brief.rs` | built | `brief_test.rs` |
| `host.rs` | built | `host_test.rs` |
| `collect.rs` | built | `collect_test.rs` |
| `dispatch.rs` | built | `dispatch_test.rs` |
| `merge.rs` | built | `merge_test.rs` |
| `remote.rs` | built | `remote_test.rs` |
| `gate.rs` | built | `gate_test.rs` |
| `board.rs` | built | `board_test.rs` |
| `run.rs` | built | `run_test.rs` |
| CLI `vivi pump run \| once \| status` | built | `src/cli/pump_command.rs`, `src/local_pump_command.rs`, `tests/pump_cli.rs` |

| Module (`src/pump/`) | Owns |
| --- | --- |
| `config.rs` | The `[pump]` table, its defaults and `validate`; unit kinds. |
| `state.rs` | load/save; the `MergeJob`/`MergeResult` records. |
| `plan.rs` | `overlaps`, `ingest`. |
| `ready.rs` | `satisfied`, `ready`, `pick_model`, the reserved-paths file. |
| `brief.rs` | The default hand rules, `brief_for`, the gate brief. |
| `host.rs` | `sh`, `vivi`, `git`, `log`, `exception`: the command-line seam and its test fake. |
| `collect.rs` | `receipt`, `fail`, `split`, `collect`. |
| `dispatch.rs` | `slots`, `dispatch`, kind resolution. |
| `merge.rs` | `merge_work`, `batch_round`, `batch_work`, the merge thread, `feed`, `apply`. |
| `remote.rs` | `remote_merge`. |
| `gate.rs` | `start_gate`, `finish_gate`, `hold_cycle`, `promote`. |
| `gatefix.rs` | `gatefix`, the planner prompt, the fix need id, the residual re-plan. |
| `board.rs` | `board`, `replan`, `replies`. |
| `run.rs` | `tick`, `write_status`, the run/once loop. |

Rust-specific decisions:

- The pump runs its own binary (`std::env::current_exe`) and `git`; it never
  links to pool internals.
- Every external effect goes through the `Host` seam, so tests script command
  lines instead of touching a real pool.
- No regex crate: the created-handle line and the gate name fallback use
  hand-written matchers.
- `pump.db` keeps its column names and preserves unknown keys; the export
  keeps the predecessor's field names.
- The merge thread owns the repo and never the state.
- No environment variable is read.
