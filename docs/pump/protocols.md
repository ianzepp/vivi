# Pump protocols: the wire contract (part two)

This is part two of the pump's contract, written from the living Python pump
(`/mnt/agents/pump/pump.py`, read-only). It documents every external command
line and file protocol the pump speaks, exactly as the code behaves today.
Part one — surface, home layout, flag files, state fields, the unit state
machine, the tick, scheduling, and the config table — is
[`contract.md`](contract.md); this file does not repeat it.

Line references are `pump.py:<line> <name>` against the 1082-line file of
2026-10-14. Constants named here (`BUDGET`, `TICK`, …) are the same constants
the config table in `contract.md` maps onto.

Every command line runs through one funnel:

- `sh(cmd, timeout=120, cwd=None)` — `pump.py:88 sh`: `bash -c ". " +
  "/mnt/agents/faber-runs/pharos-env.sh" + "; " + cmd`, combined
  stdout+stderr, optional cwd. On timeout it returns `rc=124` with the partial
  output plus the line `[timeout after <n>s]`.
- `vivi(args, timeout=120)` — `pump.py:101 vivi`: `sh("vivi " + args +
  " --project /home/ianzepp/work/faberlang")`.
- `git(cmd, timeout=300)` — `pump.py:621 git`: `sh("git " + cmd, cwd=<home>/repo)`.
- Planner launches are detached `subprocess.Popen(["bash","-c",cmd],
  start_new_session=True)` with no timeout; the pump watches their output
  files instead (`pump.py:355`, `pump.py:436`, `pump.py:927`).

## 1. Commands the pump runs

### 1.1 `vivi` commands

| Command line | Where (timeout) | Output parsed, and how |
| --- | --- | --- |
| `vivi sync pull --project <proj>` | `pump.py:325` board (120 s) | Nothing machine-read; last output line logged as `sync pull rc=<n> …`. |
| `vivi need list --from operator@bd371d96.local --status open --json` | `pump.py:327` board (60 s) | JSON from the first `[` in the output (`out[out.index("["):]`); rows read for keys `handle`, `date`, `subject`. Unparseable → `board list unreadable:` log, no rows. |
| `vivi need list --status open --json --held` and `vivi need list --status open --json --to mind` | `pump.py:307` `marked_rows` (60 s) | Same JSON-from-`[` slice; rows kept only when the `marks` list contains the approval mark `pump` (`APPROVAL_MARK`, `pump.py:301`). Deduped by `handle`. Each failure logs `marked list unreadable (…)`. |
| `vivi need show <handle>` | `pump.py:258` `operator_replied_after`, `pump.py:349` board (60 s) | Raw thread text. Board stores `thread[:40000]` into `needs/<h>.json`. `operator_replied_after` scans line by line: a header `re.match(r"## (\S+) - [0-9a-f]+ \[")` sets the current timestamp; a later `From: operator@` line whose header timestamp parses as ISO and is newer than `since` (epoch) returns true (`pump.py:263-270`). |
| `vivi mail reply <h> --from mind --to operator@bd371d96.local --body-file /tmp/pump-reply.txt` | `pump.py:382` board (60 s) | `rc == 0` marks the stage replied. |
| `vivi sync push --since 30m --drain --project <proj>` | `pump.py:388` board (detached) | Never waited on; output appended to `<home>/sync-push.log`. |
| `vivi pool reconcile --porcelain` | `pump.py:449` collect (300 s) | Lines `reconciled|collected <task> <pool> <slot> <state> <detail>`. Split with `shlex.split`; a line counts when it has ≥ 6 fields and field 0 is `reconciled` or `collected`; fields 1, 4, 5 are taken: `seen[task] = (state, detail)` (`pump.py:454-458`). |
| `vivi pool status --porcelain` | `pump.py:507` `slots`, `pump.py:472` collect (120 s) | Slot lines `slot <pool> <name> <id> <state> …` split on whitespace: `p[0]=="slot"` and `p[4]=="free"` increments the free count for pool `p[1]` (`pump.py:510-513`). In collect, a plain substring test `" " + task + " "` asks "is this task still on some slot". |
| `vivi task show <task> --latest` | `pump.py:401` `receipt` (60 s) | The receipt: last 6000 characters of combined output. The pump decides nothing from it except the literal marker `VERDICT: no-defect` and the exit-124 patterns (section 3). |
| `vivi task send --from operator --to hand --subject <subj> --body-file /tmp/pump-brief-<uid>.txt` (units) or `/tmp/pump-gate.txt` (gate) | `pump.py:598` dispatch (120 s default), `pump.py:891` `start_gate` (120 s default) | Created-handle regex `re.search(r"^(?:node\|created)\s+(?:\S+\s+)?([0-9a-f]{8})\s*$", out, re.M)` (`pump.py:600`, `pump.py:892`) — the first 8-hex handle on its own line. No match → dispatch aborts for that unit (`task send failed …` log). Unit subject is `"[<uid> a<n>] <title>"`; gate subject is `"[pump gate] stage 4b at <tip10>"`. NOTE: the 170-character cap is applied to the unquoted subject, before `shlex.quote` (`pump.py:598` slices first, then quotes), so the quoted form can be longer than 170 characters. |
| `vivi pool dispatch --pool <pool> <task> --model <model> --from next --branch unit/<uid>-a<n> --budget 600 --porcelain` | `pump.py:605` dispatch (180 s) | Success = `rc == 0` and the string `dispatched` in the output. `no free slot` in output zeroes that pool's free count. |
| `vivi pool dispatch --pool <pool> <task> --model zai/glm-5.3-flash --read-only --from <tip> --budget 1800 --porcelain` | `pump.py:896` `start_gate` (180 s) | Same `dispatched` check; failure logs `gate dispatch failed:` and closes the task. |
| `vivi task done <task> --for operator --note 'dispatch failed; superseded'` | `pump.py:609` dispatch | Fire-and-forget cleanup of a task whose pool dispatch failed. |
| `vivi task done <task> --for operator --note 'dispatch failed'` | `pump.py:899` `start_gate` | Same, gate variant. |

### 1.2 `git` commands (cwd `<home>/repo`)

| Command | Where (timeout) | Use |
| --- | --- | --- |
| `git ls-remote origin refs/heads/<branch>` | `pump.py:396` `remote_has` (60 s), `pump.py:885` `start_gate` (60 s) | `remote_has`: branch exists iff rc 0 and output non-empty. Gate: first whitespace token is the tip; must be exactly 40 chars or the gate is skipped. |
| `git merge --abort` | `pump.py:626` `reset_repo` (300 s) | Reset step 1. |
| `git fetch origin next` | `pump.py:627` `reset_repo` (120 s) | Reset step 2. |
| `git checkout -q next` | `pump.py:628` `reset_repo` (300 s) | Reset step 3. |
| `git reset -q --hard origin/next` | `pump.py:629` `reset_repo` (300 s) | Reset step 4. The integration clone is always reset to `origin/next` before merging. |
| `git fetch origin refs/heads/<b>:refs/remotes/origin/<b>` | `pump.py:636`, `pump.py:691` (180 s) | Fetch the unit branch. rc != 0 → result `why="fetch-failed"`. |
| `git merge --no-ff --no-edit -m "merge <uid>: <title[:80]> (need <need>)" origin/<b>` | `pump.py:640`, `pump.py:696` (300 s) | Merge a unit branch. rc != 0 → conflict path. |
| `git merge --no-ff --no-edit -m 'merge main into next' origin/main` | `pump.py:683` `batch_round` (300 s) | Merge main into the integration branch before a batch (section 5). |
| `git fetch -q origin main` | `pump.py:680`, `pump.py:951` `promote` (120 s) | Refresh main. |
| `git merge-base --is-ancestor origin/main <ref>` | `pump.py:681` (HEAD), `pump.py:952` (tip) | Is main already contained? Decides whether main must be merged in / whether promote may run. |
| `git diff --name-only --diff-filter=U` | `pump.py:642`, `pump.py:698` | Conflicting files, quoted into the `merge-conflict` detail. |
| `git diff --name-only HEAD~1 HEAD` | `pump.py:646`, `pump.py:703` | Files the merge touched; whitespace-split; each file not in `overlaps([f], write_scope)` lands in `outside`. |
| `git status --porcelain` | `pump.py:649`, `pump.py:660`, `pump.py:709`, `pump.py:737` | Dirty iff output non-empty. Drives the style commit and the pre-push clean check. |
| `git commit -q -am 'style: cargo fmt after <ids>'` | `pump.py:651` (one uid), `pump.py:711` (comma-joined, 150-char cap) | The `cargo fmt` style commit. |
| `git checkout -q -- .` | `pump.py:662`, `pump.py:739` | Discard any tree dirt before pushing. |
| `git push origin HEAD:refs/heads/next` | `pump.py:663`, `pump.py:740` (180 s) | Publish the integration branch. rc != 0 → `why="push-failed"`, `ok=null`. |
| `git rev-parse HEAD` | `pump.py:667`, `pump.py:744` | The tip recorded on success. |
| `git push origin <tip>:refs/heads/main` | `pump.py:956` `promote` (180 s) | Fast-forward main to the gated tip. |

### 1.3 Checks and formatter (run in `<home>/repo` via `sh`)

| Command | Where (timeout) | Use |
| --- | --- | --- |
| `cargo fmt --all` | `pump.py:648`, `pump.py:708` (180 s) | Format after merging; a dirty tree afterwards becomes the style commit. |
| `./scripta/test --stage 1` and `./scripta/test --stage 2` | `SHARED` at `pump.py:672`; run at `pump.py:652` (single) and `pump.py:714-715` (batch) | The shared checks after every merge, before the unit's own proof. 300 s each (`PROOF_TIMEOUT`). |
| the unit's `proof` commands | `pump.py:655`, `pump.py:729` (300 s each) | The unit's own proof (skipped for `tools == "full"` units; the worker proved them with its own toolchain). |
| `cargo test -q -p hygiene-ratchet --test hygiene` | `pump.py:713` | Only when `<home>/hygiene.on` exists; appended to the shared checks for that batch. |
| `./scripta/test --stage 4b > /tmp/stage4b.log 2>&1; echo "rc=$?"` | The gate worker runs it, not the pump (`GATE_BRIEF`, `pump.py:64`) | The gate's measurement command; the pump only reads the worker's reply. |

### 1.4 Planner launches (detached, no timeout)

All three use the same command shape — `. <env>; cd <home> && claude -p
--model claude-sonnet-5-5 --effort high --permission-mode bypassPermissions
--append-system-prompt-file <home>/planner-charter.md < <prompt> > <out> 2>&1`
— with the prompt file as stdin and stdout+stderr to the `.out` file:

| Launch | Prompt / out files | Where |
| --- | --- | --- |
| Board planner (one per need) | `board/<need>.prompt` → `board/<need>.out` | `pump.py:353-355` |
| Split planner (one per stuck-timed-out unit) | `resplit/<unit>.prompt` → `resplit/<unit>.out` | `pump.py:434-436` |
| Gate-fix planner | `gatefix/<need>.prompt` → `gatefix/<need>.out` | `pump.py:925-927` |

## 2. Plan files

### 2.1 `plans/<need>.json`

Written by a planner (board or gate-fix) in the charter format. Ingested by
`pump.py:191 ingest`, which globs `plans/*.json` sorted, one file at a time.
Top-level keys read: `need`, `verdict` (default `"units"`), `reason`, `units`,
`replaces` (split plans only). Per-unit keys: `id`, `title` (capped at 150
chars), `brief`, `write_scope` (list), `after` (list, default `[]`), `proof`
(list; passed through `fix_proof`), `tools` (default `"plain"`).

```json
{
 "need": "2b048b8c",
 "verdict": "delivered",
 "reason": "176a89cb5b borrows the bounded-ascii window source … with its emit test.",
 "units": []
}
```

A plan with units:

```json
{"need": "000f290d", "verdict": "units", "reason": "…",
 "units": [{"id": "000f290d-1", "title": "…", "brief": "…",
            "write_scope": ["crates/…"], "after": [], "proof": ["cargo test -p …"], "tools": "plain"}]}
```

### 2.2 Split plans (`replaces`)

Same directory, named `<unit>.split.json`. Top level adds `replaces`:

```json
{"need": "000f290d", "replaces": "000f290d-1", "verdict": "units",
 "reason": "The single unit … ran out of its 600s budget. Split into: …",
 "units": [ {"id": "000f290d-1.1", …}, {"id": "000f290d-1.2", …} ]}
```

### 2.3 Validation rules and exception texts

Validation runs per unit before anything is created (`pump.py:212-218`):

- `id`, `brief`, `write_scope` must be present and non-empty, and `proof`
  must be a list — else `bad = "unit missing id, brief or write_scope"`.
- `id` must not already exist in `st["units"]` — else
  `bad = "duplicate unit id <uid>"`.

On `bad`: exception kind `bad-plan` with the plan file basename as the unit
field and the reason text above; if the plan has no `replaces`, the need is
recorded as `verdict="invalid"` with that reason and no units (`pump.py:220-224`).
A plan whose JSON does not parse is skipped silently, no exception
(`pump.py:194-196`). A plan for a need already in `st["needs"]` without
`replaces` is marked seen and ignored (`pump.py:201-203`).

### 2.4 `plans_seen`

`st["plans_seen"]` is the list of plan file basenames already ingested
(`pump.py:203-206`). On first load of an old state it is seeded from the needs
(`pump.py:109-110`). Every ingested file — including rejected ones — is
appended, so a bad plan is examined once. The parked-need re-plan path removes
`<need>.json` from it after moving the file to `plans/superseded/`
(`pump.py:290`).

### 2.5 Verdict values and what each does

| Verdict | Effect in the pump |
| --- | --- |
| `units` | Units are created `pending` and dispatched (`pump.py:225-231`). |
| `delivered` | Recorded with `reason`; no units; no reply beyond the `planned` stage reply. |
| `operator`, `design`, `hold` | Recorded; these three are `PARKED` (`pump.py:253`): eligible for re-plan when the operator replies later (section 8). |
| `invalid` | Set by the pump itself for a rejected non-split plan (section 2.3). |

### 2.6 `needs-ready.txt`

Appended, never rewritten, when the last unit of a need reaches a satisfied
state at merge time (`pump.py:850-854`): one line
`<need> <tip[:10]> <unit>,<unit>,…` — e.g.

```
854ff203 b8215dc755 854ff203-1
c6ff92ae c5068a964d c6ff92ae-1,c6ff92ae-2,c6ff92ae-1.1
```

The need also enters `st["closed_needs"]`, which suppresses duplicate lines.

## 3. Collect

`pump.py:444 collect` runs each tick. It reconciles the pool once (only when
something is in flight or a gate is running), then classifies each in-flight
unit strictly in this order (`pump.py:460-494`):

1. **running** — leave it; but if `now - u["since"] > BUDGET + 900` (600 + 900
   s) the unit is sent to `split` with the text
   `overdue: still running long past its budget`.
2. **state `timeout`, or `detail` starting with `timeout`** — `split` with the
   receipt text.
3. **state unknown and younger than 90 s** — leave it (reconcile grace).
4. **state unknown after 90 s** — ask `vivi pool status --porcelain`; if the
   task appears on any slot (`" " + task + " "` substring), it is still
   running: leave it.
5. **branch on the remote** (`remote_has`, `pump.py:395`) — the work arrived:
   when the reconcile said `finished`, read the receipt and check
   `re.search(r"agent exit 124|agent_rc 124", t2)`; exit 124 → `split`.
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

- every unit still `splitting` after 1500 s becomes `stuck` with exception
  `split-no-answer` (`pump.py:496-499`);
- the gate task, when reconcile reports `finished`, `timeout`, `lost` or
  `error`, or when it is unknown for more than 2400 s, is finished via
  `finish_gate(receipt(task))` (`pump.py:501-504`).

### 3.1 The `fail` history record

`pump.py:405 fail` appends to the unit's `history`:
`{"attempt": <attempts>, "model": <model>, "why": <why>, "detail":
<detail truncated to the last 2500 chars>, "at": <epoch>}`, clears `task`, and
either returns the unit to `pending` or, at `attempts >= MAX_ATTEMPTS` (3),
marks it `stuck` with exception `stuck` carrying `why + ": " + detail[-300:]`.
The last history record is quoted back into the next attempt's brief
(`pump.py:571-573`).

### 3.2 `failure_text` filtering

`pump.py:179 failure_text` compresses check output for result details: it
keeps each line matching
`panicked at|^---- |^error(\[|:)|assertion|^failures:|FAILED|^test .* \.\.\. FAILED`
plus the three lines after it, dedupes in order, and returns the first 1400
chars + `\n...\n` + the last 500 chars of the full output; with no matches,
just the last 1800 chars.

## 4. Split

When a unit times out or overruns, `pump.py:428 split`:

1. writes `resplit/<unit>.json` — `{"id", "need", "title", "brief",
   "write_scope", "proof", "tools", "worker_said": <last 3000 chars of the
   triggering text>}` (literal example keys observed in
   `resplit/000f290d-1.json`);
2. writes `resplit/<unit>.prompt` with `SPLIT_PROMPT` (`pump.py:417`): the
   planner is told the unit ran out of time, must write
   `plans/<unit>.split.json` (`.tmp` then `mv`) with top level
   `{"need": …, "replaces": "<unit>", "verdict": "units", "reason": …,
   "units": […]}`, unit ids `<unit>.1`, `<unit>.2`, …, two to four units each
   finishable in 5 minutes, chained with `after` when they touch the same
   file, and `"units": []` with a reason when the work was already done or not
   a defect;
3. launches the split planner detached (`pump.py:434-436`);
4. moves the unit to `status="splitting"`, clears `task`, resets `since` to
   now, appends a history record `{"attempt": …, "model": …, "why":
   "timeout", "detail": "", "at": …}`, and logs `split-requested <uid>`.

The 1500 s no-answer rule: a unit still `splitting` 1500 s after the request
becomes `stuck` with exception `split-no-answer`, text
`the split planner wrote no plan in 25 minutes` (`pump.py:496-499`).

Replacement: the split plan is ingested like any other; with `replaces` set
and the old unit present, the old unit becomes `status="replaced"` with
`replaced_by` = the new ids, the need's unit list gains the new ids, and the
log says `split <uid> into <n> units` (`pump.py:226-230`). When the split plan
carries `units: []` (the planner judged the work already done or no defect),
the old unit is still marked `replaced` with `replaced_by: []` and exception
`split-empty` is written with the plan's `reason` (`pump.py:231-232`); a
replaced unit with no successors is satisfied for scheduling purposes
(`satisfied`, `pump.py:516-525`).

## 5. Merge

Merging runs on a single worker thread fed by two queues, `MERGE_IN` and
`MERGE_OUT` (`pump.py:805-806`). `merge_feed` (`pump.py:858`) moves every
`delivered` unit to `merging` and puts its job on `MERGE_IN` — under the
`HOLD-MERGE` flag only units whose id starts with `pumpfix` are fed. The
thread (`pump.py:803 merge_thread`) takes one job, drains up to `BATCH = 8`
(`pump.py:671`) more, then offers the batch to the remote merger (section 6)
and, if that declines, runs `batch_work` for more than one job or `merge_work`
for exactly one. Any exception in the thread yields one
`{"uid": …, "ok": null, "why": "merge-error", "detail": repr(e)}` per job and
the log line `MERGE ERROR …`; every job that produced no result at all gets
`{"ok": null, "why": "merge-undecided", "detail": ""}`.

### 5.1 The single-job path — `merge_work`

`pump.py:632 merge_work(job)`, where a job is
`{"uid", "branch", "title", "need", "proof", "tools", "write_scope"}`:

1. `reset_repo` — abort any merge, fetch, checkout and hard-reset to
   `origin/next` (`pump.py:625-629`).
2. fetch the unit branch → failure gives `ok=false, why="fetch-failed"`.
3. `git merge --no-ff --no-edit -m "merge <uid>: <title[:80]> (need <need>)"
   origin/<branch>`; on conflict: collect `git diff --name-only
   --diff-filter=U`, reset the repo, return `ok=false, why="merge-conflict"`,
   detail `The branch no longer merges into the integration branch.
   Conflicting files: <files>`.
4. compute `outside` from `git diff --name-only HEAD~1 HEAD` against
   `write_scope` via `overlaps` (`pump.py:143`).
5. `cargo fmt --all`; a dirty `git status --porcelain` is committed as
   `style: cargo fmt after <uid>`.
6. checks: `./scripta/test --stage 1`, `./scripta/test --stage 2`, then the
   unit's own `proof` (skipped when `tools == "full"`). Each with
   `PROOF_TIMEOUT` = 300 s and a `proof <uid> rc=<n> <n>s: <cmd>` log line.
   A failure resets the repo and returns `ok=false, why="proof-failed"`,
   detail `After merging into the integration branch, this command failed
   (exit <n>):\n<cmd>\n\nLast output:\n<failure_text(out)>`.
7. discard any remaining dirt, `git push origin HEAD:refs/heads/next`;
   failure → `ok=null, why="push-failed"`, detail = last 200 chars.
8. success: `ok=true`, `tip`, `outside`, `secs`.

### 5.2 The batch path — `batch_round`, `batch_work`

`pump.py:675 batch_round(jobs)`:

1. reset; `git fetch -q origin main`; if `origin/main` is not an ancestor of
   HEAD, merge it with message `merge main into next`. A conflict is aborted
   and only logged (`main does not merge into next cleanly: …`) — the batch
   continues without main. NOTE: main is merged in the batch path only; the
   single-job `merge_work` path never merges main first.
2. per job: fetch + merge as in 5.1; conflicts abort that one merge and yield
   its `merge-conflict` result; cleanly merged jobs (with their `outside`)
   collect into `merged`.
3. `cargo fmt --all`; dirty → one style commit
   `style: cargo fmt after <uid>,<uid>,…` (150-char cap).
4. shared checks (`SHARED`, `pump.py:672`: stage 1, stage 2, plus the hygiene
   test when `<home>/hygiene.on` exists), each 300 s with a
   `batch <n> shared rc=<n> <n>s: <cmd>` log line. Failure with one job in
   the batch → that job gets `proof-failed` and the round ends; failure with
   several jobs → the round returns `None` for "undecided, no culprit known".
5. the units' own proofs, one job at a time (skipping `tools == "full"`);
   each failing job gets `proof-failed` (detail wording `What failed:` here)
   and joins `culprits`.
6. if any culprits: the surviving jobs are returned for a second round.
7. push `HEAD:refs/heads/next`; failure → every merged job gets
   `ok=null, why="push-failed"`. Success → every merged job
   `ok=true` with the same `tip`, its own `outside`, and `secs` split evenly
   (`max(1, elapsed // len(merged))`).

`pump.py:749 batch_work(jobs)` drives the rounds: first round over all jobs;
if jobs survive it, a second round over the survivors; if the first round was
undecided (`None`), the not-yet-decided jobs are merged one at a time with
`merge_work` — the culprit-isolation fallback.

### 5.3 Result records

| `ok` | Meaning | `why` values |
| --- | --- | --- |
| `true` | merged and pushed; carries `tip`, `outside`, `secs` | — |
| `false` | decided failure; the unit is failed (retry or stuck) | `fetch-failed`, `merge-conflict`, `proof-failed` |
| `null` | undecided; the unit returns to `delivered` and is retried later | `push-failed`, `merge-error`, `merge-undecided` |

### 5.4 `merge_apply` effects

`pump.py:830 merge_apply` drains `MERGE_OUT` on the main thread:

- `ok is null` → unit back to `delivered`; log `<why> <uid>: <detail[:200]>`.
- `ok is false` → `fail(...)` (section 3.1).
- `ok is true` → `status="merged"`, `commit=tip`, `since=now()`,
  `outside` stored, `st["merges"] += 1`. A non-empty `outside` logs
  `scope-warn <uid> touched outside scope: <files>` — the out-of-scope
  detection; it never fails the merge. Then `merged <uid> tip=<10> in <n>s`,
  and when every unit of the need is now satisfied, the need closes:
  `needs-ready.txt` line (section 2.6).

## 6. The remote merger

`pump.py:773 remote_merge(jobs)` offers a batch to a remote merger (a laptop)
when the flag file `<home>/REMOTE-MERGE` exists; without it the pump always
merges locally. The file protocol in `<home>/merge-jobs/`:

- **Job** — `<bid>.job.json`, `bid = "b" + <epoch seconds>`. Written
  atomically (`.tmp` then rename). Content: the JSON list of job objects of
  section 5.1:

```json
[
 {"uid": "84e6122f-6.1", "branch": "unit/84e6122f-6.1-a1",
  "title": "codegen/rust: add _la helpers in test_support, sweep small test files",
  "need": "84e6122f",
  "proof": ["RADIX_DEFAULT_CODE_LOCALE=en cargo test -p radix-module --lib -- codegen::rust::…"],
  "tools": "plain",
  "write_scope": ["crates/radix-module/src/codegen/rust/test_support.rs", "…"]}
]
```

- **Claim** — by rename: the remote merger renames (or removes)
  `<bid>.job.json`, conventionally to `<bid>.claimed.json` with the same
  content. Whoever makes the file disappear first owns the batch
  (`pump.py:785-787`). Observed: `b1791981813.claimed.json`.
- **Result** — `<bid>.result.json`, written by the remote merger: a JSON list
  of the same result records as section 5.3:

```json
[
 {"uid": "84e6122f-6.1", "ok": true, "tip": "d6aa42b199b211e643fd51075a269f6497a455ee",
  "outside": [], "secs": 9},
 {"uid": "ee7b87ee-5.1", "ok": true, "tip": "d6aa42b199b211e643fd51075a269f6497a455ee",
  "outside": ["docs/factory/corpus-e2e-coverage-gate/coverage-ledger.toml"], "secs": 9}
]
```

Timing: the pump polls the job file every 3 s for `REMOTE_WAIT = 90` s
(`pump.py:769`). If the file is still there it deletes it, logs
`remote merger did not take batch <bid>; merging here`, and returns `None`
(merge locally). Once claimed, it polls the result file every 5 s up to
`REMOTE_LIMIT = 1800` s (`pump.py:770`); no result in time logs
`remote merger timed out on batch <bid>; merging here` and also returns
`None`. A result file is loaded and logged as
`remote batch <bid>: <n> results in <n>s`.

What the remote merger is expected to do per job: exactly the algorithm of
section 5 — reset to `origin/next`, merge main in when batching, fetch and
merge the unit branches, `cargo fmt --all` with a style commit, the shared
checks and the units' own proofs (300 s each), push `HEAD:refs/heads/next`,
and answer with the same record shapes, `ok` true / false / null with the same
`why` vocabulary.

## 7. The gate

### 7.1 Due rules and dispatch

`pump.py:872 start_gate` does nothing while a gate task is open. A gate is
due when the `FORCE-GATE` flag exists, or there is no baseline yet, or
`merges - last_gate.merges >= GATE_EVERY_MERGES` (8), or merges have advanced
and `now - last_gate.at > GATE_EVERY_SECONDS` (1800 s). Pool choice: `flyfull`
if it has a free slot, else `flygate`, else no gate this tick. The tip comes
from `git ls-remote origin refs/heads/next` and must be 40 hex chars. The
gate task is sent as `task send --from operator --to hand` with subject
`[pump gate] stage 4b at <tip10>` and body `GATE_BRIEF` (`pump.py:60`):
read-only measurement, run exactly
`./scripta/test --stage 4b > /tmp/stage4b.log 2>&1; echo "rc=$?"`, and reply
with every failing test between `FAILING-BEGIN` and `FAILING-END`, one
fully-qualified name per line, or the single line `BUILD-FAILED` when the
build itself failed. Dispatch is
`pool dispatch --pool <pool> <task> --model zai/glm-5.3-flash --read-only
--from <tip> --budget 1800 --porcelain`. On success `last_gate` becomes
`{at, merges, task, tip, pool}` and the `FORCE-GATE` flag is removed; the log
line is `gate started at <tip10> on <pool> task=<task>`.

### 7.2 Reading the reply

`pump.py:962 finish_gate(text)` parses the receipt:

- marker blocks `FAILING-BEGIN\s*\n(.*?)\n?\s*FAILING-END` (dotall); the last
  block whose lines are non-empty and do not start with `<` supplies the
  names (template examples in the brief are skipped);
- no markers and no `BUILD-FAILED` in the text → the regex fallback
  `\b([a-z][a-z0-9_-]+) ((?:[A-Za-z0-9_]+::)+[A-Za-z0-9_]+)\b`, sorted and
  deduped, with the log `gate markers missing; read <n> names from the reply
  text`;
- `names is None` → exception `gate-unreadable`; `names == ["BUILD-FAILED"]`
  → exception `gate-build-failed`. Both append the gate record and stop.

### 7.3 Baseline, known-red, owned, new failures

The first readable gate sets `st["baseline"] = names`. Later gates:

- `known-red.txt` lines are always ignored (`pump.py:990`).
- `st["red"]` (the owned map, failing name → owner need id) drops names that
  no longer fail, logging `red fixed: <name> (owner <need>)` — fixed-name
  removal.
- **new** = `sorted(set(names) - set(baseline) - known - set(owned))`.
- `red-after-fix`: an owned name that fails again while all of its owner's
  units are settled (`merged`, `stuck`, `no-defect`, `replaced`) raises one
  exception `red-after-fix` per name, once (`st["red_reported"]`,
  `pump.py:976-980`).

When `new` is non-empty, each new name is owned by
`"pumpfix-g" + tip[:8]`; suspects are the merged-status units in
`st["order"][prev_merges:this_gate.merges]`; the gate-fix planner is started;
and exception `gate-new-failures` is written with
`<n> new: <names ; joined, 350-char cap> ; suspects: <uids, 200-char cap>`.
A red gate never stops merging (ruling 1).

### 7.4 The gate-fix planner

`pump.py:919 gatefix(tip, clean, new)` uses need id `<fix_prefix>-g<tip8>`,
concretely `pumpfix-g<tip8>` (`fix_prefix` = `pumpfix`): if
`plans/<need>.json` already exists it does nothing. Otherwise it writes
`gatefix/<need>.prompt` from `GATEFIX_PROMPT` (`pump.py:907`): the failing
names, the clean commit, `git log --oneline <clean>..<tip>` as the suspect
list, the plan format `{"need": …, "verdict": "units", …}` with unit ids
`<need>-1`, `<need>-2`, …, one likely cause per unit, proofs naming the
failing tests with one `cargo test` filter per command — and launches the
planner detached. `clean` is the tip of the last gate with names and no new
failures (`pump.py:1000`).

### 7.5 Promote

`pump.py:949 promote(tip)` fast-forwards main only: after
`git fetch -q origin main`, if `origin/main` is not an ancestor of the tip it
logs `promote skipped: main has commits next does not have yet` and returns;
otherwise `git push origin <tip>:refs/heads/main` and the log
`promote main -> <tip10> rc=<n> …`. Called exactly when a readable gate has
no new unowned failure.

### 7.6 The hold cycle

`pump.py:931 hold_cycle`, while `HOLD-MERGE` exists: once every `pumpfix`
unit has settled, a clean gate newer than the flag file lifts the hold
(`hold lifted: gate at <tip10> is clean`); otherwise, when fix units merged
after the last gate or the last gate was unreadable, it writes `FORCE-GATE`
containing `after fixes\n` and logs `hold: fixes merged, forcing a gate`.

### 7.7 Gate timeouts

The gate worker gets `--budget 1800`. On the pump side, collect finishes the
gate when reconcile reports the task `finished`, `timeout`, `lost` or
`error`, or when the state is unknown for more than 2400 s after dispatch
(`pump.py:501-504`) — the receipt is read and classified whatever it says.

## 8. The board

`pump.py:319 board(st)` is throttled to one pass per `BOARD_EVERY` = 300 s
(`b["at"]`). A pass, in order:

1. **sync pull** — `vivi sync pull` (120 s); rc and the last output line are
   logged.
2. **candidate rows** — `vivi need list --from operator@bd371d96.local
   --status open --json` (keys `handle`, `date`, `subject`), then
   `marked_rows()` (`pump.py:304`): the two forms
   `need list --status open --json --held` and
   `need list --status open --json --to mind`, keeping rows whose `marks`
   list contains `pump`; marked rows not already present are appended. A
   marked open need is plannable whatever its sender or date (ruling 4).
3. **skip list** — whitespace-split handles in `<home>/skip.txt`.
4. **since-cutoff** — a row with `date < "2026-10-14T00:10"`
   (`BOARD_CUTOFF`, string compare) is skipped **unless** it is in this
   pass's `replan` set or it is a marked (approved) need. Also skipped: needs
   already in `st["needs"]`, needs already being planned.
5. **planners cap** — at most `PLANNERS_MAX = 3` concurrent planners
   (`len(b["planning"]) >= 3`); a need whose plan file already exists is not
   re-planned. Timed-out planners: a handle with no plan file after 1800 s
   leaves `planning` with exception `plan-no-answer`, text
   `the planner wrote no plan in 30 minutes` (`pump.py:338-341`).
6. **planner launch and its files** — `vivi need show <h>` (60 s) is stored
   as `needs/<h>.json` = `{"need": h, "subject": …, "triage": "",
   "thread": <first 40000 chars>}`; `board/<h>.prompt` gets `PLAN_PROMPT`
   (`pump.py:250`): *Plan one need, as your system prompt describes. Its
   thread is in /mnt/agents/pump/needs/<h>.json. Write
   /mnt/agents/pump/plans/<h>.json. Your final message is one line: handle,
   verdict, number of units.* The detached `claude -p` launch of section 1.4
   follows, and the log line is
   `board: planning new need <h> <subject[:70]>`.
7. **replies** — for every candidate handle with a need record, at stage
   `planned` (always, once) and stage `merged` (when every unit is
   satisfied), sent as `vivi mail reply <h> --from mind --to
   operator@bd371d96.local --body-file /tmp/pump-reply.txt` (60 s). Bodies,
   exactly:

   ```
   pump (pharos): planned. verdict: <verdict>; <n> unit(s).
   <reason>
   - <uid> <title>
   ```

   ```
   pump (pharos): every unit of this need is merged into branch next.
   - <uid> <status> <commit[:10]>
   Not reviewed. Not on main.
   ```

   Each stage is replied once (`b["replied"]` keys `<h>:planned`,
   `<h>:merged`); the log line is
   `board reply <h> <stage> rc=<n> …`.
8. **push** — when any reply succeeded, a detached
   `vivi sync push --since 30m --drain --project <proj>` appending to
   `<home>/sync-push.log`, logged as `sync push started (detached, drain)`.

### 8.1 Parked-need re-plan on an operator reply

`pump.py:275 replan_answered`: a parked need (verdict `operator`, `design` or
`hold`, no units) whose plan file exists and whose thread carries an operator
message newer than the plan file's mtime (`operator_replied_after`,
`pump.py:256`) is re-planned: the plan moves to
`plans/superseded/<h>.<epoch>.json`, the basename leaves `plans_seen`, the
need record is deleted, any `planned`-stage reply key is dropped, the handle
joins `b["replan"]` (which bypasses the since-cutoff for this pass), and the
log says `board: operator replied on parked need <h> (was <verdict>);
re-planning` (ruling 5).

## 9. Exceptions and log lines

### 9.1 Exception kinds

`pump.py:126 exception` appends `HH:MM:SS kind unit text` to
`exceptions.txt` (UTC), newlines in the text become ` | `, the text is capped
at 600 characters, and a log line `EXCEPTION <kind> <unit>` is written.

| Kind | Trigger | Text written |
| --- | --- | --- |
| `bad-plan` | a plan file fails validation (`pump.py:223`) | `unit missing id, brief or write_scope` or `duplicate unit id <uid>`; unit field is the plan file basename |
| `split-empty` | a split plan with `units: []` replaces a unit (`pump.py:237`) | the plan's `reason` (e.g. "Already delivered: commit …") |
| `stuck` | `attempts >= 3` on a fail (`pump.py:411`) | `<why>: <detail[-300:]>` |
| `no-defect` | receipt contains `VERDICT: no-defect` (`pump.py:491`) | the last 400 chars of the receipt |
| `split-no-answer` | unit `splitting` longer than 1500 s (`pump.py:497`) | `the split planner wrote no plan in 25 minutes` |
| `gate-unreadable` | gate reply yields no names (`pump.py:984`) | the last 500 chars of the gate receipt; unit field is the gated tip[:10] |
| `gate-build-failed` | gate reply is exactly `BUILD-FAILED` (`pump.py:984`) | the last 500 chars of the gate receipt |
| `red-after-fix` | an owned failing name fails again after its fix units settled (`pump.py:980`) | the failing test name; unit field is the owner need id |
| `gate-new-failures` | a readable gate has new unowned failures (`pump.py:1005`) | `<n> new: <names> ; suspects: <uids>` (350/200-char caps); unit field is the gated tip[:10] |
| `plan-no-answer` | a board planner wrote no plan in 1800 s (`pump.py:340`) | `the planner wrote no plan in 30 minutes` |

### 9.2 Log line shapes a script may grep

`pump.py:82 log` writes `HH:MM:SS <msg>` (UTC). Stable message shapes:

| Prefix / shape | Where |
| --- | --- |
| `dispatch <uid> a<n> <pool> <model> task=<task>` | `pump.py:619` |
| `task send failed <uid>: <out[-200:]>` / `dispatch failed <uid>: <out[-300:]>` | `pump.py:602`, `pump.py:608` |
| `delivered <uid> <branch> secs=<n> model=<model> pool=<pool>` | `pump.py:481` |
| `fail <uid> attempt=<n> <why>` | `pump.py:414` |
| `split-requested <uid>` / `split <uid> into <n> units` | `pump.py:442`, `pump.py:229` |
| `plan <need> verdict=<v> units=<n>` | `pump.py:232` |
| `proof <uid> rc=<n> <n>s: <cmd[:90]>` | `pump.py:656`, `pump.py:730` |
| `batch <n> shared rc=<n> <n>s: <cmd>` / `batch of <n>: <n> merged` | `pump.py:716`, `pump.py:813` |
| `merged <uid> tip=<tip[:10]> in <n>s` | `pump.py:847` |
| `scope-warn <uid> touched outside scope: <files>` | `pump.py:845` |
| `<why> <uid>: <detail[:200]>` (undecided merge results) | `pump.py:836` |
| `merged main into next` / `main does not merge into next cleanly: …` | `pump.py:687`, `pump.py:686` |
| `gate started at <tip10> on <pool> task=<task>` / `gate dispatch failed: …` | `pump.py:905`, `pump.py:898` |
| `gate markers missing; read <n> names from the reply text` | `pump.py:975` |
| `gate baseline at <tip10>: <n> failing names` / `gate at <tip10>: <n> failing, <n> new` | `pump.py:988`, `pump.py:1007` |
| `gatefix planner started for <need> (<n> new)` | `pump.py:928` |
| `promote main -> <tip10> rc=<n> …` / `promote skipped: main has commits next does not have yet` | `pump.py:958`, `pump.py:954` |
| `red fixed: <name> (owner <need>)` | `pump.py:994` |
| `hold lifted: gate at <tip10> is clean` / `hold: fixes merged, forcing a gate` | `pump.py:939`, `pump.py:946` |
| `remote merger did not take batch <bid>; merging here` / `remote merger timed out on batch <bid>; merging here` / `remote batch <bid>: <n> results in <n>s` | `pump.py:788`, `pump.py:795`, `pump.py:798` |
| `board: planning new need <h> <subject[:70]>` | `pump.py:361` |
| `board reply <h> <stage> rc=<n> …` | `pump.py:383` |
| `board: operator replied on parked need <h> (was <v>); re-planning` | `pump.py:295` |
| `sync pull rc=<n> …` / `sync push started (detached, drain)` | `pump.py:326`, `pump.py:390` |
| `EXCEPTION <kind> <unit>` | `pump.py:129` |
