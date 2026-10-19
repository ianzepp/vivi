# Pump contract

This reference describes the live path used by **vivi pump run** and **vivi pump once**. The pump persists its state in the mailspace pump home, normally **.vivi/pump/pump.db**. Pool jobs are the execution boundary for planner, merge, gate, audit, promotion, cadence, investigation, and reclaim work. The pump selects and records work; the configured role and pool run it.

For worker receipt formats, see [pump protocols](pump-protocols.md). For the read-only monitor and report surfaces, see [monitoring and reports](monitoring-and-reports.md).

## Control and tick lifecycle

**run** takes the pump-home lock, loads state, recovers dispatched pool jobs, repairs orphaned merge state, then runs ticks until control is stopped or shutdown is requested. **once** performs one tick under the same single-writer lock. **status** reads the saved status and control state without taking the pump lock. **pause**, **resume**, **stop**, and **start** update durable control state in pump.db.

A normal tick applies completed results, ingests plans, collects unit and job receipts, starts due gates, queues merge and cadence work, dispatches pending role jobs, dispatches ready unit tasks, requests a board pass, and saves. A paused tick still collects completed work and runs board bookkeeping, but does not start new dispatch, merge, or gate work. A stop request lets the current tick finish before the process exits. Stop does not cancel or drain pool jobs.

The controller reloads mailspace configuration between ticks. Valid pool, routing, dispatch, merge, gate, budget, and check changes take effect on the next tick. The pump home and repository target checkout paths stay fixed while the lock is held; changing either requires a restart. Invalid configuration is reported and the last valid runtime snapshot remains in use.

## Routing: kind to role to pool

A role's assigned pool is the primary route. The pool's **fallback** chain adds later candidates. A durable job remains pending when no candidate has capacity. The dispatcher tries candidates in order and sends a job only after a slot is available. A task already accepted by a pool stays owned by that pool; fallback is for dispatch, not migration of a running task. Fallback targets must exist, and fallback cycles fail configuration validation.

| Work kind | Role addressed | Pool selection |
| --- | --- | --- |
| Unit task | **[pump].to** (the hand role) | The hand role's pool and fallback chain. If that role has no pool, legacy **[pump.dispatch.kinds]** pool lists route the unit; **[pump.dispatch.lanes]** supplies model caps. A configured unit kind is still required. |
| **planner:board**, **planner:revision**, **planner:split** | Active **planner** role | Planner role pool and fallback chain. An active planner role with a pool is required when **[pump]** is configured. |
| **merge** | **merge** role | Merge role pool and fallback chain. The live pipeline requires a merge role with a usable pool. |
| **gate** | **gate** role | Gate role pool and fallback chain, followed by configured **[pump.gate].pools** candidates. The gate job is read-only. |
| **audit** | **auditor** role | Auditor role pool and fallback chain, followed by configured gate-pool candidates. If no auditor role exists, the built-in read-only audit fallback uses gate pools. |
| **investigation** | Configured **auditor** role | Auditor role pool and fallback chain. Investigation work is read-only and is not eligible without an auditor role. |
| **promote** | **release** role, or **merge** when no release role exists | Selected role pool and fallback chain. Publication is restricted to the exact configured main ref and candidate. |
| **cadence**, **digest** | The scheduled role itself | That role's pool and fallback chain. |
| **reclaim** | **reclaim** role, or the hand role | Local-provider pools only; it never falls back to a remote pool. |

Pool fallback is configured in the pool's table. For example, **[pools.workers]** can set **fallback = "backup"**. For unit work, the hand role pool is preferred; when no hand pool is assigned, legacy kind pool lists remain the compatibility route. A configured role pool takes precedence over legacy dispatch kind routing.

Some old settings remain accepted to read existing configurations. They are not the live execution path: **[pump.planner].run** and **charter** are not executed; **[pump.merge].lanes**, **remote_wait**, **REMOTE-MERGE**, and the **merge-jobs/** file protocol do not select or run live merges. **[pump.merge].remote_limit** still supplies the minimum pool merge-job budget, as max(remote_limit, dispatch.budget); it is no longer a remote-claim timeout. The old **[pump.ramdisk].lane_gib** setting applies only to uncalled local merge helpers. Configure per-job RAM disks on supported local pools under **[pools.&lt;name&gt;.ramdisk]** when needed.

## Durable pool jobs

A role job is saved in the pump state before pool capacity is selected. Its stable job ID makes queueing idempotent. The saved record includes the kind, role, subject, brief, candidate pools, task handle, selected pool, dispatch status, receipt, and application state. Jobs with no slot remain pending. Once dispatched, the pool owns execution and the pump polls the task receipt.

On startup, the pump lists tasks for each role and matches the stable job marker in the task subject and body. It restores missing handles, reads terminal receipts, and applies completed results through the regular tick path. A dispatched merge job continues to own its units across a restart. A unit left in **merging** without an owning durable merge job returns to **delivered** so it can be retried.

The SQLite store is **pump.db**, schema version 6. Its state includes units, plans, gates, promotions, control, requeue/replan requests, and the durable **jobs** map. **state.json** is an import/export format, not the live store. The first migration from an old state file imports it and archives it as **state.json.imported**.

## Unit lifecycle and merge ownership

Ready unit work is sent as a task to the hand role and dispatched to a pool branch named **unit/&lt;unit-id&gt;-a&lt;attempt&gt;**. Unit attempts remain in the unit state machine; role jobs use the separate durable jobs map.

The principal unit states are:

- **pending**: ready or waiting for dispatch.
- **held**: its repository target is unavailable.
- **inflight**: a pool task is running.
- **delivered**: the worker completed and its branch is available for integration.
- **merging**: a durable merge job owns the unit.
- **merged** and **no-defect**: satisfied outcomes.
- **quarantined**: legacy state retained from the old local merge path; current durable merge receipts use merged, dropped, or undecided.
- **stuck**, **needs-replan**, **splitting**, **replaced**, and **withdrawn**: parked, replacement, or operator-managed states; they are not satisfied unless their successors are.

The merge scheduler queues at most one active merge job per repository target. It includes eligible delivered unit branches and eligible completed write-role branches, and records the source IDs before dispatch. A worker starts from the integration tip, integrates the listed sources, runs configured formatting, shared checks, scope-mapped checks, and unit proofs, then commits a candidate. It returns a structured result receipt and does not push the integration ref. The privileged pool runner publishes the candidate; the controller accepts a merge only after checking the worker's clean exit, exact source list, runner publication receipt, current remote ref, and candidate ancestry. The full protocol is in [pump protocols](pump-protocols.md).

## Planning and gate behavior

Whole-need planning, thread amendments, and split planning all use durable jobs for the active planner role. The worker returns complete plan JSON inside the **PUMP-PLAN-BEGIN** and **PUMP-PLAN-END** markers; the pump materializes the plan under **plans/** and passes it through normal validation and ingestion. Planner command execution and detached planner reapers are not used.

A gate is due when there is no baseline, when a merge has landed since the previous gate, or when **FORCE-GATE** requests one. The legacy **every_merges** and **every_seconds** thresholds are ignored. A gate pins one integration tip, runs the configured command in a read-only job, and returns failing names inside **FAILING-BEGIN** and **FAILING-END**. An optional external result file may finish a gate while a live runner heartbeat is present; otherwise the role job runs.

New or residual failures are owned by durable read-only audit work. A valid audit report creates one fix unit per reported root cause. **HOLD-MERGE** prevents unrelated merges while failures remain owned; fix and qualifying priority work can proceed. A clean gate can queue a promotion job for the release role or merge role. Promotion is recorded only after the configured main ref is independently verified to contain the requested gated tip.

Cadence and digest jobs are queued for roles whose configured schedule is due and whose open-work limits allow another run. Head-ceo investigation needs must carry the **investigate** mark and a body line **for: &lt;want-handle&gt;**. The pump sends a read-only job to the configured auditor. The investigation need closes only after a recent auditor reply appears on the want thread addressed to head-ceo. The default investigation cap is two rounds; the head-ceo charter can override it with **investigation-rounds: N**.

## Home, reports, and operator controls

The pump home defaults to **&lt;project&gt;/.vivi/pump**. It contains **pump.db**, **lock**, **status.txt**, **log.txt**, **exceptions.txt**, **needs-ready.txt**, plan and brief files, task/job briefs, and bounded monitor tick receipts. Repository checkouts and target-specific working directories are also kept under the configured home unless explicitly configured otherwise.

The operator control commands update durable state. **HOLD-MERGE** and **FORCE-GATE** are per-target control flags; **skip.txt** lists need handles that board planning ignores; the reserved file lists paths that block overlapping unit dispatch. There are no live STOP or PAUSE home marker files; use the CLI control commands.

**vivi monitor** displays pump rows and tick cycles. Its row snapshot is limited to persisted task, pool, and pump metadata. Selecting a row tails the chosen raw slot or pump log, whose contents are not redacted. **vivi report** reads synchronized attempt data and pump receipts; missing historical timestamps remain unavailable rather than being treated as zero. See [monitoring and reports](monitoring-and-reports.md) for filters, formats, and measures.
