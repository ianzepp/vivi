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
- The `state.json` shape (§5) is stable: a state file written by an earlier
  pump loads, and unknown keys are preserved.

## 2. Surface

`vivi pump run | status | once`, with the global `--project` flag naming the
project root; every `vivi` call the pump makes appends
`--project <project>`:

| mode | behaviour |
| --- | --- |
| `run` | loop: one `tick`, then sleep `tick` seconds, until the STOP file appears |
| `once` | exactly one `tick`, then exit |
| `status` | print `status.txt`, or `no status yet` when absent; no lock, no state load |

`once` also honours STOP: the check happens after the tick.

Single instance: `run`/`once` open `<home>/lock` and take an exclusive
non-blocking flock before doing anything. A second instance exits non-zero
with the message `another vivi pump is running`.

Exit codes: 0 on a clean exit (STOP seen, or `once` finished); non-zero on
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

To restart the pump, touch STOP, wait for the process to exit (a tick may
take minutes), remove STOP and start `vivi pump run` again. Stop by pid or
session, never by matching a command line.

## 3. The home directory

`[pump] home`, default `<project>/.vivi/pump` (an operator
may point it elsewhere, for example `/srv/pump`). One table, every file and
directory:

| path | written by | read by | format |
| --- | --- | --- | --- |
| `state.json` | pump, atomic (`state.json.tmp` then rename, save) | pump (load) | JSON, indent 1; shape in §5 |
| `state.json.tmp` | pump (transient) | none | scratch |
| `log.txt` | pump `log` | operator | one line per event: `HH:MM:SS msg`, UTC |
| `exceptions.txt` | pump `exception` | operator | `HH:MM:SS kind unit text`; newlines in text become ` \| `; text capped at 600 chars |
| `needs-ready.txt` | pump on need completion (merge_apply) | operator | `need tip10 unit,unit` per line |
| `status.txt` | pump `write_status` | `pump status` | 4 lines: timestamp, `units k=v …`, `plans k=v …`, `merges N needs-ready N gates N baseline …` |
| `lock` | pump opens for flock | none (lock only) | empty |
| `skip.txt` | operator | board pass | need handles to never plan, whitespace separated |
| `plans/` | planner writes `<need>.json` (and `<uid>.split.json`) | pump ingest | plan JSON; see [`pump-protocols.md`](pump-protocols.md) |
| `plans/superseded/` | re-plan moves old plans here | operator | `<need>.<epoch>.json` |
| `needs/` | board dumps the need thread | planner (the plan prompt) | JSON `{need, subject, triage, thread}` |
| `board/` | pump writes `<need>.prompt`; planner shell writes `<need>.out` | operator | prompt text; planner stdout+stderr |
| `resplit/` | pump writes `<unit>.json` and `<unit>.prompt`; planner writes `<unit>.out` | split planner | JSON + prompt text |
| `gatefix/` | pump writes `<need>.prompt`; planner writes `<need>.out` | gatefix planner | prompt text |
| `merge-jobs/` | pump writes `<bid>.job.json`; remote merger writes `.claimed.json`/`.result.json` | pump reads the result | batch protocol; see [`pump-protocols.md`](pump-protocols.md) |
| `repo/` | git operations (reset_repo and all merge work) | pump | the integration clone; `[pump] repo` overrides |
| `planner-charter.md` | operator | planner command lines (`{charter}`) | the planner's system prompt |
| `sync-push.log` | detached `vivi sync push` | operator | shell redirect |
| operator scripts, notes and their logs | operator | operator | not engine state |

## 4. Flag files

All flags live in `home`. The operator creates them; removal is per flag:

| flag | exact effect | removed by |
| --- | --- | --- |
| `PAUSE` | `tick` returns early after the first save point: no dispatch, no hold cycle, no external gate intake, no gate start, no post-dispatch merge apply/feed. The steps before the check (merge apply, board, ingest, collect, first feed) still run, so already-fed units keep merging | operator |
| `STOP` | after the current tick completes, save and exit the loop | operator |
| `HOLD-MERGE` | `merge_feed` feeds only units whose id starts with the fix prefix; once every fix unit has settled and merged, `hold_cycle` writes `FORCE-GATE`; a clean gate newer than the hold lifts it (removes `HOLD-MERGE`) | pump (`hold_cycle`) or operator |
| `FORCE-GATE` | `start_gate` treats the gate as due now, whatever the counters say | pump, when the gate dispatches; also written by `hold_cycle` |
| `REMOTE-MERGE` | merge batches are offered to a remote merger first (file protocol under `merge-jobs/`); this host merges only if the offer is not claimed in time | operator |

## 5. The state file

`state.json` is the pump's whole memory. It is saved atomically and reloaded
at startup. Top-level keys:

| key | type | meaning |
| --- | --- | --- |
| `units` | object: unit id → unit record | every unit ever ingested |
| `order` | array of unit ids | dispatch and merge order (fix units sorted first at dispatch) |
| `needs` | object: need handle → `{verdict, reason, units[]}` | one record per planned need; verdict is the planner's (`units`, `hold`, `operator`, `design`, `delivered`, `invalid`, …) |
| `merges` | int | count of units that reached `merged` |
| `last_gate` | object `{at, merges, task, tip, pool}` | the running or last gate; `task` is null when no gate is in flight |
| `baseline` | array of failing names, or null | failing names of the first readable gate; new failures are measured against it |
| `closed_needs` | array of need handles | needs whose every unit is satisfied |
| `gates` | array of gate records `{tip, at, merges, names, [new]}` | every finished gate, unreadable ones included |
| `plans_seen` | array of plan file names | plan files already ingested (dedup) |
| `board` | object `{at, planning{h→start}, replied[keys], replan[h]}` | board pass bookkeeping: last pass time, planners in flight, replies already sent, needs awaiting re-plan |
| `red` | object: failing name → owning fix-need prefix | failures owned by a gate-fix plan; later gates ignore owned names |
| `red_reported` | array `owner:name` | owned names already reported `red-after-fix` once |
| `promotions` | array `{tip, at}` | fast-forwards of main |

Per-unit fields (all units carry the ingest-time fields; runtime fields appear
as marked):

| field | type | meaning |
| --- | --- | --- |
| `need` | string | the owning need handle |
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
| `outside` | array of files | runtime (merged): files the branch touched outside `write_scope` |
| `replaced_by` | array of unit ids | runtime (replaced): the split's successor ids |
| `verdict_text` | string | runtime (no-defect): tail of the no-defect receipt |

Compatibility rule: the pump reads and writes exactly this shape. Unknown
top-level and per-unit keys are preserved on save. At load, a state older than
`plans_seen` gets it backfilled from `needs`. `run`/`once` additionally reset
every `merging` unit to `delivered` before the first tick, so a crash
mid-merge re-feeds the unit.

## 6. The unit state machine

States:

| state | meaning | terminal | satisfied |
| --- | --- | --- | --- |
| `pending` | waiting to dispatch | no | no |
| `inflight` | dispatched to a pool slot; a task handle exists | no | no |
| `delivered` | the worker's branch exists on the remote; waiting to merge | no | no |
| `merging` | queued to or being handled by the merge thread | no | no |
| `merged` | merged into the integration branch, proof passed, pushed | yes | yes |
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
| — | `pending` | a new plan file in `plans/` | each unit has `id`, `brief`, `write_scope`, a list `proof`; no duplicate id | unit record minted with `since=now`, appended to `order`; need recorded |
| `pending` | `inflight` | dispatch loop reaches it | `ready` (§8); a free pool slot; `pick_model` returns a model | task sent, pool dispatched, unit updated (`attempts+1`, `task`, `branch`, `model`, `since`, `since_dispatch`, `pool`) |
| `inflight` | `delivered` | collect: the task is not running and not reported, or reported `finished`, and the worker branch exists on the remote | receipt does not contain `agent exit 124` / `agent_rc 124` | `since=now`, log `delivered` |
| `inflight` | `no-defect` | collect: no remote branch, and the receipt contains `VERDICT: no-defect` | — | `verdict_text` kept, `no-defect` exception filed |
| `inflight` | `pending` or `stuck` | collect: no branch, no verdict marker | — | `fail`: history entry `{attempt, model, why, detail≤2500, at}`; `task=null`; `stuck` when `attempts >= max_attempts` (plus a `stuck` exception), else back to `pending` |
| `inflight` | `splitting` | timeout: reconcile state `timeout`, or still running past `budget + overdue_grace`, or the receipt shows agent exit 124 | — | `split`: `resplit/` files written, split planner spawned detached, `task=null`, history entry `why="timeout"`, `since=now` |
| `splitting` | `stuck` | split planner wrote no plan in `split_timeout` | — | `split-no-answer` exception |
| `splitting`/`pending` | `replaced` | a plan with `replaces: <uid>` is ingested | the plan passes the same unit checks | old unit `status="replaced"`, `replaced_by=<ids>`; empty `units` files a `split-empty` exception; successors minted as `pending` |
| `delivered` | `merging` | merge feed after collect | not `HOLD-MERGE`, or the id starts with the fix prefix | job put on the merge queue (state change only; the merge thread touches the repo, never the state) |
| `merging` | `merged` | merge result `ok=true` | — | `commit`, `outside`, `merges+=1`; `scope-warn` logged when `outside` is non-empty; need closed and a `needs-ready.txt` line appended when every unit of the need is satisfied |
| `merging` | `delivered` | merge result `ok=null` (`push-failed`, `merge-error`, `merge-undecided`) | — | back to `delivered`; the next feed retries |
| `merging` | `pending` or `stuck` | merge result `ok=false` (`fetch-failed`, `merge-conflict`, `proof-failed`) | — | `fail` as above, with the merge's `why`/`detail` |

The merge batch itself (which units go together, the remote merger, promotion)
is [`pump-protocols.md`](pump-protocols.md); the state machine above only records what
each merge outcome does to the unit.

## 7. The tick

`tick` runs these steps in order:

1. `merge_apply`: drain finished merge results into state.
2. `board`: sync pull, plan new needs, reply on threads — wrapped so that
   a failure logs `BOARD ERROR` and the tick continues.
3. `ingest`: read new plan files.
4. `collect`: reconcile pool state against inflight units and the gate task.
5. `merge_apply` again.
6. `merge_feed`: queue delivered units for merging.
7. `save` — save point one.
8. PAUSE check: if `PAUSE` exists, `write_status` and return — skipping
   dispatch, the hold cycle, external gate intake, the gate start, and the second merge apply/feed. Note merging is not paused: step 6 already ran, and
   the merge thread is independent of the tick.
9. `dispatch`.
10. `save` — save point two.
11. `hold_cycle`.
12. `external_gates`: take finished results from `gate.results_dir`, if set.
13. `start_gate`.
14. `merge_apply` — lets gate/merge results land in state.
15. `merge_feed`.
16. `save` — save point three.
17. `write_status`.

Error handling:

- Board errors are caught inside `tick` (step 2, above).
- Any other tick exception propagates to the `run`/`once` loop, which logs
  `TICK ERROR` with the panic note and saves the state before continuing. The loop does not exit on tick errors.
- Merge thread errors are caught inside `merge_thread`: every job in the batch
  gets an `ok=null` `merge-error` result, and any job the results did not name
  gets `merge-undecided`. `merge_apply` turns both into
  `delivered`, so nothing is lost or double-merged.
- State is saved at the three tick save points, after a tick error, and before exiting on STOP. The merge thread
  never saves; it hands results to `merge_apply` through the queue.

## 8. Scheduling

Readiness (`ready`):

- The unit must be `pending`.
- Every id in `after` must be `satisfied`.
- Write-scope exclusivity: no other unit whose write scope overlaps may be
  busy. Busy is `inflight` for fix units and
  `inflight`+`delivered`+`merging` for normal units. A fix unit
  (id starting with `fix_prefix`) ignores overlap against non-fix units.

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
remembered in `state.json`) so an operator sees it. Free slot counts come from
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
    --branch unit/<uid>-a<N> --budget 600 --porcelain
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
| `repo` | path | `<home>/repo` | the integration clone; at startup a missing one is cloned from `remote_url`, or the pump refuses to start when none is set |
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
| `fix_prefix` | string | `pumpfix` | unit ids with this prefix are fix units: dispatched first, merged even under `HOLD-MERGE`, scope-checked only against in-flight units |
| `from` | role | `operator` | role the pump sends unit tasks from |
| `to` | role | `hand` | recipient role of a unit dispatch |
| `reply_from` | role | `mind` | role that replies on need threads |

`[pump.board]`:

| key | type | default | controls |
| --- | --- | --- | --- |
| `from` | list of senders | none | senders whose open needs are plannable |
| `since` | ISO datetime | none | needs dated before this are skipped, unless marked or re-planned |
| `mark` | string | `pump` | approval mark: a marked open need is plannable whatever sender or date |
| `every` | int seconds | `300` | seconds between board passes |
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
| `pool` | pool name | none — **required** | the `[pools.<name>]` table units of this kind are dispatched to |
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
pool = "fly4"

[pump.dispatch.kinds.full]
pool = "flyfull"
max = 2
skip_proof = true
```

A plan unit with `"tools": "full"` goes to the `flyfull` pool; one with no
`tools` goes to `fly4`.

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
| `ignore_file` | path, optional | unset | test names dropped from every gate's failing set: one per line, blank lines and `#` comment lines skipped; relative paths join the project root |

External gate results: when `results_dir` is set, every tick takes each
`*.json` there, oldest first, as a finished gate. The file is
`{"tip", "rc", "names", "build_failed", "tail", "seconds", "host"}`: `names` is
the failing test names or `null`, `build_failed: true` finishes the gate as a
build failure carrying `tail`, `seconds` and `host` are only logged (`host` is
free text and optional), and the other keys are not read. The gate then goes
through the same classification as a worker's reply, and the consumed file moves to
`<results_dir>/done/`. A file that does not parse stays where it is. A due gate
that finds no result starts a wait (`external_wait_since` in `state.json`);
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
4. **An open need carrying the approval mark is plannable** whatever its sender
   or date: `marked_rows` lists held and to-mind open needs carrying `mark`,
   and the board adds them past the `since` cutoff.
5. **A parked need is re-planned when the operator replies** on its thread
   after the plan file was written: verdict in `operator|design|hold`, no
   units, plan superseded, need deleted, handle queued for re-planning.
6. **No environment variables for configuration:** config lives in `[pump]` and
   CLI flags only; the pump reads no environment variable for configuration.
7. **No speculative hooks.** The pump has no decision script. The one place
   config cannot express a decision today is gate-reply name extraction when
   the reply has no marker block; see §11.

## 11. Not built, and the first decision-hook event

Not built, as decided in this contract:

- The decision hook.
- The pump's state is not shown by `vivi boot`, `board` or `trace`; state
  stays a private `state.json` in `home`.
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

- The once-only `red-after-fix` report: an owned failing name still failing
  after its fix units all settled raises one exception per `owner:name`, then
  never again.
- The reconcile-miss re-check: a task absent from reconcile output is first
  given a 90-second grace, then re-checked against `pool status --porcelain`
  before the pump decides from the remote branch.
- The receipt scan for `agent exit 124` / `agent_rc 124` on the finished and
  failed paths, which routes to `split` rather than `fail`.
- Single-job batches bypass `batch_round` and its merge-main-first step; see the
  NOTE on ruling 2.
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
| `gate.rs` | `start_gate`, `finish_gate`, `gatefix`, `hold_cycle`, `promote`. |
| `board.rs` | `board`, `replan`, `replies`. |
| `run.rs` | `tick`, `write_status`, the run/once loop. |

Rust-specific decisions:

- The pump runs its own binary (`std::env::current_exe`) and `git`; it never
  links to pool internals.
- Every external effect goes through the `Host` seam, so tests script command
  lines instead of touching a real pool.
- No regex crate: the created-handle line and the gate name fallback use
  hand-written matchers.
- `state.json` keeps its field names and preserves unknown keys.
- The merge thread owns the repo and never the state.
- No environment variable is read.
