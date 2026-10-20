# Pump contract

This reference describes the live path used by **vivi pump run** and **vivi pump once**. The pump persists its state in the mailspace pump home, normally **.vivi/pump/pump.db**. Pool jobs are the execution boundary for planner, merge, gate, audit, promotion, cadence, investigation, and reclaim work. The pump selects and records work; the configured role and pool run it.

For worker receipt formats, see [pump protocols](pump-protocols.md). For the read-only monitor and report surfaces, see [monitoring and reports](monitoring-and-reports.md).

## Control and tick lifecycle

**run** takes the pump-home lock, loads state, recovers dispatched pool jobs, repairs orphaned merge state, then runs ticks until control is stopped or shutdown is requested. **once** performs one tick under the same single-writer lock. **status** reads the saved status and control state without taking the pump lock. **pause**, **resume**, **stop**, and **start** update durable control state in pump.db.

A normal tick applies completed results, ingests plans, collects unit and job receipts, starts due gates, queues merge and cadence work, dispatches pending role jobs, dispatches ready unit tasks, requests a board pass, and saves. A paused tick still collects completed work and runs board bookkeeping, but does not start new dispatch, merge, or gate work. A stop request lets the current tick finish before the process exits. Stop does not cancel or drain pool jobs.

The controller reloads mailspace configuration between ticks. Valid pool, routing, dispatch, merge, gate, budget, and check changes take effect on the next tick, including the next board pass: that worker receives the live context with each request rather than keeping the startup snapshot. The pump home and repository target checkout paths stay fixed while the lock is held; changing either requires a restart. Invalid configuration is reported and the last valid runtime snapshot remains in use.

## Routing: kind to role to pool

A role's assigned pool is the primary route. The pool's **fallback** chain adds later candidates. A durable job remains pending when no candidate has capacity. The dispatcher tries candidates in order and sends a job only after a slot is available. A task already accepted by a pool stays owned by that pool; fallback is for dispatch, not migration of a running task. Fallback targets must exist, and fallback cycles fail configuration validation.

| Work kind | Role addressed | Pool selection |
| --- | --- | --- |
| Unit task | **[pump].to** (the hand role) | The hand role's pool and fallback chain; **tools** may be empty or name an unknown kind. Defined kind limits and proof-skip metadata still apply, but kind pool lists are ignored. Without a hand pool, configured kinds and **default_kind** route the unit. |
| **planner:board**, **planner:revision**, **planner:split** | Active **planner** role | Planner role pool and fallback chain. An active planner role with a pool is required when **[pump]** is configured. |
| **merge** | **merge** role | Merge role pool and fallback chain. The live pipeline requires a merge role with a usable pool. |
| **gate** | **gate** role | Gate role pool and fallback chain, followed by configured **[pump.gate].pools** candidates. The gate job is read-only. |
| **audit** | **auditor** role | Auditor role pool and fallback chain, followed by configured gate-pool candidates. If no auditor role exists, the built-in read-only audit fallback uses gate pools. |
| **need-audit** | Active **auditor** role | Auditor role pool and fallback chain. Managed need acceptance requires this role to have a configured pool. |
| **investigation** | Configured **auditor** role | Auditor role pool and fallback chain. Investigation work is read-only and is not eligible without an auditor role. |
| **promote** | Active **release** role with a pool, or active **merge** role with a pool as fallback | Selected role pool and fallback chain. Publication is restricted to the exact configured main ref and candidate. |
| **cadence**, **digest** | The scheduled role itself | That role's pool and fallback chain. A **mind**-kind cadence run is a read-only oversight wake: the brief is a compact pump-state snapshot, and the worker returns **PUMP_OVERSIGHT_RESULT=** JSON. Allowlisted actions are **hold-merge**, **clear-hold-merge**, **skip-unit**, **unskip-unit**, **replan-need**, and **note**. Anything else is refused and recorded; applying the receipt never blocks the tick. |
| **reclaim** | **reclaim** role, or the hand role | Local-provider pools only; it never falls back to a remote pool. |
| **ref-delete** | Active **merge** role | Merge role pool and fallback chain. Used to delete a failed attempt branch; the privileged runner deletes the ref. |

Pool fallback is configured in the pool's table. For example, **[pools.workers]** can set **fallback = "backup"**. For unit work, the hand role pool is preferred; when no hand pool is assigned, legacy kind pool lists remain the compatibility route. A configured role pool takes precedence over legacy dispatch kind routing.

Some old settings remain accepted to read existing configurations. They are not the live execution path: **[pump.planner].run** and **charter** are not executed; **[pump.merge].lanes**, **remote_wait**, **REMOTE-MERGE**, and the **merge-jobs/** file protocol do not select or run live merges. **[pump.merge].remote_limit** still supplies the minimum pool merge-job budget, as max(remote_limit, dispatch.budget); it is no longer a remote-claim timeout. Configure per-job RAM disks on supported local pools under **[pools.&lt;name&gt;.ramdisk]** when needed. A source contract test fails if production `src/pump` grows an in-process merger, planner, or git push of refs.

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

The shared integration merge scheduler queues at most one target-level job at a time. It includes eligible delivered units from unmanaged needs and eligible completed write-role branches, and records the source IDs before dispatch. Managed needs use the separate per-need merge path below. A shared merge worker starts from the integration tip, diagnoses and repairs conflicts and failing checks within each source's write scope, runs configured formatting, shared checks, scope-mapped checks, and unit proofs, then commits a candidate. Isolation among several sources is a last-resort diagnosis tool, not the default action. The worker returns a structured result receipt and does not push the integration ref. The privileged pool runner publishes the candidate; the controller accepts a merge only after checking the worker's clean exit, exact source list, runner publication receipt, current remote ref, and candidate ancestry. The full protocol is in [pump protocols](pump-protocols.md).

A fresh mailspace provisions the **merge** role on init. When a local pool already exists, that pool is assigned automatically; otherwise `vivi doctor` fails the **merge pool** check until `vivi role pool merge <pool>` binds one. Without that binding, the pump raises **merge-route-missing** and queues no merge jobs.

## Need-level acceptance and landing

The first unit dispatch reserves one managed branch for that need from the
target's observed integration tip. New need branches use
**vivi/need/&lt;need&gt;**; unit attempts use **vivi/unit/&lt;unit-id&gt;-a&lt;attempt&gt;**.
Unit merge jobs are serialized within a need and publish only to that need's
branch. Separate needs may progress concurrently. A unit reaching **merged**
or **no-defect** does not accept the need and does not publish its work to the
shared integration branch. Already-recorded in-flight branch names remain in
use as stored, with no prefix migration.

When every unit in the current plan revision is settled, the pump records the
need branch's exact target, revision, base SHA, and frozen tip SHA. It sends a
durable, read-only audit job to the Auditor role using the regular pool routing
and fallback rules. The audit brief identifies the exact **git diff
&lt;audit_base&gt;..&lt;tip&gt;** review range and includes the original need, current
plan, requirements, settled units, and changed paths. The first review starts
at the need branch base. A repair review keeps the original binding but starts
at the previous frozen tip, so its range contains only repair work. A plan
revision, unit membership, or branch-tip change invalidates the old acceptance
identity.

The audit receipt supports four verdicts:

- **clean_pass** accepts the exact frozen range when every declared
  requirement is satisfied or not applicable and there are no findings.
- **residual** accepts the reviewed range and files deduplicated ordinary
  follow-up needs for each finding or unmet requirement, retaining report,
  path, and recheck-trigger provenance.
- **block_range** keeps acceptance pending, creates repair units on the same
  need branch, then reviews only the repair diff under the Auditor policy.
  **[pump.need].repair_rounds** (default 1) is the cap: a finding that remains
  blocked after that many repairs is held and escalated.
- **block_ship** keeps a persistent hold on the need branch and records an
  operator and CTO escalation. It does not authorize landing.

Audit receipts bind the durable job ID, source-thread hash, need, target, plan
revision, original base SHA, frozen tip SHA, review-range base, and report
identity. The source-thread hash includes the need and its amendments while
excluding only authenticated pump-generated replies. A later source amendment
invalidates an earlier receipt even if the plan revision and branch tip have
not changed. Replaying a receipt is idempotent. A receipt for an old revision,
source snapshot, or tip is stale and cannot authorize landing.

Only a current **clean_pass** or **residual** receipt can enqueue the
predefined **merge** role for need landing. The Merge worker integrates the
accepted branch into the current configured integration ref, runs the checks
assigned by the merge contract, and returns a candidate receipt. The
privileged pool runner publishes the candidate. Before recording landing, the
pump verifies the exact ref and candidate and independently checks the
published target's ancestry against the accepted tip. It stores that verified
inclusion receipt before queueing a separate cleanup job. Cleanup is limited to
the recorded owned branch and expected SHA, and completes only after the
runner confirms the ref is absent. A failed cleanup leaves the branch and its
retry evidence intact.

Landing to the integration branch does not promote to **main**. The existing
gate pins and checks the landed integration tip; only a passing gate can make
that exact tip eligible for the configured release or promotion path.

## Planning and gate behavior

Whole-need planning, thread amendments, and split planning all use durable jobs for the active planner role. The worker returns complete plan JSON inside the **PUMP-PLAN-BEGIN** and **PUMP-PLAN-END** markers; the pump materializes the plan under **plans/** and passes it through normal validation and ingestion. Planner command execution and detached planner reapers are not used.

A planned need is rehashed from its source thread on every board pass. Board **last_event** is not the trigger: a reply records a **replied** event on the parent need so operators can see the thread moved, and the hash comparison asks the planner for a revision even when that stamp is unchanged or truncated to the same second. A reply on a closed need reopens it and starts the same revision path.

A gate is due when there is no baseline, when a merge has landed since the previous gate, or when **FORCE-GATE** requests one. The legacy **every_merges** and **every_seconds** thresholds are ignored. A gate pins one integration tip, runs the configured command in a read-only job, and returns failing names inside **FAILING-BEGIN** and **FAILING-END**. An optional external result file may finish a gate while a live runner heartbeat is present; otherwise the role job runs.

New or residual failures are owned by durable read-only audit work. A valid audit report creates one fix unit per reported root cause. **HOLD-MERGE** prevents unrelated merges while failures remain owned; fix and qualifying priority work can proceed. A clean gate can queue a promotion job for the active release role with a pool, or fall back to an active merge role with a pool. Promotion is recorded only after the configured main ref is independently verified to contain the requested gated tip.

## Proof validation, split-plan retries, and repair budgets

The optional **[pump.proof].allow** list adds executable basenames to the
built-in proof command allowlist. The built-in basenames are **[**, **awk**,
**bash**, **cargo**, **cmake**, **diff**, **env**, **find**, **git**, **go**,
**grep**, **ls**, **make**, **mkdir**, **node**, **npm**, **npx**, **pnpm**,
**python**, **python3**, **rg**, **sed**, **sh**, **test**, **true**, **vivi**,
**yarn**, and **zsh**. Added entries are matched by basename.

The validator splits shell chains using **;**, **|**, and **&&** outside
quotes. It unwraps leading **env** or **NAME=value** assignments and
**sh -c**, **bash -c**, and **zsh -c** wrappers up to two levels. A
repo-relative executable must exist in the unit's target checkout; generated
paths under **target/** may be created by the proof. Absolute paths are
validated by executable basename. A **cargo test** command may have at most
one positional TESTNAME filter; use separate proof commands for separate
filters.

Proofs are checked at plan ingestion and again before unit dispatch. An
invalid proof rejects a new plan as **bad-plan**. A proof that has become
invalid before dispatch parks the unit as **needs-replan** and requests a
planner revision, without charging a hand attempt. Each hand brief ends with a
**done_when:** condition: all listed PROOF commands exit successfully and the
configured rules pass, or the reply ends with **VERDICT: no-defect** and
evidence.

A split plan rejected by validation gets one re-ask with the validation error
added to the planner brief. The rejected file moves to **plans/rejected/**.
The second invalid split plan parks the unit as **stuck** with a
**split-plan-rejected** exception. Use **vivi pump replan <need>** to start a
fresh plan.

**[pump.merge].repair_budget** is an optional wall-clock budget for quarantine
repair units. When omitted it is **proof_timeout + 300 seconds**. An explicit
value must be at least **proof_timeout** and at least one second. Repair units
use this budget instead of **[pump.dispatch].budget**.

A failed shared check gets exactly one retry on the same tree before bisect
or quarantine. A timeout (exit 124 or the timeout wrapper's kill) is
infrastructure uncertainty. A timed-out check whose retry passes is recovered
and recorded. If a timeout is not followed by a passing retry, every source is
**undecided**, with an exception naming the command and timeout; the pump does
not bisect, quarantine, or create a repair unit. A non-timeout failure still
present after its retry may proceed to bisect. A solo timeout during bisect
remains **undecided** and is never counted as clean or as a culprit.

Durable merge receipts include **shared_check_timeouts** (timed-out check
executions), **shared_check_timeout_retries** (additional attempts after an
initial timeout), and **shared_check_recoveries** (recovered failed checks with
name, command, and first-failure detail). Pump status reports these totals
from terminal merge receipts; totals are marked incomplete or unavailable
when receipts lack the metrics.

## Gate audit retry limit

The same red integration tip and set of failing names gets at most three
failed audit jobs. Retry delays begin at 60 seconds and double after each
failure. Failures before enqueue and a missing slot for the built-in auditor
defer for 60 seconds without incrementing the failed-job count. A durable role
job with no free slot remains pending. Once the failure limit is reached, the
unchanged failure signature stays stopped until it changes or **vivi pump
replan <fix-need>** explicitly rearms it. Provider or harness failures create
an **environment** exception with the exit and receipt tail; exhaustion
creates **gatefix-planner-failed**. Status reports
**gatefix-active** or **gatefix-no-live-work**, attempt and failure counts,
queued/active/parked fix-unit counts, the reason, and the recovery action.

Cadence and digest jobs are queued for roles whose configured schedule is due and whose open-work limits allow another run. Head-ceo investigation needs must carry the **investigate** mark and a body line **for: &lt;want-handle&gt;**. The pump sends a read-only job to the configured auditor. The investigation need closes only after a recent auditor reply appears on the want thread addressed to head-ceo. The default investigation cap is two rounds; the head-ceo charter can override it with **investigation-rounds: N**.

## Home, reports, and operator controls

The pump home defaults to **&lt;project&gt;/.vivi/pump**. It contains **pump.db**, **lock**, **status.txt**, **log.txt**, **exceptions.txt**, **needs-ready.txt**, plan and brief files, task/job briefs, and bounded monitor tick receipts. Repository checkouts and target-specific working directories are also kept under the configured home unless explicitly configured otherwise.

The operator control commands update durable state. **HOLD-MERGE** and **FORCE-GATE** are per-target control flags; **skip.txt** lists need handles that board planning ignores; the reserved file lists paths that block overlapping unit dispatch. There are no live STOP or PAUSE home marker files; use the CLI control commands.

**vivi monitor** displays pump rows and tick cycles. Its row snapshot is limited to persisted task, pool, and pump metadata. Selecting a row tails the chosen raw slot or pump log, whose contents are not redacted. **vivi report** reads synchronized attempt data and pump receipts; missing historical timestamps remain unavailable rather than being treated as zero. See [monitoring and reports](monitoring-and-reports.md) for filters, formats, and measures.
