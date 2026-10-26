# Pump contract

The pump runs needs through flows. A flow is a fixed chain of steps, each owned by a role. The pump starts a step, hands the role a brief, reads the role's answer, and follows the flow to the next step. The pump persists its state in the pump home, normally **.vivi/pump/pump.db**. Pool jobs are the execution boundary: the pump selects and records work, and the configured role and pool run it.

The flows ship with vivi as Mermaid sources: **flows/build.mmd**, **research.mmd**, **release.mmd** and **repair.mmd**. Print one with **vivi flow show &lt;kind&gt;**, list them with **vivi flow list**, and add **--need &lt;need&gt;** to see that need's runs, steps, states and verdicts.

This file is the flow contract: what starts a run, what each step receives and answers, and how records, actions and answers move a run. The shape of every letter (pump record, action, notes, the `[job]` and `[step]` tables, role replies) is in [envelope](envelope.md). Job identity, briefs, restart recovery and runner publication are in [pump protocols](pump-protocols.md). The read-only monitor and report surfaces are in [monitoring and reports](monitoring-and-reports.md).

## Control and tick

**run** takes the pump-home lock, loads state, recovers dispatched pool jobs, then runs ticks until control is stopped or shutdown is requested. **once** performs one tick under the same single-writer lock. **pause**, **resume**, **stop** and **start** update durable control state in pump.db. A fresh pump.db starts **stopped**: run **vivi pump start** to enable it (a registered service starts it). A stop request lets the current tick finish. Stop does not cancel or drain pool jobs.

A tick applies operator actions, collects finished pool jobs, advances every live flow run, then (unless paused) queues cadence work and dispatches pending jobs. A paused tick still collects and advances runs, but dispatches nothing new.

The controller reloads mailspace configuration between ticks. Valid pool, routing, dispatch, rung and flow changes apply on the next tick, and a target added with **vivi pump target add** is cloned and applied then. A change the pump cannot apply, such as the pump home or a running target's checkout path, keeps the running value and raises one **config-reload-refused** exception; applying it needs a restart. Invalid configuration is reported and the last valid snapshot stays in use.

## Records, kinds and runs

A run starts from a **pump record**, never from a need alone. **vivi pump send --need &lt;need&gt;** files one record of kind `pump` for one need; each tick, intake starts one flow run for every need with an open record and no run in flight. The record's letter shape and the refusals at send are in [envelope](envelope.md#pump-record).

The run kind is the record's `kind` (`build`, `research`, `release` or `repair`). A record with no kind falls to the need's marks (`research`, then `release`, then `repair`), else `build`. The kind is fixed when the run starts; a later mark never changes a started run.

`[pump.flow] max_live_runs` caps live runs; unset means no cap. Repair runs neither count toward the cap nor wait for it, and a target has at most one repair run. Intake takes repair needs first, then by need priority (**vivi pump priority** sets it), then oldest record first. Dispatch ranks pending jobs the same way: repair, head-cto, priority, age.

A run has a number (`r1`, `r2`, ...) and a state: `starting`, `live`, `restarting`, `finishing`, then `done`, `withdrawn` or `superseded`. Each run owns a **sentinel** task, which closes last, and one task per step. Every step task carries a `[step]` table naming the need, run and node. The run-start reply on the need thread (**pump: run rN started**), the `[pump]` notes on step tasks, and **vivi flow show &lt;kind&gt; --need &lt;need&gt;** show where a run stands. **vivi pump list** and **vivi pump show** read the records.

A run ends one of three ways. The need lands: the pump closes the need, closes the sentinel, and closes the record **landed**. The need is withdrawn: the record closes **withdrawn** and every open task of the run closes `withdrawn`. The need is replanned: the record closes **superseded** and a fresh record starts a new run. A run that is locked (below) keeps its record open until **vivi pump unlock**.

A reply from anyone but the pump on a need's thread, newer than the run's start post, restarts a live or parked build run at **plan**. Once a merge to main has started, or when the run is locked, the pump raises **flow-thread-changed** once and restarts nothing.

## Roles and pools

Each step names a role. The role's pool is the primary route; the pool's **fallback** chain adds later candidates. A job with no free slot stays pending. A task a pool has accepted stays with that pool. Fallback targets must exist, and fallback cycles fail validation.

| Step | Role | Job |
| --- | --- | --- |
| research | researcher | read-only |
| plan | pump-planner | read-only |
| admit | admitter | read-only |
| slice, rework | slicer | read-only |
| slice task | hand | writes the slice branch |
| slice merge, merge to main | merge role (model-free job) | merges and pushes by script |
| sync main into need branch | merge role | model-driven merge |
| audit, judge | auditor | read-only |
| need rung, main rung, raised rung | `[pump.rungs] pools` | runs the rung command |
| escalation | head-cto | read-only |
| repair attempt | repairer | writes the repair branch |
| repair audit | auditor | read-only, after landing |
| cadence, digest | the role whose schedule is due | read-only |

**vivi config validate** checks that the roles a flow needs exist and have pools. A job the pool refuses three times is **held** and raises one **job-dispatch-refused** exception. Release it with **vivi pump requeue --job &lt;job-id&gt;** or requeue its step task, or withdraw the need.

## Build flow

A build run starts at **plan**. A step answers with one word from its role's envelope ([envelope](envelope.md#fields-by-role) lists the words and fields). The word `failed` belongs to the pump: it is the answer of a step whose job failed or gave no valid envelope, and it always routes to head-cto.

| Step | Receives | Answers | Next |
| --- | --- | --- | --- |
| plan (pump-planner) | the need's `done_when`, `target` and `depends_on` lines, the need thread, the target ids, read-only checkouts of every target at main; on a revision, the first plan and the admitter's reasons; after a restart, the prior need branch | `planned` | admit |
| | | `delivered` | run ends (the need is already met) |
| | | `operator`, `hold` | parked |
| | | `design`, `failed` | head-cto |
| admit (admitter) | the plan and the need thread | `admitted` | slice |
| | | `revise` (first time) | plan revision, then admit again |
| | | `escalate`, `failed`; `revise` the second time | head-cto |
| slice (slicer, mode slice) | the admitted plan, the targets, the slice size from `[pump.slice] minutes`, | `sliced` | slice tasks |
| | | `escalate`, `failed` | head-cto |
| slice tasks (hand) | see below | all slices merged | audit and need rung |
| | | no slice merged, or a slice exhausted | head-cto |
| audit (auditor) | the review range of the need branch, the admitted plan, the need thread, the requirements to account for; a second audit also gets the first audit and the rework slices | `clean_pass`, `residual`, `block_range`, `block_ship` | join |
| need rung | the need-branch tip; the `need` rung command | `pass`, `fail` | join |
| judge (auditor, short mode) | the audit and the need rung report | `block`, `residual` | rework or raise check |
| rework (slicer, mode rework) | the audit findings, requirement gaps, judge reason and failing rung output | `sliced`, `escalate` | rework tasks, then verify again |
| merge to main | model-free script | `merged` | run ends |
| | | `conflict` | sync |
| sync (merge role) | main merged into the need branch | `merged`, `conflict` | need rung, then merge to main again |

### Slice tasks

The slicer's `[[slice]]` tables become slice tasks on the need. Each names a target, a write scope and the slices it depends on. A slice starts when every slice it depends on has merged. Two open slices of a run never hold overlapping write scopes; a hand that asks for paths outside its scope is checked against the other open slices, and a request that is too large goes back to the slicer.

A hand gets the hand rules, its write scope, the hand envelope contract, the deviations of slices already landed, and the slice task body. It works on its own branch and answers `built`, `blocked-scope` or `failed`. The runner's push result decides whether it built anything; the envelope reports.

A built slice goes to a model-free merge job under the merge role. It merges the hand's commit onto the need branch **vivi/need/&lt;need&gt;**, runs `[pump.merge] format` and the `task` rung, and publishes the branch only when both pass. One slice merge per need is in flight at a time. A conflict, a red rung or an empty merge is a failed attempt. A failed slice is tried again once (two attempts in all), then exhausted. An environment failure (a missing tool, a host setup failure) is never retried: the pump raises an exception and holds the slice for the operator.

A hand's deviations and follow-up items are posted on its slice task as pump notes. A required follow-up goes to the slicer in rework mode; an optional one is only posted.

### Verify

When every slice has merged, the audit and the need rung run side by side on the need branch. The join reads both:

- an audit of `block_range` or `block_ship` blocks, and goes to rework;
- a failed audit job, or a lost rung, goes to head-cto;
- a failing rung goes to the judge, which answers `block` (rework) or `residual`;
- a judge `residual` goes to the raise check, which goes to merge to main unless a raise is asked;
- a passing rung with an audit that names `raise_rung` goes to a raised rung;
- otherwise the need is verified and goes to merge to main.

A raised rung runs the named rung (`need`, `main` or `release`) at the same tip. A raise holds for the rest of the run. A pass goes to merge to main; a fail goes to rework in the first pass and to head-cto in the second.

Rework is one pass. The slicer cuts rework slices, the hand and merge steps run them, and a second verify pass follows with the first audit as context. A block or failure in the second pass goes to head-cto.

### Merge to main

Merge to main is a model-free job: it merges the need's tip into the target's main with a no-fast-forward merge, and the runner pushes it. The push is fast-forward-only and the runner checks ancestry, so a main that moved reads as a conflict. A conflict goes to **sync**, where the merge role merges main into the need branch and publishes the result; the need rung then runs again before one more merge to main. A second conflict, or any failure, goes to head-cto.

A merge to main closes the need and the record **landed**. The runner publishes only to the refs [pump protocols](pump-protocols.md#runner-publication) lists.

### Head-cto and the lock

Every failure path ends at head-cto. It is read-only. It receives the run history of the need (every run, its entry and its steps), why this run reached it, the admitter, auditor, judge and earlier head-cto replies, the block-ship audit text, and the rung failure reports.

Its words restart the need in a new run: `research`, `plan`, `slice` or `rework` (the build flow's entries), or `lock`. From the third run on, every answer except `lock` needs a non-empty `new_fact`. A locked run waits for the operator and nothing else: no reply frees it. The pump raises **flow-need-locked** once. **vivi pump unlock &lt;need&gt; [--entry &lt;entry&gt;]** restarts it at the named entry, or at **plan** when none is named.

A parked run (the planner said `operator` or `hold`) waits too: a reply on the need thread restarts it at **plan**, or answer the step by hand.

## Research flow

A research run has one step. The researcher gets the need thread and the want the need's `explores` key names, and answers `READY`, `NOT READY` or `NEEDS FURTHER REVIEW`. Whatever it answers, the run ends and the pump posts the analysis as a reply on the want. A run with no usable envelope posts a failure note on the want instead and raises an exception.

Research is filed by head-ceo: **vivi need send --mark research --explores &lt;want&gt; --done-when "..."**, then **vivi pump send --need &lt;need&gt;**. A research need does not count toward the want's state. The build flow can also enter at **research** (a head-cto word), and then `READY` goes on to plan; any other word goes to head-cto.

Cadence asks head-ceo to send at most two research needs for one want; after that it sends serving needs (**vivi need send --serves &lt;want&gt;**) or drops the want. Cadence and digest jobs are queued for roles whose configured schedule is due and whose open-work limits allow another run; they are read-only and their output is mail.

## Release flow

A release run has one waiting step. It answers `ready`, and the run ends, once every need in the release need's `depends_on` has landed on main. A prerequisite that closed without landing holds the step for the operator and raises an exception; answer the step by hand once the operator has decided.

## Repair flow

A repair run has two repairer attempts. Each attempt works on a branch cut from the current main tip and gets the repair need, the target, the tested tip, the current main tip, the failing output, and the rule never to weaken, delete or ignore a test. The second attempt also gets the first attempt's steps, facts and rung verdict. A repairer answers `built` or `failed`.

Each newly constructed attempt job (the **fix1** and **fix2** jobs) carries one hard time cap: **[pump] repair_attempt_secs**, in seconds, default 1800, accepted range 1 through 43200 inclusive. The attempt's brief advertises the exact same seconds value, and the cap covers the whole attempt, builds and tests included. The main-rung job that follows on the repair branch has its own timeout; **[pump.dispatch] budget** is a different control and does not size repair attempts. A valid reload applies to newly constructed attempt jobs; already-created pending or dispatched jobs keep their saved budget, and an automatic lost-job rerun reruns the saved job with its saved budget. The two-attempt policy and the meaning of an overtime attempt are unchanged. Example: **repair_attempt_secs = 7200** under **[pump]** gives each new attempt four hours; a larger cap admits longer attempts and does not guarantee that a slow repair finishes.

A built attempt runs the main rung on the repair branch. A pass goes to merge to main and the run ends; a fail, a conflict, an overtime attempt or an unchanged branch goes to the second attempt, and after the second to **unrepaired**. In **unrepaired** main stays red, the pump raises one alarm, and the run waits for a green main rung at a tip that holds the tested tip, or for **vivi pump unlock**.

A repair need is filed by the pump. When the `main` rung of a target goes red on a new main tip, the pump sends one repair-kind need for the target and opens its record with `kind = "repair"`, `target` and `tested`. A repeat red with the same failing ids does nothing; a different failing set replies on the open repair need. A green main rung lands a repair need that waits at unrepaired. A repair run at any other step whose tested tip main now holds ends outside the flow: the need closes and the run is withdrawn. After a repair lands, a non-blocking auditor job reviews it and sends one need for each finding.

The main rung runs when a target's main has a new tip, at most once every 30 minutes, using the `main` command in `[pump.rungs]`.

## Operator controls

Each control posts one action on the need's open pump record ([envelope](envelope.md#pump-action) has the letter shape). The pump applies every action letter once and raises **flow-action-refused** when it cannot.

| Command | Effect on a flow run |
| --- | --- |
| **vivi pump requeue** &lt;step task&gt; | queues the step's newest job again, once that job is terminal and the step is open; a job held after pool refusals is released in place |
| **vivi pump withdraw** &lt;need&gt;... --reason | closes the need `withdrawn` and the record `withdrawn`; the run closes every open task |
| **vivi pump replan** &lt;need&gt; | restarts the live run at **plan** |
| **vivi pump answer** &lt;step&gt; &lt;word&gt; | answers a step by hand; the step closes with that word as its verdict and the flow follows its edge |
| **vivi pump unlock** &lt;need&gt; | restarts a locked run |

Answer a step only with one of its words. A slice task takes `merged`, `no-defect` or `failed`. Closing a step task by hand is a foreign close: the pump reopens it. When git cannot settle whether a step's side effect landed, the step is held for **vivi pump answer**. A step that fails on several ticks in a row is held for the operator, with an exception.

An action posted on a closed record, or a letter that does not parse as an action, is never applied (the pump raises an alarm for it).

## Jobs and briefs

A job is saved before a pool is chosen; its stable id makes queueing idempotent, and a restart matches the saved job to its task. The `[job]` table, brief notes and restart recovery are in [pump protocols](pump-protocols.md). A brief over the project server's 64 KiB letter cap is not sent whole: the head and tail stay, the middle is replaced by a marker, the full text is saved as **job-briefs/&lt;job&gt;.full.txt**, and one **job-brief-truncated** exception records the cut.

Step jobs that read are read-only: the checkout starts at the target's main ref and every other target is checked out beside it. A role answers with the envelope in [envelope](envelope.md); a reply with no valid envelope, or one that disagrees with git, is recorded and the machine's fact wins. Facts come from machines (the runner's push result, exit codes, git history); the envelope carries judgment and reports.

## Pump home

The pump home defaults to **&lt;project&gt;/.vivi/pump**. It holds **pump.db** (schema 8: flow runs, steps, step jobs, need branches, rung runs, applied actions and main state, beside the durable jobs), **lock**, **status.txt**, **log.txt**, **exceptions.txt**, **monitor-cycles.jsonl**, **job-briefs/**, **board/**, **repair/**, **follow-up/**, **sync-push.log** and **repo/**. **exceptions.txt** is where alarms land, named by kind (**flow-need-locked**, **flow-need-parked**, **flow-slices-invalid**, **flow-side-effect-unclear** and so on); read it before reading the log.

There are no home marker files for stop or pause; use the control commands. **vivi monitor** shows pump rows and tick cycles, and **vivi report** reads attempt data and pump receipts; see [monitoring and reports](monitoring-and-reports.md).
