# Pump protocols: the wire contract (part two)

This is part two of the pump's contract. It documents every external command
line and file protocol `vivi pump` speaks, as the code behaves. The engine was
ported from a Python predecessor; this file describes the Rust engine itself.
Part one — surface, home layout, flag files, state fields, the unit state
machine, the tick, scheduling, and the config table — is
[`pump-contract.md`](pump-contract.md); this file does not repeat it.

Angle-bracket names in command lines (`<remote>`, `<branch>`, `<main>`,
`<from>`, `<to>`) are config values from the `[pump]` tables
(`remote`, `branch`, `main`, `from`, `to`, and so on); the defaults and
validation rules are in `pump-contract.md` §9. Example values in this file
(`/srv/pump`, `you@example.com`, `my-project`) are illustrative.

Every command line runs through one funnel:

- `sh(cmd, timeout=120, cwd=None)`: `bash -c "<shell_prefix>; <cmd>"`
  (the prefix is omitted when empty), combined stdout+stderr, optional cwd. On
  timeout it returns `rc=124` with the partial output plus the line
  `[timeout after <n>s]`.
- `vivi(args, timeout=120)`: `sh("vivi " + args + " --project <project>")`,
  for example `--project /home/you/my-project`.
- `git(cmd, timeout=300)`: `sh("git " + cmd, cwd=<home>/repo)`.
- Planner launches are detached processes with no timeout; the pump watches
  their output files instead.

## 1. Commands the pump runs

### 1.1 `vivi` commands

| Command line | Context (timeout) | Output parsed, and how |
| --- | --- | --- |
| `vivi sync pull --project <proj>` (when `board.pull`) | board (120 s) | Nothing machine-read; last output line logged as `sync pull rc=<n> …`. |
| `vivi need list --from <sender> --status open --json` | board (60 s) | JSON from the first `[` in the output (`out[out.index("["):]`); rows read for keys `handle`, `date`, `subject`. Unparseable → `board list unreadable:` log, no rows. |
| `vivi need list --status open --json --held` and `vivi need list --status open --json --to <reply_from>` | `marked_rows` (60 s) | Same JSON-from-`[` slice; rows kept only when the `marks` list contains the approval mark (`board.mark`, default `pump`). Deduped by `handle`. Each failure logs `marked list unreadable (…)`. |
| `vivi need show <handle>` | `operator_replied_after`,  board (60 s) | Raw thread text. Board stores `thread[:40000]` into `needs/<h>.json`. `operator_replied_after` scans line by line: a header `re.match(r"## (\S+) - [0-9a-f]+ \[")` sets the current timestamp; a later `From: <sender>@` line whose header timestamp parses as ISO and is newer than `since` (epoch) returns true. |
| `vivi mail reply <h> --from <reply_from> --to <reply_to> --body-file <home>/board/reply-<h>-<stage>.txt` | board (60 s) | `rc == 0` marks the stage replied. |
| `vivi sync push --since <push_since> --drain --project <proj>` | board (detached) | Never waited on; output appended to `<home>/sync-push.log`. |
| `vivi pool reconcile --porcelain` |  collect (300 s) | Lines `reconciled|collected <task> <pool> <slot> <state> <detail>`. Split with `shlex.split`; a line counts when it has ≥ 6 fields and field 0 is `reconciled` or `collected`; fields 1, 4, 5 are taken: `seen[task] = (state, detail)`. |
| `vivi pool status --porcelain` | `slots`,  collect (120 s) | Slot lines `slot <pool> <name> <id> <state> …` split on whitespace: `p[0]=="slot"` and `p[4]=="free"` increments the free count for pool `p[1]`. In collect, a plain substring test `" " + task + " "` asks "is this task still on some slot". |
| `vivi task show <task> --latest` | `receipt` (60 s) | The receipt: last 6000 characters of combined output. The pump decides nothing from it except the literal marker `VERDICT: no-defect` and the exit-124 patterns (section 3). |
| `vivi task send --from <from> --to <to> --subject <subj> --body-file <home>/briefs/<uid>.txt` (units) or a gate brief file (gate) |  dispatch (120 s default),  `start_gate` (120 s default) | Created-handle regex `re.search(r"^(?:node\|created)\s+(?:\S+\s+)?([0-9a-f]{8})\s*$", out, re.M)` — the first 8-hex handle on its own line. No match → dispatch aborts for that unit (`task send failed …` log). Unit subject is `"[<uid> a<n>] <title>"`; gate subject is `"[pump gate] <gate.command> at <tip10>"`. NOTE: the 170-character cap is applied to the unquoted subject, before `shlex.quote` ( slices first, then quotes), so the quoted form can be longer than 170 characters. |
| `vivi pool dispatch --pool <pool> <task> --model <model> --from <dispatch from> --branch unit/<uid>-a<n> --budget <budget> --porcelain` |  dispatch (180 s) | Success = `rc == 0` and the string `dispatched` in the output. `no free slot` in output zeroes that pool's free count. |
| `vivi pool dispatch --pool <pool> <task> --model <gate.model> --read-only --from <tip> --budget <gate.budget> --porcelain` | `start_gate` (180 s) | Same `dispatched` check; failure logs `gate dispatch failed:` and closes the task. |
| `vivi task done <task> --for <from> --note 'dispatch failed; superseded'` |  dispatch | Fire-and-forget cleanup of a task whose pool dispatch failed. |
| `vivi task done <task> --for <from> --note 'dispatch failed'` | `start_gate` | Same, gate variant. |

### 1.2 `git` commands (cwd `<home>/repo`)

| Command | Context (timeout) | Use |
| --- | --- | --- |
| `git ls-remote <remote> refs/heads/<branch>` | `remote_has` (60 s),  `start_gate` (60 s) | `remote_has`: branch exists iff rc 0 and output non-empty. Gate: first whitespace token is the tip; must be exactly 40 chars or the gate is skipped. |
| `git merge --abort` | `reset_repo` (300 s) | Reset step 1. |
| `git fetch <remote> <branch>` | `reset_repo` (120 s) | Reset step 2. |
| `git checkout -q <branch>` | `reset_repo` (300 s) | Reset step 3. |
| `git reset -q --hard <remote>/<branch>` | `reset_repo` (300 s) | Reset step 4. The integration clone is always reset to `<remote>/<branch>` before merging. |
| `git fetch <remote> refs/heads/<b>:refs/remotes/<remote>/<b>` | ,  (180 s) | Fetch the unit branch. rc != 0 → a missing remote branch ("couldn't find remote ref") is a decided failure, `ok=false, why="branch-missing"`; any other error is undecided, `ok=null, why="fetch-failed"`, and is retried. |
| `git merge --no-ff --no-edit -m "merge <uid>: <title[:80]> (need <need>)" <remote>/<b>` | ,  (300 s) | Merge a unit branch. rc != 0 → conflict path. |
| `git merge --no-ff --no-edit -m 'merge main into <branch>' <remote>/<main>` | `batch_round` (300 s) | Merge main into the integration branch before a batch (section 5). |
| `git fetch -q <remote> <main>` | ,  `promote` (120 s) | Refresh main. |
| `git merge-base --is-ancestor <remote>/<main> <ref>` |  (HEAD),  (tip) | Is main already contained? Decides whether main must be merged in / whether promote may run. |
| `git diff --name-only --diff-filter=U` | ,  | Conflicting files, quoted into the `merge-conflict` detail. |
| `git diff --name-only HEAD~1 HEAD` | ,  | Files the merge touched; whitespace-split; each file not in `overlaps([f], write_scope)` lands in `outside`. |
| `git status --porcelain` | , | Dirty iff output non-empty. Drives the style commit and the pre-push clean check. |
| `git commit -q -am 'style: format after <ids>'` |  (one uid),  (comma-joined, 150-char cap) | The formatter style commit. |
| `git checkout -q -- .` | ,  | Discard any tree dirt before pushing. |
| `git push <remote> HEAD:refs/heads/<branch>` | ,  (180 s) | Publish the integration branch. rc != 0 → `why="push-failed"`, `ok=null`. |
| `git rev-parse HEAD` | ,  | The tip recorded on success. |
| `git push <remote> <tip>:refs/heads/<main>` | `promote` (180 s) | Fast-forward main to the gated tip. |

### 1.3 Checks and formatter (run in `<home>/repo` via `sh`)

| Command | Context (timeout) | Use |
| --- | --- | --- |
| `<merge.format>` | ,  (180 s) | Format after merging (skipped when empty); a dirty tree afterwards becomes the style commit. |
| each `merge.shared_checks` command | single and batch merges | The shared checks after every merge, before the unit's own proof. `merge.proof_timeout` seconds each. |
| the unit's `proof` commands | ,  (300 s each) | The unit's own proof (skipped for a kind with `skip_proof`). |
| `<gate.command>` | The gate worker runs it, not the pump (the gate brief) | The gate's measurement command; the pump only reads the worker's reply. |

### 1.4 Planner launches (detached, no timeout)

All three use the same command shape — `<shell_prefix>; cd <home> && <planner.run> < <prompt> > <out> 2>&1`,
where `{charter}` in `planner.run` is replaced by `planner.charter` — with the
prompt file as stdin and stdout+stderr to the `.out` file:

| Launch | Prompt / out files |
| --- | --- |
| Board planner (one per need) | `board/<need>.prompt` → `board/<need>.out` |
| Split planner (one per stuck-timed-out unit) | `resplit/<unit>.prompt` → `resplit/<unit>.out` |
| Gate-fix planner | `gatefix/<need>.prompt` → `gatefix/<need>.out` |

## 2. Plan files

### 2.1 `plans/<need>.json`

Written by a planner (board or gate-fix) in the charter format. Ingested by
`ingest`, which globs `plans/*.json` sorted, one file at a time.
Top-level keys read: `need`, `verdict` (default `"units"`), `reason`, `units`,
`revision` (a later plan for a known need), `withdraw` (pending unit ids a
revision removes), and `replaces` (split plans only). A plan for a known need
without `revision` is ignored; a revision at or below the current one is stale.
Per-unit keys: `id`, `title` (capped at 150
chars), `brief`, `write_scope` (list), `after` (list, default `[]`), `proof`
(list), `tools` (the unit's kind; absent means `default_kind`).

```json
{
 "need": "2b048b8c",
 "verdict": "delivered",
 "reason": "Already delivered by commit 176a89cb5b.",
 "units": []
}
```

A plan with units:

```json
{"need": "000f290d", "verdict": "units", "reason": "…",
 "units": [{"id": "000f290d-1", "title": "…", "brief": "…",
            "write_scope": ["src/…"], "after": [], "proof": ["./run-tests …"], "tools": "plain"}]}
```

### 2.2 Split plans (`replaces`)

Same directory, named `<unit>.split.json`. Top level adds `replaces`:

```json
{"need": "000f290d", "replaces": "000f290d-1", "verdict": "units",
 "reason": "The single unit … ran out of its 600s budget. Split into: …",
 "units": [ {"id": "000f290d-1.1", …}, {"id": "000f290d-1.2", …} ]}
```

### 2.3 Validation rules and exception texts

Validation runs per unit before anything is created:

- `id`, `brief`, `write_scope` must be present and non-empty, and `proof`
  must be a list — else `bad = "unit missing id, brief or write_scope"`.
- `id` must not already exist in `st["units"]` — else
  `bad = "duplicate unit id <uid>"`.

On `bad`: exception kind `bad-plan` with the plan file basename as the unit
field and the reason text above; if the plan has no `replaces`, the need is
recorded as `verdict="invalid"` with that reason and no units.
A plan whose JSON does not parse is skipped silently, no exception. A plan for a need already in `st["needs"]` without
`replaces` is marked seen and ignored.

### 2.4 `plans_seen`

`st["plans_seen"]` is the list of plan file basenames already ingested. On first load of an old state it is seeded from the needs. Every ingested file — including rejected ones — is
appended, so a bad plan is examined once. The parked-need re-plan path removes
`<need>.json` from it after moving the file to `plans/superseded/`.

### 2.5 Verdict values and what each does

| Verdict | Effect in the pump |
| --- | --- |
| `units` | Units are created `pending` and dispatched. |
| `delivered` | Recorded with `reason`; no units; no reply beyond the `planned` stage reply. |
| `operator`, `design`, `hold` | Recorded; these three are `PARKED`: eligible for re-plan when the operator replies later (section 8). |
| `invalid` | Set by the pump itself for a rejected non-split plan (section 2.3). |

### 2.6 `needs-ready.txt`

Appended, never rewritten, when the last unit of a need reaches a satisfied
state at merge time: one line
`<need> <tip[:10]> <unit>,<unit>,…` — e.g.

```
854ff203 b8215dc755 854ff203-1
c6ff92ae c5068a964d c6ff92ae-1,c6ff92ae-2,c6ff92ae-1.1
```

The need also enters `st["closed_needs"]`, which suppresses duplicate lines.

## 3. Collect

`collect` runs each tick. It reconciles the pool once (only when
something is in flight or a gate is running), then classifies each in-flight
unit strictly in this order:

1. **running** — leave it; but if `now - since_dispatch > budget + overdue_grace` the unit is sent to `split` with the text
   `overdue: still running long past its budget`.
2. **state `timeout`, or `detail` starting with `timeout`** — `split` with the
   receipt text.
3. **state unknown and younger than 90 s** — leave it (reconcile grace).
4. **state unknown after 90 s** — ask `vivi pool status --porcelain`; if the
   task appears on any slot (`" " + task + " "` substring), it is still
   running: leave it.
5. **branch on the remote** (`remote_has`) — the work arrived:
   when the reconcile said `finished`, read the receipt and check whether it contains `agent exit 124` or
   `agent_rc 124`; if so → `split`.
   Otherwise the unit becomes `delivered` and the log line
   `delivered <uid> <branch> secs=<n> model=<model> pool=<pool>` is written
   (secs from `since_dispatch`, `-1` when missing). NOTE: the
   `VERDICT: no-defect` marker is **not** checked on this path — a worker that
   both pushed a branch and claimed no-defect is counted `delivered`.
6. **no remote branch** — read the receipt: exit-124 patterns → `split`; the
   literal marker `VERDICT: no-defect` in the text → status `no-defect`,
   `verdict_text` = last 1500 chars, exception `no-defect` with the last 400
   chars; anything else → `fail(st, uid, "<state>/<detail>", text)`.

After the units, two more collectors:

- every unit still `splitting` after `planner.split_timeout` (default 1500 s) becomes `stuck` with exception
  `split-no-answer`;
- the gate task, when reconcile reports `finished`, `timeout`, `lost` or
  `error`, or when it is unknown for more than 2400 s, is finished via
  `finish_gate(receipt(task))`.

### 3.1 The `fail` history record

`fail` appends to the unit's `history`:
`{"attempt": <attempts>, "model": <model>, "why": <why>, "detail":
<detail truncated to the last 2500 chars>, "at": <epoch>}`, clears `task`, and
either returns the unit to `pending` or, at `attempts >= max_attempts`,
marks it `stuck` with exception `stuck` carrying `why + ": " + detail[-300:]`.
The last history record is quoted back into the next attempt's brief.

### 3.2 `failure_text` filtering

`failure_text` compresses check output for result details: it
keeps each line matching
`panicked at|^---- |^error(\[|:)|assertion|^failures:|FAILED|^test .* \.\.\. FAILED`
plus the three lines after it, dedupes in order, and returns the first 1400
chars + `\n...\n` + the last 500 chars of the full output; with no matches,
just the last 1800 chars.

## 4. Split

When a unit times out or overruns, `split`:

1. writes `resplit/<unit>.json` — `{"id", "need", "title", "brief",
   "write_scope", "proof", "tools", "worker_said": <last 3000 chars of the
   triggering text>}`;
2. writes `resplit/<unit>.prompt` with `SPLIT_PROMPT`: the
   planner is told the unit ran out of time, must write
   `plans/<unit>.split.json` (`.tmp` then `mv`) with top level
   `{"need": …, "replaces": "<unit>", "verdict": "units", "reason": …,
   "units": […]}`, unit ids `<unit>.1`, `<unit>.2`, …, two to four units each
   small enough to finish in 5 minutes, chained with `after` when they touch the same
   file, and `"units": []` with a reason when the work was already done or not
   a defect;
3. launches the split planner detached;
4. moves the unit to `status="splitting"`, clears `task`, resets `since` to
   now, appends a history record `{"attempt": …, "model": …, "why":
   "timeout", "detail": "", "at": …}`, and logs `split-requested <uid>`.

The no-answer rule: a unit still `splitting` `planner.split_timeout` seconds after the request
becomes `stuck` with exception `split-no-answer`, text
`the split planner wrote no plan in 25 minutes` (with the default timeout).

Replacement: the split plan is ingested like any other; with `replaces` set
and the old unit present, the old unit becomes `status="replaced"` with
`replaced_by` = the new ids, the need's unit list gains the new ids, and the
log says `split <uid> into <n> units`. When the split plan
carries `units: []` (the planner judged the work already done or no defect),
the old unit is still marked `replaced` with `replaced_by: []` and exception
`split-empty` is written with the plan's `reason`; a
replaced unit with no successors is satisfied for scheduling purposes
(`satisfied`).

## 5. Merge

Merging runs on a single worker thread fed by two queues (jobs in, results out). `merge_feed` moves every
`delivered` unit to `merging` and puts its job on `MERGE_IN` — under the
`HOLD-MERGE` flag only units whose id starts with `fix_prefix` are fed. The
thread (`merge_thread`) takes one job, drains up to `merge.batch` (default 8) more, then offers the batch to the remote merger (section 6)
and, if that declines, runs `batch_work` for more than one job or `merge_work`
for exactly one. Any exception in the thread yields one
`{"uid": …, "ok": null, "why": "merge-error", "detail": <error text>}` per job and
the log line `MERGE ERROR …`; every job that produced no result at all gets
`{"ok": null, "why": "merge-undecided", "detail": ""}`.

### 5.1 The single-job path — `merge_work`

`merge_work(job)`, where a job is
`{"uid", "branch", "title", "need", "proof", "tools", "write_scope"}`:

1. `reset_repo` — abort any merge, fetch, checkout and hard-reset to
   `<remote>/<branch>`.
2. fetch the unit branch → a missing remote branch gives `ok=false,
   why="branch-missing"` (detail: the git output); any other fetch error gives
   `ok=null, why="fetch-failed"` and is retried.
3. `git merge --no-ff --no-edit -m "merge <uid>: <title[:80]> (need <need>)"
   <remote>/<branch>`; on conflict: collect `git diff --name-only
   --diff-filter=U`, reset the repo, return `ok=false, why="merge-conflict"`,
   detail `The branch no longer merges into the integration branch.
   Conflicting files: <files>`.
4. compute `outside` from `git diff --name-only HEAD~1 HEAD` against
   `write_scope` via `overlaps`.
5. `merge.format` (skipped when empty); a dirty `git status --porcelain` is
   committed as `style: format after <uid>`.
6. checks: each `merge.shared_checks` command, then the unit's own `proof`
   (skipped when the unit's kind has `skip_proof`). Each with
   `merge.proof_timeout` (default 300 s) and a `proof <uid> rc=<n> <n>s: <cmd>` log line.
   A failure resets the repo and returns `ok=false, why="proof-failed"`,
   detail `After merging into the integration branch, this command failed
   (exit <n>):\n<cmd>\n\nLast output:\n<failure_text(out)>`.
7. discard any remaining dirt, `git push <remote> HEAD:refs/heads/<branch>`;
   failure → `ok=null, why="push-failed"`, detail = last 200 chars.
8. success: `ok=true`, `tip`, `outside`, `secs`.

### 5.2 The batch path — `batch_round`, `batch_work`

`batch_round(jobs)`:

1. reset; `git fetch -q <remote> <main>`; if `<remote>/<main>` is not an ancestor of
   HEAD, merge it with message `merge main into <branch>`. A conflict is aborted
   and only logged (`main does not merge into <branch> cleanly: …`) — the batch
   continues without main. NOTE: main is merged in the batch path only; the
   single-job `merge_work` path never merges main first.
2. per job: fetch + merge as in 5.1; conflicts abort that one merge and yield
   its `merge-conflict` result; cleanly merged jobs (with their `outside`)
   collect into `merged`.
3. `merge.format`; dirty → one style commit
   `style: format after <uid>,<uid>,…` (150-char cap).
4. shared checks (`merge.shared_checks`), each `merge.proof_timeout` seconds with a
   `batch <n> shared rc=<n> <n>s: <cmd>` log line. Failure with one job in
   the batch → that job gets `proof-failed` and the round ends; failure with
   several jobs → the round returns `None` for "undecided, no culprit known".
5. the units' own proofs, one job at a time (skipping kinds with `skip_proof`);
   each failing job gets `proof-failed` (detail wording `What failed:` here)
   and joins `culprits`.
6. if any culprits: the surviving jobs are returned for a second round.
7. push `HEAD:refs/heads/<branch>`; failure → every merged job gets
   `ok=null, why="push-failed"`. Success → every merged job
   `ok=true` with the same `tip`, its own `outside`, and `secs` split evenly
   (`max(1, elapsed // len(merged))`).

`batch_work(jobs)` drives the rounds: first round over all jobs;
if jobs survive it, a second round over the survivors; if the first round was
undecided (`None`), the not-yet-decided jobs are merged one at a time with
`merge_work` — the culprit-isolation fallback.

### 5.3 Result records

| `ok` | Meaning | `why` values |
| --- | --- | --- |
| `true` | merged and pushed; carries `tip`, `outside`, `secs` | — |
| `false` | decided failure; the unit is failed (retry or stuck) | `branch-missing`, `merge-conflict`, `proof-failed` |
| `null` | undecided; the unit returns to `delivered` and is retried later | `fetch-failed`, `push-failed`, `merge-error`, `merge-undecided` |

### 5.4 `merge_apply` effects

`merge_apply` drains `MERGE_OUT` on the main thread:

- `ok is null` → unit back to `delivered`; log `<why> <uid>: <detail[:200]>`.
- `ok is false` → `fail(...)` (section 3.1).
- `ok is true` → `status="merged"`, `commit=tip`, `since=now`,
  `outside` stored, `st["merges"] += 1`. A non-empty `outside` logs
  `scope-warn <uid> touched outside scope: <files>` — the out-of-scope
  detection; it never fails the merge. Then `merged <uid> tip=<10> in <n>s`,
  and when every unit of the need is now satisfied, the need closes:
  `needs-ready.txt` line (section 2.6).

## 6. The remote merger

`remote_merge(jobs)` offers a batch to a remote merger (any other machine)
when the flag file `<home>/REMOTE-MERGE` exists; without it the pump always
merges locally. The file protocol in `<home>/merge-jobs/`:

- **Job** — `<bid>.job.json`, `bid = "b" + <epoch seconds>`. Written
  atomically (`.tmp` then rename). Content: the JSON list of job objects of
  section 5.1:

```json
[
 {"uid": "8e1d2c3a-6.1", "branch": "unit/8e1d2c3a-6.1-a1",
  "title": "add helpers in test_support, sweep small test files",
  "need": "8e1d2c3a",
  "proof": ["./run-tests --filter codegen"],
  "tools": "plain",
  "write_scope": ["src/codegen/test_support.rs", "…"]}
]
```

- **Claim** — by rename: the remote merger renames (or removes)
  `<bid>.job.json`, conventionally to `<bid>.claimed.json` with the same
  content. Whoever makes the file disappear first owns the batch. Observed: `b1791981813.claimed.json`.
- **Result** — `<bid>.result.json`, written by the remote merger: a JSON list
  of the same result records as section 5.3:

```json
[
 {"uid": "8e1d2c3a-6.1", "ok": true, "tip": "d6aa42b199b211e643fd51075a269f6497a455ee",
  "outside": [], "secs": 9},
 {"uid": "5f6a7b8c-5.1", "ok": true, "tip": "d6aa42b199b211e643fd51075a269f6497a455ee",
  "outside": ["docs/ledger.toml"], "secs": 9}
]
```

Timing: the pump polls the job file every 3 s for `merge.remote_wait` (default 90) seconds. If the file is still there it deletes it, logs
`remote merger did not take batch <bid>; merging here`, and returns `None`
(merge locally). Once claimed, it polls the result file every 5 s up to
`merge.remote_limit` (default 1800) seconds; no result in time logs
`remote merger timed out on batch <bid>; merging here` and also returns
`None`. A result file is loaded and logged as
`remote batch <bid>: <n> results in <n>s`.

What the remote merger is expected to do per job: exactly the algorithm of
section 5 — reset to `<remote>/<branch>`, merge main in when batching, fetch and
merge the unit branches, the formatter with a style commit, the shared
checks and the units' own proofs, push `HEAD:refs/heads/<branch>`,
and answer with the same record shapes, `ok` true / false / null with the same
`why` vocabulary.

## 7. The gate

### 7.1 Due rules and dispatch

`start_gate` does nothing while a gate task is open. A gate is
due when the `FORCE-GATE` flag exists, or there is no baseline yet, or
`merges - last_gate.merges >= gate.every_merges`, or merges have advanced
and `now - last_gate.at > gate.every_seconds`. Pool choice: the first pool of
`gate.pools` with a free slot, else no gate this tick. The tip comes
from `git ls-remote <remote> refs/heads/<branch>` and must be 40 hex chars. The
gate task is sent as `task send --from <from> --to <to>` with subject
`[pump gate] <gate.command> at <tip10>` and the gate brief: read-only
measurement, run exactly `gate.command`, and reply
with every failing test between `FAILING-BEGIN` and `FAILING-END`, one
fully-qualified name per line, or the single line `BUILD-FAILED` when the
build itself failed. Dispatch is
`pool dispatch --pool <pool> <task> --model <gate.model> --read-only
--from <tip> --budget <gate.budget> --porcelain`. On success `last_gate` becomes
`{at, merges, task, tip, pool}` and the `FORCE-GATE` flag is removed; the log
line is `gate started at <tip10> on <pool> task=<task>`.

When `gate.results_dir` is set, an external result may stand in for the pool
gate (see `pump-contract.md` §9).

### 7.2 Reading the reply

`finish_gate(text)` parses the receipt:

- marker blocks `FAILING-BEGIN\s*\n(.*?)\n?\s*FAILING-END` (dotall); the last
  block whose lines are non-empty and do not start with `<` supplies the
  names (template examples in the brief are skipped);
- no markers and no `BUILD-FAILED` in the text → the fallback matcher
  `\b([a-z][a-z0-9_-]+) ((?:[A-Za-z0-9_]+::)+[A-Za-z0-9_]+)\b`, sorted and
  deduped, with the log `gate markers missing; read <n> names from the reply
  text`;
- `names is None` → exception `gate-unreadable`; `names == ["BUILD-FAILED"]`
  → exception `gate-build-failed`. Both append the gate record and stop.

### 7.3 Baseline, ignore file, owned, new failures

The first readable gate sets `st["baseline"] = names`. Later gates:

- Names listed in the optional `[pump.gate] ignore_file` are ignored.
- `st["red"]` (the owned map, failing name → owner need id) drops names that
  no longer fail, logging `red fixed: <name> (owner <need>)` — fixed-name
  removal.
- **new** = `sorted(set(names) - set(baseline) - ignored - set(owned))`.
- `red-after-fix`: an owned name that fails again while all of its owner's
  units are settled (`merged`, `stuck`, `no-defect`, `replaced`) raises one
  exception `red-after-fix` per name, once (`st["red_reported"]`).

When `new` is non-empty, each new name is owned by
`<fix_prefix>-g<tip8>`; suspects are the merged-status units in
`st["order"][prev_merges:this_gate.merges]`; the gate-fix planner is started;
and exception `gate-new-failures` is written with
`<n> new: <names ; joined, 350-char cap> ; suspects: <uids, 200-char cap>`.
A red gate never stops merging (ruling 1).

### 7.4 The gate-fix planner

`gatefix(tip, clean, new)` uses need id `<fix_prefix>-g<tip8>`: if
`plans/<need>.json` already exists it does nothing. Otherwise it writes
`gatefix/<need>.prompt`: the failing
names, the clean commit, `git log --oneline <clean>..<tip>` as the suspect
list, the plan format `{"need": …, "verdict": "units", …}` with unit ids
`<need>-1`, `<need>-2`, …, one likely cause per unit, proofs naming the
failing tests with one filter per command — and launches the
planner detached. `clean` is the tip of the last gate with names and no new
failures.

### 7.5 Promote

`promote(tip)` fast-forwards main only: after
`git fetch -q <remote> <main>`, if `<remote>/<main>` is not an ancestor of the
tip it logs `promote skipped: main has commits <branch> does not have yet` and
returns; otherwise `git push <remote> <tip>:refs/heads/<main>` and the log
`promote main -> <tip10> rc=<n> …`. Called exactly when a readable gate has
no new unowned failure.

### 7.6 The hold cycle

`hold_cycle`, while `HOLD-MERGE` exists: once every fix
unit has settled, a clean gate newer than the flag file lifts the hold
(`hold lifted: gate at <tip10> is clean`); otherwise, when fix units merged
after the last gate or the last gate was unreadable, it writes `FORCE-GATE`
containing `after fixes\n` and logs `hold: fixes merged, forcing a gate`.

### 7.7 Gate timeouts

The gate worker gets `--budget <gate.budget>`. On the pump side, collect
finishes the gate when reconcile reports the task `finished`, `timeout`,
`lost` or `error`, or when the state is unknown for more than 2400 s after
dispatch — the receipt is read and classified whatever it says.

## 8. The board

`board(st)` is throttled to one pass per `board.every` seconds
(`b["at"]`). A pass, in order:

1. **sync pull** — `vivi sync pull` (120 s) when `board.pull`; rc and the last
   output line are logged.
2. **candidate rows** — `vivi need list --from <sender> --status open --json`
   for each sender in `board.from` (keys `handle`, `date`, `subject`), then
   `marked_rows`: the two forms
   `need list --status open --json --held` and
   `need list --status open --json --to mind`, keeping rows whose `marks`
   list contains `board.mark`; marked rows not already present are appended. A
   marked open need is plannable whatever its sender or date (ruling 4).
3. **skip list** — whitespace-split handles in `<home>/skip.txt`.
4. **since-cutoff** — a row with `date < board.since` (string compare) is
   skipped **unless** it is in this pass's `replan` set or it is a marked
   (approved) need. Also skipped: needs already in `st["needs"]`, needs
   already being planned.
5. **planners cap** — at most `board.planners_max` concurrent planners; a need
   whose plan file already exists is not re-planned. Timed-out planners: a
   handle with no plan file after `board.plan_timeout` seconds leaves
   `planning` with exception `plan-no-answer`, text
   `the planner wrote no plan in 30 minutes` (with the default timeout).
6. **planner launch and its files** — `vivi need show <h>` (60 s) is stored
   as `needs/<h>.json` = `{"need": h, "subject": …, "triage": "",
   "thread": <first 40000 chars>}`; `board/<h>.prompt` gets: *Plan one need, as
   your system prompt describes. Its thread is in `<home>`/needs/<h>.json.
   Write `<home>`/plans/<h>.json. Your final message is one line: handle,
   verdict, number of units.* The detached planner launch of section 1.4
   follows, and the log line is `board: planning new need <h> <subject[:70]>`.
7. **replies** — for every candidate handle with a need record, at stage
   `planned` (always, once) and stage `merged` (when every unit is
   satisfied), sent as `vivi mail reply <h> --from <reply_from> --to
   <reply_to> --body-file <home>/board/reply-<h>-<stage>.txt` (60 s). Bodies,
   exactly:

   ```
   pump: planned. verdict: <verdict>; <n> unit(s).
   <reason>
   - <uid> <title>
   ```

   ```
   pump: every unit of this need is merged into branch <branch>.
   - <uid> <status> <commit[:10]>
   Not reviewed. Not on main.
   ```

   Each stage is replied once (`b["replied"]` keys `<h>:planned`,
   `<h>:merged`); the log line is
   `board reply <h> <stage> rc=<n> …`.
8. **amendments** — for each planned need, compare the newest open-need event
   from one `board --json` with `thread_seen_at`; only when it moved, fetch
   `need show <h> --json` and hash the thread. A different hash sets
   `amendment_pending` and spawns the revision planner with
   `plans/<need>.r<N>.json` as its output. `revision_planning` records the
   start; a plan file or `plan_timeout` expires the entry, and a timeout
   files `revision-no-answer`. The planner is not spawned when
   `[pump.planner] run` is empty.
9. **push** — when any reply succeeded, a detached
   `vivi sync push --since <board.push_since> --drain --project <proj>`
   appending to `<home>/sync-push.log`, logged as
   `sync push started (detached, drain)`.

### 8.1 Parked-need re-plan on an operator reply

`replan_answered`: a parked need (verdict `operator`, `design` or
`hold`, no units) whose plan file exists and whose thread carries an operator
message newer than the plan file's mtime (`operator_replied_after`) is re-planned: the plan moves to
`plans/superseded/<h>.<epoch>.json`, the basename leaves `plans_seen`, the
need record is deleted, any `planned`-stage reply key is dropped, the handle
joins `b["replan"]` (which bypasses the since-cutoff for this pass), and the
log says `board: operator replied on parked need <h> (was <verdict>);
re-planning` (ruling 5).

## 9. Exceptions and log lines

### 9.1 Exception kinds

`exception` appends `HH:MM:SS kind unit text` to
`exceptions.txt` (UTC), newlines in the text become ` | `, the text is capped
at 600 characters, and a log line `EXCEPTION <kind> <unit>` is written.

| Kind | Trigger | Text written |
| --- | --- | --- |
| `bad-plan` | a plan file fails validation | `unit missing id, brief or write_scope` or `duplicate unit id <uid>`; unit field is the plan file basename |
| `unknown-kind` | a pending unit names a kind (its `tools` value) that is not defined, or has none and no `default_kind` is set; raised once per unit | the kind name and that the unit is not dispatched |
| `split-empty` | a split plan with `units: []` replaces a unit | the plan's `reason` (e.g. "Already delivered: commit …") |
| `stuck` | `attempts >= max_attempts` on a fail | `<why>: <detail[-300:]>` |
| `no-defect` | receipt contains `VERDICT: no-defect` | the last 400 chars of the receipt |
| `split-no-answer` | unit `splitting` longer than `planner.split_timeout` | `the split planner wrote no plan in 25 minutes` |
| `gate-unreadable` | gate reply yields no names | the last 500 chars of the gate receipt; unit field is the gated tip[:10] |
| `gate-build-failed` | gate reply is exactly `BUILD-FAILED` | the last 500 chars of the gate receipt |
| `red-after-fix` | an owned failing name fails again after its fix units settled | the failing test name; unit field is the owner need id |
| `gate-new-failures` | a readable gate has new unowned failures | `<n> new: <names> ; suspects: <uids>` (350/200-char caps); unit field is the gated tip[:10] |
| `plan-no-answer` | a board planner wrote no plan in `board.plan_timeout` seconds | `the planner wrote no plan in 30 minutes` |

### 9.2 Log line shapes a script may grep

`log` writes `HH:MM:SS <msg>` (UTC). Stable message shapes:

| Prefix / shape |
| --- |
| `dispatch <uid> a<n> <pool> <model> task=<task>` |
| `task send failed <uid>: <out[-200:]>` / `dispatch failed <uid>: <out[-300:]>` |
| `delivered <uid> <branch> secs=<n> model=<model> pool=<pool>` |
| `fail <uid> attempt=<n> <why>` |
| `split-requested <uid>` / `split <uid> into <n> units` |
| `plan <need> verdict=<v> units=<n>` |
| `proof <uid> rc=<n> <n>s: <cmd[:90]>` |
| `batch <n> shared rc=<n> <n>s: <cmd>` / `batch of <n>: <n> merged` |
| `merged <uid> tip=<tip[:10]> in <n>s` |
| `scope-warn <uid> touched outside scope: <files>` |
| `<why> <uid>: <detail[:200]>` (undecided merge results) |
| `main does not merge into <branch> cleanly: …` |
| `gate started at <tip10> on <pool> task=<task>` / `gate dispatch failed: …` |
| `gate markers missing; read <n> names from the reply text` |
| `gate baseline at <tip10>: <n> failing names` / `gate at <tip10>: <n> failing, <n> new` |
| `gatefix planner started for <need> (<n> new)` |
| `promote main -> <tip10> rc=<n> …` / `promote skipped: main has commits <branch> does not have yet` |
| `red fixed: <name> (owner <need>)` |
| `hold lifted: gate at <tip10> is clean` / `hold: fixes merged, forcing a gate` |
| `remote merger did not take batch <bid>; merging here` / `remote merger timed out on batch <bid>; merging here` / `remote batch <bid>: <n> results in <n>s` |
| `board: planning new need <h> <subject[:70]>` |
| `board reply <h> <stage> rc=<n> …` |
| `board: operator replied on parked need <h> (was <v>); re-planning` |
| `sync pull rc=<n> …` / `sync push started (detached, drain)` |
| `EXCEPTION <kind> <unit>` |
