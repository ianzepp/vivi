# The pump's contract, part one

This is the contract of the pump as it behaves today, written from
`/mnt/agents/pump/pump.py` (read-only). It is part one of the contract; part two
is [`protocols.md`](protocols.md) (merge, remote merger, gate, board, plan
files, collect rules). Where this document needs those details it links there
instead of duplicating them.

References look like `pump.py:632 merge_work`. The design decisions are
decided in this contract; the contract states them as decided and does not
reopen them.

## 1. Purpose and non-goals

The pump is a Mind written as a state machine. It reads planned units from
`plans/*.json`, dispatches each ready unit to a pool slot, merges what comes
back into the integration branch, runs the unit's proof on the merged tree,
retries failures on another model, and writes what needs a person to
`exceptions.txt` (pump.py module docstring, lines 1–9).

- It never reads a receipt to decide anything except the literal marker
  `VERDICT: no-defect` (pump.py:488 collect). Every other receipt text is used
  only as failure detail to feed back into the next attempt or to file an
  exception.
- It is a generic engine. Project specifics (which project, which branch, which
  pools, which hand rules, which checks) are config in the `[pump]` tables of
  `.vivi/mailspace.toml`, not code. The Faber hand-rules text (`RULES`,
  pump.py:45) and the gate brief (`GATE_BRIEF`, pump.py:60) are the two blobs
  the engine replaces with config (`hand_rules`, gate `command`).
- Step 3 of the pump plan is the cutover: retiring `pump.py` and running the
  Rust pump on the same home and the same state.json. The cutover itself is not
  part of this contract's build; this contract is what the Rust pump must
  reproduce so a cutover can adopt the live state (§5).

## 2. Surface

`vivi pump run | status | once`, with the global `--project` flag naming the
project root (pump.py's equivalent is `PROJECT`, pump.py:27, and every `vivi`
call appends `--project <PROJECT>`, pump.py:101 vivi):

| mode | behaviour | reference |
| --- | --- | --- |
| `run` | loop: one `tick`, then sleep `tick` seconds, until the STOP file appears | pump.py:1065–1078 main |
| `once` | exactly one `tick`, then exit | pump.py:1072–1073 main |
| `status` | print `status.txt`, or `no status yet` when absent; no lock, no state load | pump.py:1054–1057 main |

With no argument pump.py defaults to `status` (pump.py:1054). `once` also
honours STOP: the check happens after the tick (pump.py:1072–1077).

Single instance: `run`/`once` open `<home>/lock` and take an exclusive
non-blocking flock before doing anything (pump.py:1058–1059 main). A second
instance's flock raises an uncaught `BlockingIOError`, so it exits non-zero
with a Python traceback as the message. NOTE: the design says "exits non-zero
with a message"; the message is the traceback, not a friendly line.

Exit codes: 0 on a clean exit (STOP seen, or `once` finished); 1 on lock
contention as above. A crashing tick is not an exit: tick exceptions are caught
and logged (§7).

`--project` is how the pump finds the mailspace; the pump runs its own binary
(`vivi`, through the command-line seam below) with `--project` appended.

The command-line seam. Every external command runs through `sh`
(pump.py:88–98):

```
bash -c "<shell_prefix>; <cmd>"
```

with `shell_prefix` the `ENV` prefix (`. /mnt/agents/faber-runs/pharos-env.sh`,
pump.py:28, prepended at pump.py:90), an optional cwd, and a timeout (default
120 s). It returns the exit code and combined stdout+stderr. On timeout it
returns exit code 124 plus the text `[timeout after Ns]` appended to whatever
partial output was captured (pump.py:94–98). This seam is also the test seam:
tests script a fake host that answers command lines. Detached processes
(planner, split, gatefix, sync push) use `bash -c` with `start_new_session`
and redirected files instead (pump.py:355, 388, 436, 927).

Operators restart the pump with `restart.sh`: touch STOP, wait up to 10 minutes
for the tmux session to end, kill the session, remove STOP, `py_compile`
pump.py, relaunch `python3 pump.py run` in tmux (restart.sh:1–13). It stops by
pid/session, never by matching a command line.

## 3. The home directory

`[pump] home`, default `<project>/.vivi/pump` (pump.py's `HOME`,
pump.py:25 = `/mnt/agents/pump`). One table, every file and directory:

| path | written by | read by | format |
| --- | --- | --- | --- |
| `state.json` | pump, atomic (`state.json.tmp` then rename, pump.py:120–124 save) | pump (load, pump.py:105) | JSON, indent 1; shape in §5 |
| `state.json.tmp` | pump (transient) | none | scratch |
| `log.txt` | pump `log()` (pump.py:82–86) | operator | one line per event: `HH:MM:SS msg`, UTC |
| `exceptions.txt` | pump `exception()` (pump.py:126–131) | operator | `HH:MM:SS kind unit text`; newlines in text become ` \| `; text capped at 600 chars |
| `needs-ready.txt` | pump on need completion (pump.py:852–856 merge_apply) | operator | `need tip10 unit,unit` per line |
| `status.txt` | pump `write_status` (pump.py:1014–1027) | `pump status` (pump.py:1055) | 4 lines: timestamp, `units k=v …`, `plans k=v …`, `merges N needs-ready N gates N baseline …` |
| `lock` | pump opens for flock (pump.py:1058) | none (lock only) | empty |
| `skip.txt` | operator | board pass (pump.py:337) | need handles to never plan, whitespace separated |
| `hygiene.on` | operator | batch shared checks (pump.py:712) | existence toggle; adds one cargo hygiene test. NOT carried to the engine (§11) |
| `plans/` | planner writes `<need>.json` (and `<uid>.split.json`) | pump ingest (pump.py:191) | plan JSON; see [`protocols.md`](protocols.md) |
| `plans/superseded/` | re-plan moves old plans here (pump.py:290–292) | operator | `<need>.<epoch>.json` |
| `needs/` | board dumps the need thread (pump.py:350) | planner (`PLAN_PROMPT`, pump.py:250) | JSON `{need, subject, triage, thread}` |
| `board/` | pump writes `<need>.prompt`; planner shell writes `<need>.out` (pump.py:352–355) | operator | prompt text; planner stdout+stderr |
| `resplit/` | pump writes `<unit>.json` and `<unit>.prompt`; planner writes `<unit>.out` (pump.py:429–436) | split planner | JSON + prompt text |
| `gatefix/` | pump writes `<need>.prompt`; planner writes `<need>.out` (pump.py:920–927) | gatefix planner | prompt text |
| `merge-jobs/` | pump writes `<bid>.job.json` (pump.py:776–780); remote merger writes `.claimed.json`/`.result.json` | pump reads the result (pump.py:791–800) | batch protocol; see [`protocols.md`](protocols.md) |
| `repo/` | git operations (pump.py:625 reset_repo and all merge work) | pump | the integration clone; `[pump] repo` overrides |
| `planner-charter.md` | operator | planner command lines (`{charter}`, pump.py:353, 434, 925) | the planner's system prompt |
| `planners/` | planner tooling | planner | observed in the live home; not referenced by pump.py |
| `sync-push.log` | detached `vivi sync push` (pump.py:388–389) | operator | shell redirect |
| `pump.py`, `restart.sh`, operator scripts (`lift-hold.sh`, `fly-*.sh`, `launch-planner.sh`), their logs, `pump.py.bak-*`, `review-*.md`, `__pycache__/` | operator | operator | not engine state |

## 4. Flag files

All flags live in `home`. The operator creates them; removal is per flag:

| flag | exact effect | removed by | reference |
| --- | --- | --- | --- |
| `PAUSE` | `tick` returns early after the first save point: no dispatch, no hold cycle, no gate start, no post-dispatch merge apply/feed. The steps before the check (merge apply, board, ingest, collect, first feed) still run, so already-fed units keep merging | operator | pump.py:1040–1042 tick |
| `STOP` | after the current tick completes, save and exit the loop | `restart.sh` (`rm -f STOP`) or operator | pump.py:1074–1077 main |
| `HOLD-MERGE` | `merge_feed` feeds only units whose id starts with the fix prefix; once every fix unit has settled and merged, `hold_cycle` writes `FORCE-GATE`; a clean gate newer than the hold lifts it (removes `HOLD-MERGE`) | pump (`hold_cycle`, pump.py:938–941) or operator | pump.py:858–863 merge_feed, pump.py:931–947 hold_cycle |
| `FORCE-GATE` | `start_gate` treats the gate as due now, whatever the counters say | pump, when the gate dispatches (pump.py:901–902); also written by `hold_cycle` (pump.py:943–944) | pump.py:876–877 start_gate |
| `REMOTE-MERGE` | merge batches are offered to a remote merger first (file protocol under `merge-jobs/`); this host merges only if the offer is not claimed in time | operator | pump.py:773–800 remote_merge |

## 5. The state file

`state.json` is the pump's whole memory. It is saved atomically and reloaded
at startup. Top-level keys, from the live state (read-only inspection):

| key | type | meaning | reference |
| --- | --- | --- | --- |
| `units` | object: unit id → unit record | every unit ever ingested | pump.py:224–231 ingest |
| `order` | array of unit ids | dispatch and merge order (fix units sorted first at dispatch, pump.py:579) | pump.py:231 ingest |
| `needs` | object: need handle → `{verdict, reason, units[]}` | one record per planned need; verdict is the planner's (`units`, `hold`, `operator`, `design`, `delivered`, `invalid`, …) | pump.py:232–243 ingest |
| `merges` | int | count of units that reached `merged` | pump.py:846 merge_apply |
| `last_gate` | object `{at, merges, task, tip, pool}` | the running or last gate; `task` is null when no gate is in flight | pump.py:903 start_gate |
| `baseline` | array of failing names, or null | failing names of the first readable gate; new failures are measured against it | pump.py:987–989 finish_gate |
| `closed_needs` | array of need handles | needs whose every unit is satisfied | pump.py:850–855 merge_apply |
| `gates` | array of gate records `{tip, at, merges, names, [new]}` | every finished gate, unreadable ones included | pump.py:981, 1009 finish_gate |
| `plans_seen` | array of plan file names | plan files already ingested (dedup) | pump.py:199–206 ingest |
| `board` | object `{at, planning{h→start}, replied[keys], replan[h]}` | board pass bookkeeping: last pass time, planners in flight, replies already sent, needs awaiting re-plan | pump.py:322 board, pump.py:275–299 replan_answered |
| `red` | object: failing name → owning fix-need prefix | failures owned by a gate-fix plan; later gates ignore owned names | pump.py:992, 1002 finish_gate |
| `red_reported` | array `owner:name` | owned names already reported `red-after-fix` once | pump.py:978–979 finish_gate (absent until first use) |
| `promotions` | array `{tip, at}` | fast-forwards of main | pump.py:959–960 promote (absent until first promotion) |
| `rr` | int | observed in the live state; removed from pump.py by commit 13eb290 — the Rust pump never reads or writes it | NOTE: unknown key, preserved by save |

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
| `tools` | string, `plain` or `full` | pool selection (pump.py:585–588) and whether the merger runs the unit's own proof (pump.py:655, 726) |
| `status` | string | the state machine state (§6) |
| `attempts` | int | dispatches so far; incremented at dispatch (pump.py:613) |
| `history` | array of `{attempt, model, why, detail, at}` | one entry per failure or split request (pump.py:405–415 fail, pump.py:441 split); `detail` capped at 2500 chars |
| `task` | 8-hex handle or null | the pool task while `inflight`, else null |
| `branch` | `unit/<uid>-aN` or null | the worker branch |
| `model` | string or null | the model of the current/last attempt |
| `since` | epoch seconds | set at ingest and at each state change |
| `pool` | string | runtime: pool dispatched to (pump.py:613) |
| `since_dispatch` | epoch seconds | runtime: dispatch time, used for the overdue check |
| `commit` | string | runtime (merged): integration-branch tip the merge landed on (pump.py:845) |
| `outside` | array of files | runtime (merged): files the branch touched outside `write_scope` (pump.py:647, 700) |
| `replaced_by` | array of unit ids | runtime (replaced): the split's successor ids (pump.py:233–234) |
| `verdict_text` | string | runtime (no-defect): tail of the no-defect receipt (pump.py:490) |

Compatibility rule: the Rust pump reads and writes exactly this shape, same
field names, so the cutover adopts the live state. Unknown top-level keys are
preserved (pump.py's `save` dumps the whole dict back, pump.py:120–124 — the
live `rr` key survives this way). At startup, load applies three repairs
(pump.py:105–118): `fix_proof` is re-applied to every unit's proof; a `pending`
unit whose last history entry says `unexpected argument` has that attempt
undone (attempts decremented, entry popped); a state older than `plans_seen`
gets it backfilled from `needs`. `run`/`once` additionally reset every `merging`
unit to `delivered` before the first tick (pump.py:1061–1063 main), so a crash
mid-merge re-feeds the unit. Of the load repairs only the `merging` →
`delivered` reset is carried to the engine; the other two are legacy and not
built (§11).

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
| `replaced` | superseded by split successor units | yes | only if every successor is satisfied (pump.py:521–523 satisfied, recursion depth capped at 6) |

`satisfied` (pump.py:516–525) is the readiness predicate used everywhere:
`merged` and `no-defect` are directly satisfied; `replaced` delegates to its
successors; everything else, `stuck` included, is not. A stuck unit therefore
blocks its dependents and keeps its need open forever; only a person unblocks
it. Record as-is.

Transitions (from, to, trigger, guard, action):

| from | to | trigger | guard | action | reference |
| --- | --- | --- | --- | --- | --- |
| — | `pending` | a new plan file in `plans/` | each unit has `id`, `brief`, `write_scope`, a list `proof`; no duplicate id | unit record minted with `since=now`, appended to `order`; need recorded | pump.py:191–244 ingest |
| `pending` | `inflight` | dispatch loop reaches it | `ready` (§8); a free pool slot; `pick_model` returns a model | task sent, pool dispatched, unit updated (`attempts+1`, `task`, `branch`, `model`, `since`, `since_dispatch`, `pool`) | pump.py:576–620 dispatch |
| `inflight` | `delivered` | collect: the task is not running and not reported, or reported `finished`, and the worker branch exists on the remote | receipt does not contain `agent exit 124` / `agent_rc 124` | `since=now`, log `delivered` | pump.py:475–483 collect |
| `inflight` | `no-defect` | collect: no remote branch, and the receipt contains `VERDICT: no-defect` | — | `verdict_text` kept, `no-defect` exception filed | pump.py:488–492 collect |
| `inflight` | `pending` or `stuck` | collect: no branch, no verdict marker | — | `fail`: history entry `{attempt, model, why, detail≤2500, at}`; `task=null`; `stuck` when `attempts >= max_attempts` (plus a `stuck` exception), else back to `pending` | pump.py:405–415 fail, pump.py:493 collect |
| `inflight` | `splitting` | timeout: reconcile state `timeout`, or still running past `budget + overdue_grace`, or the receipt shows agent exit 124 | — | `split`: `resplit/` files written, split planner spawned detached, `task=null`, history entry `why="timeout"`, `since=now` | pump.py:428–443 split, pump.py:462–467 and 485–487 collect |
| `splitting` | `stuck` | split planner wrote no plan in `split_timeout` | — | `split-no-answer` exception | pump.py:494–497 collect |
| `splitting`/`pending` | `replaced` | a plan with `replaces: <uid>` is ingested | the plan passes the same unit checks | old unit `status="replaced"`, `replaced_by=<ids>`; empty `units` files a `split-empty` exception; successors minted as `pending` | pump.py:204–215, 232–241 ingest |
| `delivered` | `merging` | merge feed after collect | not `HOLD-MERGE`, or the id starts with the fix prefix | job put on the merge queue (state change only; the merge thread touches the repo, never the state) | pump.py:858–867 merge_feed |
| `merging` | `merged` | merge result `ok=true` | — | `commit`, `outside`, `merges+=1`; `scope-warn` logged when `outside` is non-empty; need closed and a `needs-ready.txt` line appended when every unit of the need is satisfied | pump.py:844–855 merge_apply |
| `merging` | `delivered` | merge result `ok=null` (`push-failed`, `merge-error`, `merge-undecided`) | — | back to `delivered`; the next feed retries | pump.py:838–841 merge_apply |
| `merging` | `pending` or `stuck` | merge result `ok=false` (`fetch-failed`, `merge-conflict`, `proof-failed`) | — | `fail` as above, with the merge's `why`/`detail` | pump.py:842–843 merge_apply |

The merge batch itself (which units go together, the remote merger, promotion)
is [`protocols.md`](protocols.md); the state machine above only records what
each merge outcome does to the unit.

## 7. The tick

`tick` (pump.py:1029–1050) runs these steps in order:

1. `merge_apply`: drain finished merge results into state (pump.py:1030).
2. `board`: sync pull, plan new needs, reply on threads — wrapped in
   `try/except`; an exception logs `BOARD ERROR` and the tick continues
   (pump.py:1031–1034).
3. `ingest`: read new plan files (pump.py:1035).
4. `collect`: reconcile pool state against inflight units and the gate task
   (pump.py:1036).
5. `merge_apply` again (pump.py:1037).
6. `merge_feed`: queue delivered units for merging (pump.py:1038).
7. `save` (pump.py:1039) — save point one.
8. PAUSE check: if `PAUSE` exists, `write_status` and return — skipping
   dispatch, the hold cycle, the gate start, and the second merge apply/feed
   (pump.py:1040–1042). Note merging is not paused: step 6 already ran, and
   the merge thread is independent of the tick.
9. `dispatch` (pump.py:1043).
10. `save` (pump.py:1044) — save point two.
11. `hold_cycle` (pump.py:1045).
12. `start_gate` (pump.py:1046).
13. `merge_apply` (pump.py:1047) — lets gate/merge results land in state.
14. `merge_feed` (pump.py:1048).
15. `save` (pump.py:1049) — save point three.
16. `write_status` (pump.py:1050).

Error handling:

- Board errors are caught inside `tick` (step 2, above).
- Any other tick exception propagates to the `run`/`once` loop, which logs
  `TICK ERROR` with the traceback tail and saves the state before continuing
  (pump.py:1066–1071). The loop does not exit on tick errors.
- Merge thread errors are caught inside `merge_thread`: every job in the batch
  gets an `ok=null` `merge-error` result, and any job the results did not name
  gets `merge-undecided` (pump.py:807–828). `merge_apply` turns both into
  `delivered`, so nothing is lost or double-merged.
- State is saved at the three tick save points, after a tick error
  (pump.py:1071), and before exiting on STOP (pump.py:1076). The merge thread
  never saves; it hands results to `merge_apply` through the queue.

## 8. Scheduling

Readiness (`ready`, pump.py:527–543):

- The unit must be `pending`.
- Every id in `after` must be `satisfied` (pump.py:531–533).
- Write-scope exclusivity: no other unit whose write scope overlaps may be
  busy. Busy is `inflight` for fix units and
  `inflight`+`delivered`+`merging` for normal units (pump.py:538). A fix unit
  (id starting with `fix_prefix`) ignores overlap against non-fix units
  (pump.py:540–541).

Overlap uses `scope_root` (pump.py:134–141): strip a leading `./`, truncate at
the first glob character (`*?[`) and keep the directory part; a path with no
directory becomes `""`. `overlaps` (pump.py:143–150) returns true when any pair
of paths shares a scope root, or when either root is `""` — a root-level or
glob-only scope overlaps everything.

Model choice (`pick_model`, pump.py:546–561):

- Count inflight units per model (pump.py:547–550).
- If `attempts >= max_attempts - 1`, return `last_resort` (pump.py:551–552).
- Walk the `lanes` in order, skipping the model of the last attempt if another
  is free under its cap (pump.py:553–557); then walk again without the
  not-the-last-model preference (pump.py:558–560). Lane order alternates with
  attempt parity: even attempts walk the configured order, odd attempts walk it
  reversed (pump.py:553).
- No model under any cap returns `None`, which breaks the dispatch loop for
  this tick (pump.py:582–583).

Pool choice (pump.py:585–588): `full` tools go to `pool_full`, everything else
to `pool_plain`. When `full_max > 0`, a `full` unit is skipped — not failed,
not counted as started — while at least `full_max` units are already inflight
on `pool_full` (`FULL_UNITS_MAX = 2`, pump.py commit 7718a9f); same-tick
starts count toward the cap, and the skipped unit is simply retried next
tick. The cap keeps one full-toolchain machine free for gates. Free slot
counts come from `pool status --porcelain`
(pump.py:506–514 slots). At most `per_tick` units start per tick
(pump.py:579–580, literal `6`), and fix units are offered first
(`sorted` key at pump.py:579).

Brief composition (`brief_for`, pump.py:564–574), in order:

1. `hand_rules` — the embedded `RULES` text (pump.py:45–58).
2. `UNIT <id>: <title>`.
3. The unit's `brief`.
4. `WRITE SCOPE:` and the one-line-per-path list.
5. `PROOF (each must exit 0):` and the command list, or
   `- (stage 1 and stage 2 only)` when empty.
6. When history exists: the `A PREVIOUS ATTEMPT … FAILED` block with the last
   entry's `why` and the last 1800 chars of its `detail`
   (pump.py:571–573).
7. The `done_when:` line: the PROOF commands and stage 1 pass, or the reply
   ends with `VERDICT: no-defect` and evidence (pump.py:574).

Task send and dispatch command lines (pump.py:598–606):

```
vivi task send --from operator --to hand --subject "[<uid> a<N>] <title>" \
    --body-file /tmp/pump-brief-<uid>.txt        # subject truncated to 170 chars
vivi pool dispatch --pool <pool> <task> --model <model> --from <branch> \
    --branch unit/<uid>-a<N> --budget 600 --porcelain
```

with `--from`/`--to`/budget from config (§9). The created handle is read with
the regex `^(?:node|created)\s+(?:\S+\s+)?([0-9a-f]{8})\s*$` in multiline mode
(pump.py:600); no match logs `task send failed` and skips the unit. A failed
pool dispatch logs `dispatch failed`, marks the task done with
`dispatch failed; superseded`, and zeroes the pool's free count when the output
says `no free slot` (pump.py:607–612).

## 9. Config reference

All keys live in the `[pump]` tables of `.vivi/mailspace.toml` and are optional
unless marked. Every default equals the pump.py constant or literal it
replaces.

`[pump]`:

| key | type | default | replaces | controls |
| --- | --- | --- | --- | --- |
| `home` | path | `<project>/.vivi/pump` | `HOME = "/mnt/agents/pump"` (pump.py:25) | the home directory (§3) |
| `repo` | path | `<home>/repo` | `REPO = HOME + "/repo"` (pump.py:26) | the integration clone; at startup a missing one is cloned from `remote_url`, or the pump refuses to start when none is set |
| `remote` | string | `origin` | `origin` literals (pump.py:626–629, 637, 664, 683, 743) | the git remote (name) the integration branch and main live on; the name given to the clone's remote |
| `remote_url` | string | none | — (pump.py has no clone code) | URL or path the missing integration clone is created from (`git clone -o <remote> --branch <branch>`) |
| `committer_name` | string | none — **required** | pump.py commits as the host's git identity | `user.name` of every commit and merge the pump makes, passed as `git -c user.name=…` |
| `committer_email` | string | none — **required** | same | `user.email`, passed as `git -c user.email=…` |
| `branch` | string | `next` — **required** | `BRANCH = "next"` (pump.py:29) | the integration branch |
| `main` | string | `main` | `main` literals (pump.py:683, 950) | main branch merged into the integration branch each batch; `""` disables both merging main in and promotion |
| `promote` | bool | `true` | `promote()` called on a clean gate (pump.py:1007–1008) | a gate with no new failure and no owned failure still red fast-forwards main to the gated tip (pump.py:1041–1046) |
| `tick` | int seconds | `20` | `TICK = 20` (pump.py:42) | sleep between ticks (pump.py:1078) |
| `shell_prefix` | string | `""` | `ENV = "/mnt/agents/faber-runs/pharos-env.sh"` (pump.py:28, used at pump.py:90) | prepended to every command line (§2) |
| `hand_rules` | path | embedded generic text | `RULES` (pump.py:45–58, used at pump.py:565) | file whose text opens every unit brief; a relative path joins the project root like `home`; `validate` rejects a file that cannot be read |
| `reserved_file` | path | `reserved.txt` | — | paths held by another session; units overlapping them are not dispatched; a relative value joins `home` |
| `fix_prefix` | string | `pumpfix` | `pumpfix` literals (pump.py:534, 540, 579, 862, 920, 935, 1002) | unit ids with this prefix are fix units: dispatched first, merged even under `HOLD-MERGE`, scope-checked only against in-flight units |
| `from` | role | `operator` | `--from operator` (pump.py:598) | role the pump sends unit tasks from |
| `to` | role | `hand` | `--to hand` (pump.py:598) | recipient role of a unit dispatch |
| `reply_from` | role | `mind` | `--from mind` (pump.py:382) | role that replies on need threads |

`[pump.board]`:

| key | type | default | replaces | controls |
| --- | --- | --- | --- | --- |
| `from` | list of senders | `["operator@bd371d96.local"]` | `BOARD_FROM` (pump.py:246) | senders whose open needs are plannable |
| `since` | ISO datetime | `2026-10-14T00:10` | `BOARD_CUTOFF` (pump.py:247, compared at pump.py:345) | needs dated before this are skipped, unless marked or re-planned |
| `mark` | string | `pump` | `APPROVAL_MARK` (pump.py:301) | approval mark: a marked open need is plannable whatever sender or date |
| `every` | int seconds | `300` | `BOARD_EVERY` (pump.py:248, checked at pump.py:323) | seconds between board passes |
| `planners_max` | int | `3` | `PLANNERS_MAX` (pump.py:249, checked at pump.py:347) | concurrent planners |
| `plan_timeout` | int seconds | `1800` | `> 1800` (pump.py:338) | seconds a planner has to write `plans/<need>.json` before a `plan-no-answer` exception |
| `reply_to` | address | first of `from` | `BOARD_FROM` as reply target (pump.py:382) | `--to` of thread replies |
| `pull` | bool | `true` | unconditional `vivi sync pull` (pump.py:325) | run `vivi sync pull` at the start of a pass |
| `push_since` | duration | `30m` | `--since 30m` (pump.py:388) | after replying: detached `vivi sync push --since <push_since> --drain` |

`[pump.planner]`:

| key | type | default | replaces | controls |
| --- | --- | --- | --- | --- |
| `run` | command template | the `claude -p …` line (pump.py:353, 434, 925) | same | the planner command; prompt file on stdin, stdout+stderr to the `.out` file, cwd `home` |
| `charter` | path | `/mnt/agents/pump/planner-charter.md` | `--append-system-prompt-file %s/planner-charter.md` (pump.py:353) | the `{charter}` the planner runs under |
| `split_timeout` | int seconds | `1500` | `> 1500` (pump.py:495) | seconds a split planner has to answer before the unit is `stuck` |

`[pump.dispatch]`:

| key | type | default | replaces | controls |
| --- | --- | --- | --- | --- |
| `lanes` | list of `{model, cap}` | `[{deepseek/deepseek-flash, 30}, {zai/glm-5.3-flash, 10}]` | `LANES` (pump.py:36) | model lanes and per-model inflight caps (§8) |
| `last_resort` | string | `zai/glm-5.3` | `LAST_RESORT` (pump.py:37) | used when `attempts >= max_attempts - 1` |
| `max_attempts` | int | `3` | `MAX_ATTEMPTS` (pump.py:38) | attempts before `stuck` (pump.py:410) |
| `budget` | int seconds | `600` | `BUDGET` (pump.py:39) | `--budget` of a unit dispatch (pump.py:605) |
| `overdue_grace` | int seconds | `900` | `BUDGET + 900` (pump.py:462) | running longer than `budget + overdue_grace` means split |
| `per_tick` | int | `6` | `started >= 6` (pump.py:579) | units started per tick at most |
| `from` | string | the `branch` value | `--from %s` filled with `BRANCH` (pump.py:605) | `--from` of a unit dispatch |
| `pool_plain` | pool name | `fly4` | `pool = "fly4"` (pump.py:588) | pool for units with `tools = "plain"` |
| `pool_full` | pool name | `flyfull` | `pool = "flyfull"` (pump.py:586) | pool for units with `tools = "full"` |
| `full_max` | int | `0` | `FULL_UNITS_MAX = 2` (pump.py:43, as of commit 7718a9f) | units allowed in flight on `pool_full` at once; `0` = no cap; keeps one full-toolchain machine free for gates (§8) |

`[pump.merge]`:

| key | type | default | replaces | controls |
| --- | --- | --- | --- | --- |
| `batch` | int | `8` | `BATCH = 8` (pump.py:671, used at pump.py:806) | jobs merged together at most |
| `format` | command | `cargo fmt --all` | `sh("cargo fmt --all")` (pump.py:648, 708) | run after merging; a change is committed `style: cargo fmt after <ids>` (pump.py:651, 711) |
| `shared_checks` | list of commands | `["./scripta/test --stage 1", "./scripta/test --stage 2"]` | `SHARED` (pump.py:672, used at pump.py:713) | after every merge, before the units' own proof |
| `proof_timeout` | int seconds | `300` | `PROOF_TIMEOUT` (pump.py:43, used at pump.py:657, 719, 733) | seconds per check command |
| `remote_wait` | int seconds | `90` | `REMOTE_WAIT` (pump.py:769) | seconds a remote merger has to claim a batch before this host merges it |
| `remote_limit` | int seconds | `1800` | `REMOTE_LIMIT` (pump.py:770) | seconds a claimed batch may take |

`[pump.gate]`:

| key | type | default | replaces | controls |
| --- | --- | --- | --- | --- |
| `command` | command | `./scripta/test --stage 4b` | the command inside `GATE_BRIEF` (pump.py:62) | run by a read-only gate worker; it must list failing tests between FAILING-BEGIN / FAILING-END |
| `every_merges` | int | `8` | `GATE_EVERY_MERGES` (pump.py:40, used at pump.py:877) | merges since the last gate that make one due |
| `every_seconds` | int seconds | `1800` | `GATE_EVERY_SECONDS` (pump.py:41, used at pump.py:878) | also due when merges > the last gate's and this long has passed |
| `pools` | list of pool names | `["flyfull", "flygate"]` | `flyfull`/`flygate` fallback (pump.py:882) | first with a free slot wins |
| `model` | string | `zai/glm-5.3-flash` | `--model zai/glm-5.3-flash` (pump.py:896) | the gate worker's model |
| `budget` | int seconds | `1800` | `--budget 1800` (pump.py:896) | the gate dispatch budget |
| `results_dir` | path, optional | unset | none (generalises pump.py:891) | directory an external runner drops `<tip>.json` gate results in; relative paths join the project root like `home`. Unset: pool gates only |
| `results_wait_seconds` | int seconds | none; must be >= 1 when `results_dir` is set | none (pump.py:877 uses 2700 as its example) | how long a due gate waits for an external result before a pool gate is dispatched instead |
| `ignore_file` | path, optional | unset | none (generalises pump.py:991 `known-red.txt`) | test names dropped from every gate's failing set: one per line, blank lines and `#` comment lines skipped; relative paths join the project root |

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

One pump.py constant has no config key: the 2400-second gate collection grace
(pump.py:500) — a gate task absent from reconcile is finished from the receipt
after that long. The engine keeps the behaviour; it is not configurable.

Validation rules (pump.py has none — the engine adds them):

- `branch` is required.
- `committer_name` and `committer_email` are required.
- `gate.command` must not be empty (always).
- `merge.shared_checks` must not be empty while promotion is enabled
  (`promote = true` and `main` non-empty): with no checks a merge would land
  unchecked.
- `hand_rules`, when set, must be a readable file (relative to the project
  root).
- `tick` >= 1.
- `lanes` is non-empty and every lane's `cap` >= 1.
- `max_attempts` >= 1.
- Every pool named (`pool_plain`, `pool_full`, gate `pools`) must exist in
  `[pools.*]`.

## 10. Rulings the pump keeps

1. **A red gate never stops merging.** New failures are owned by a gate-fix
   plan (`red[name] = <fix need>`, pump.py:1002) and later gates subtract owned
   names (pump.py:996). Merging, dispatch, and feeding continue while red.
2. **Main moves continuously.** Every merge batch first merges main into the
   integration branch (pump.py:683–690 batch_round), and a gate with no new
   failure and no owned failure still red fast-forwards main — never forces
   (a gate that still shows an owned failure logs `promote held` and main waits
   for a clean one: main never gets redder, pump.py:1041–1046); a main with commits the
   tip lacks waits for the next batch (`merge-base --is-ancestor` check,
   pump.py:951–954 promote). NOTE: a single-job batch skips the batch path and
   goes straight to `merge_work` (pump.py:811–815 merge_thread), which does not
   merge main first; record as-is.
3. **Hands run only the unit's proof plus the cheap static stage.** The hand
   rules say stage 1 only, never stage 2/clippy/rustfmt (pump.py:50–53 RULES);
   the merger runs stage 1 + stage 2 + the unit's proof on the merged result
   (pump.py:655, 713–714, 726–731). A timeout means split the unit, never
   retry it (pump.py:462–467, 485–487 collect → `split`).
4. **An open need carrying the approval mark is plannable** whatever its sender
   or date (need 70418891): `marked_rows` lists held and to-mind open needs
   carrying `mark` (pump.py:304–317), and the board adds them past the
   `since` cutoff (pump.py:333–336, 345).
5. **A parked need is re-planned when the operator replies** on its thread
   after the plan file was written: verdict in `operator|design|hold`, no
   units, plan superseded, need deleted, handle queued for re-planning
   (pump.py:253 PARKED, pump.py:256–273 operator_replied_after,
   pump.py:275–299 replan_answered).
6. **No environment variables for configuration** (operator ruling 2026-10-14):
   config in `[pump]` and CLI flags only; the pump itself reads no environment
   variable for configuration. NOTE: pump.py:27 resolves `PROJECT` through
   `os.path.expanduser`, which consults the process home directory; the engine
   replaces this with `--project`.
7. **No speculative hooks.** The pump starts without the decision script. The
   one place config cannot express a decision today is gate-reply name
   extraction, which pump.py covers with a cargo-specific regex fallback
   (pump.py:971–976 finish_gate); see §11.

## 11. Not built in this step, and the first decision-hook event

Not built in this step, as decided in this contract:

- The decision hook.
- The pump's state is not shown by `vivi boot`, `board` or `trace`; state
  stays a private `state.json` in `home`, shaped like pump.py's so a cutover
  can adopt it.
- Units and plans are plan files, not Vivi graph nodes.
- The cargo-specific `fix_proof` rewrite (splitting `cargo test a b` into one
  command per filter; pump.py:152–177, applied at pump.py:109 and 228): the
  planner charter already demands one filter per command.
- The legacy `unexpected argument` history repair at load (pump.py:110–112).
- `hygiene.on` (pump.py:712): that check is just another entry of
  `merge.shared_checks`.
- Step 3, the cutover retiring pump.py, is the operator's.

What is built is verified by unit tests against a scripted fake host and by a
CLI smoke test; not yet run against live pools or a remote.

The first decision-hook event (the one config cannot express): `gate_result` —
classifying a gate worker's reply into failing test names when it carries no
FAILING-BEGIN/FAILING-END block. History: three `gate-unreadable` exceptions;
pump.py's regex fallback
`\b([a-z][a-z0-9_-]+) ((?:[A-Za-z0-9_]+::)+[A-Za-z0-9_]+)\b` is cargo-test-output
specific. Set arithmetic (new = names − baseline − owned, after the ignore file is dropped) is
generic and stays in the engine; only name extraction from an unmarked log is
project-specific. Proposed closed verdict set: `{"failing": ["<crate> <test>",
…]}` or `{"build_failed": true}`; a nonzero exit, invalid JSON, or an unknown
verdict fails closed (exception, no action). Not built in this step.

pump.py behaviours found that the decisions above do not list:

- The once-only `red-after-fix` report: an owned failing name still failing
  after its fix units all settled raises one exception per `owner:name`, then
  never again (pump.py:976–980 finish_gate).
- The reconcile-miss re-check: a task absent from reconcile output is first
  given a 90-second grace (pump.py:468–469), then re-checked against
  `pool status --porcelain` before the pump decides from the remote branch
  (pump.py:470–474).
- The receipt scan for `agent exit 124` / `agent_rc 124` on the finished and
  failed paths, which routes to `split` rather than `fail`
  (pump.py:477–479, 485–487).
- The `plans_seen` backfill for states older than that key
  (pump.py:113–114 load).
- Single-job batches bypass `batch_round` and its merge-main-first step
  (pump.py:811–815 merge_thread); see the NOTE on ruling 2.
- A failed pool dispatch closes the just-sent task with
  `task done … --note 'dispatch failed; superseded'` and, on `no free slot`,
  zeroes the pool's free count for the rest of the tick (pump.py:607–612).
- The split history entry carries `why="timeout"` with an empty `detail`
  (pump.py:441).

## 12. Implementation map

The engine is a Rust binary; each module ports one region of `pump.py`.

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

| Module (`src/pump/`) | Ports (pump.py functions and lines) |
| --- | --- |
| `config.rs` | The `[pump]` table; no pump.py equivalent: replaces the constants of pump.py:25–43, 246–253, 301, 768–771. |
| `state.rs` | load/save (pump.py:105–123); the `MergeJob`/`MergeResult` records. |
| `plan.rs` | `scope_root`, `overlaps`, `ingest` (pump.py:134–150, 191–241). |
| `ready.rs` | `satisfied`, `ready`, `pick_model` (pump.py:516–561). |
| `brief.rs` | `RULES`, `brief_for`, `GATE_BRIEF` (pump.py:45–75, 564–574). |
| `host.rs` | `sh`, `vivi`, `git`, `log`, `exception` (pump.py:78–102, 126–131, 621): the command-line seam and its test fake. |
| `collect.rs` | `receipt`, `fail`, `split`, `collect` (pump.py:395–501). |
| `dispatch.rs` | `slots`, `dispatch` (pump.py:506–617). |
| `merge.rs` | `merge_work`, `batch_round`, `batch_work`, the merge thread, `feed`, `apply` (pump.py:625–761, 803–867). |
| `remote.rs` | `remote_merge` (pump.py:768–800). |
| `gate.rs` | `start_gate`, `finish_gate`, `gatefix`, `hold_cycle`, `promote` (pump.py:872–1009). |
| `board.rs` | `board`, `replan`, `replies` (pump.py:244–390). |
| `run.rs` | `tick`, `write_status`, `main` (pump.py:1014–1078). |

Rust-specific decisions:

- The pump runs its own binary (`std::env::current_exe`) and `git`; it never
  links to pool internals.
- Every external effect goes through the `Host` seam, so tests script command
  lines instead of touching a real pool.
- No regex crate: the created-handle line and the gate name fallback use
  hand-written matchers.
- `state.json` keeps pump.py's field names and preserves unknown keys.
- The merge thread owns the repo and never the state.
- No environment variable is read.
