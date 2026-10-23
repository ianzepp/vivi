# Pump protocols

This reference describes the task and receipt contracts used by the current durable pump path. The pump creates and persists role jobs before selecting a pool. The pool runs the task and returns a receipt; the controller validates and applies it. The pump tick does not launch planner processes or perform the live merge and checks in a local merge thread.

See [pump contract](pump-contract.md) for control, role routing, lifecycle, and restart behavior.

## Job dispatch and identity

Each durable job has a stable ID, kind, target where applicable, recipient role, candidate pools, brief, and collector payload. Before dispatch, the pump writes a task brief containing the stable markers **pump-job-id: &lt;id&gt;** and **pump-job-kind: &lt;kind&gt;**. The task subject contains **[pump-job:&lt;id&gt;]**. Repeated queue attempts with the same ID and identical inputs are idempotent; reusing an ID for different inputs is an error.

The pump dispatches pending jobs using the current role routing and pool fallback chains. A job with no eligible free slot remains pending. The pool receipt is collected on later ticks and applied once. On restart, the pump searches the addressed role's tasks for the stable marker, reattaches its task handle, and reads the existing result instead of sending duplicate work.

## Planner receipts

Board planning, need revisions, and unit splits use the active pump-planner role and its pool; its charter is the planner's system prompt. The pool worker cannot write files in the pump home. It returns the full plan JSON in the task receipt between these exact marker lines:

**PUMP-PLAN-BEGIN**
**&lt;complete plan JSON&gt;**
**PUMP-PLAN-END**

The pump writes that JSON to the expected file under **plans/**, writes the compatible exit receipt, and sends both through the normal plan validator and ingester. A worker exit without a usable plan is recorded as a failed planner attempt and follows the planner retry or stuck rules. There is no active local command or process-reaper protocol for planners.

Plan JSON contains **need**, **verdict**, **reason**, and **units**. Each unit has a unique **id**, **brief**, non-empty **write_scope**, and a **proof** list; **title**, **target**, **after**, and **tools** are optional where configuration permits. Revision plans include **revision** and may include **withdraw**. Split plans include **replaces** naming the replaced unit. A split result is re-asked once after validation failure; the second invalid result parks the unit as stuck with a split-plan-rejected exception. The rejected result is moved under plans/rejected.

## Merge result receipt

A merge job's brief names its target, integration ref, unit sources, completed role-job sources, and runner scratch branch. The worker starts from the integration tip, diagnoses conflicts and failing checks, repairs them within write scope when possible, integrates every listed source in order, runs the configured formatter, shared checks, matching scope checks, and unit proofs, then commits the candidate. It must not push.

The receipt ends with one minified JSON line prefixed **PUMP_MERGE_RESULT=**. The top-level object contains **version: 1**, **target**, the full candidate **tip**, **sources**, integer **shared_check_timeouts**, integer **shared_check_timeout_retries**, and **shared_check_recoveries**. Include the integer metrics and recovery array even when they are zero or empty. The timeout count covers every timed-out check execution; the retry count covers additional attempts after an initial timeout. Each recovery entry has **name**, **command**, and **detail** for a failed check whose immediate retry passed. Every requested source appears exactly once. A unit source has **kind: "unit"**; a completed role branch has **kind: "job"**. Each source includes its **id**, **status** (**merged**, **dropped**, or **undecided**), and optional **why**, **detail**, **outside**, and **secs** fields. A dropped or undecided source must give a reason and detail. An omitted, duplicate, unexpected, or mismatched source invalidates the receipt.

**merging** unit state is owned by the durable merge job. The controller rejects a merge result if the worker did not exit successfully or its source list does not match the dispatched job. An isolated source failure is reported against that source; infrastructure uncertainty remains undecided for a retry rather than consuming a hand attempt.

A failed shared check is diagnosed and retried exactly once on the same tree. A timeout (exit 124 or timeout-wrapper kill) is infrastructure uncertainty, not a source defect. A timed-out check whose retry passes is recovered and recorded. If a timeout is not followed by a passing retry, report every source as **undecided** with **why: "shared-check-timeout"** and detail naming the command and timeout. Do not treat a timeout as a source defect or create a repair unit. A non-timeout failure still present after its retry, and that cannot be repaired within write scope, may isolate the defective source as a last-resort diagnosis. Any solo check that times out during isolation is **undecided** with **why: "bisect-check-timeout"**, never a clean result or culprit. Timeout alone never opens quarantine repair work.

## Need acceptance receipts

Managed need audits are ordinary durable jobs sent to the active Auditor role
and its pool fallback chain. Their brief names the durable job ID, the full
frozen binding (**need**, **target**, **revision**, **base**, and **tip**), the
review start **audit_base**, and the round. The first review uses the original
base; a repair-only review starts at the previous frozen tip while retaining
the original binding base.

The worker ends with one minified JSON line prefixed
**PUMP_NEED_AUDIT_RESULT=**. Its fields are **version: 1**, **receipt_key**,
**job_id**, **binding**, **source_hash**, **audit_base**, **round**, **verdict**,
**requirements**, **findings**, **diagnoses**, **blind_spots**, and **report**.
Each requirement names its original **id** and **text**, a status
(**SATISFIED**, **PARTIAL**, **MISSING**, **UNVERIFIED**, or
**NOT_APPLICABLE**), and evidence. Each finding names an **id**, **content**,
affected **paths**, and a **recheck_trigger**. Report provenance contains a
stable **id** and **reference**; the controller adds task, pool, and collection
time from the durable job. The controller validates every identity field and
the complete requirement set against the saved job and current frozen range.
An invalid, stale, contradictory, or incomplete receipt cannot accept the
need. The **source_hash** is the v3 canonical hash of the root need and its
source amendments, excluding only authenticated pump-generated replies. It is
part of the durable job identity and is recomputed when the receipt is
collected. A later source amendment makes the old receipt stale even when the
plan revision, need branch, and frozen code tip are unchanged.

The supported verdicts are **clean_pass**, **residual**, **block_range**, and
**block_ship**. Clean and residual receipts can accept only the exact current
range. Residual findings become deduplicated ordinary needs with their report,
path, and recheck-trigger provenance. A block-range receipt creates repair
units on the same need branch and queues a new audit over only the repair diff.
**[pump.need].repair_rounds** (default 1) caps how many repairs a finding may
receive before the branch is held.
A block-ship receipt records a persistent hold and escalation. The Auditor
charter remains the authority for verification width and repair-round policy.
Receipt replay is a no-op; changed revision, branch tip, or frozen membership
invalidates prior acceptance.

Need landing uses a durable **need-merge** job with a need-specific
**PUMP_MERGE_RESULT=** receipt shape. The predefined Merge role reports
**version**, **target**, **need**, **revision**, **base**, accepted **tip**,
**candidate**, **status**,
each required check, and the acceptance source. The worker does not publish.
The privileged pool runner publishes only to the configured integration ref;
the controller requires the worker receipt, runner publication receipt, exact
ref and candidate, and independent proofs that the accepted tip is an ancestor
of the candidate and that the candidate is an ancestor of the observed target.
Only then does it record landing and queue a separate durable cleanup job.
Cleanup can delete only the recorded need branch at the accepted tip; the
runner's compare-and-delete receipt and an independent ref-absence check must
both succeed. A cleanup failure preserves the branch for retry. The integration
gate and main promotion path remain independent of need acceptance.

## Privileged publication and verification

For an integration merge, the pool dispatcher passes a publish ref to the pool runner. The runner-only **PUBLISH_REF**, **PUBLISH_EXPECT**, and **PUBLISH_REMOTE** settings are not exposed to the agent. After a clean agent exit, the privileged runner reads the candidate commit, checks the requested ref and optional exact expected SHA, performs a plain push, fetches the ref back, and verifies the remote commit and ancestry. The collected pool summary records **runner publish: &lt;state&gt; &lt;ref&gt; &lt;candidate-sha&gt; &lt;remote-sha&gt;**.

The merge controller requires a successful runner publication receipt for the configured integration ref, requires its candidate SHA to match **PUMP_MERGE_RESULT**, then independently fetches the integration ref and verifies the remote SHA and candidate ancestry before it marks units merged. A worker's own JSON claim is not proof that the remote branch moved.

Promotion jobs set the runner-only publish ref and exact expected SHA to the configured main ref and gated commit. After the job finishes, the controller requires a successful worker exit, checks that the job was configured for that exact ref and SHA, then reads the remote main ref and verifies that it contains the gated commit. A runner publish receipt may provide additional evidence, but the independent remote ancestry check is authoritative. A failed worker or missing remote ancestry is not recorded as a successful promotion.

## Gate and audit receipts

A gate job is read-only and pinned to a single integration tip. The worker runs the configured gate command and reports results between **FAILING-BEGIN** and **FAILING-END**, one fully qualified failing name per line. Use **BUILD-FAILED** when the build itself fails. Use **TARGET-DIED &lt;target&gt;** when an entire test target dies without a test summary. Use **ENV &lt;crate&gt; &lt;test-name&gt; :: &lt;missing tool&gt;** for failures caused only by missing executables or tools. An empty marker block reports no failing tests. Unreadable reports are recorded as gate exceptions and force another gate.

A read-only auditor reply includes an **AUDIT-BEGIN** through **AUDIT-END** block containing a JSON array. Each root-cause object supplies **root_cause**, a complete fix-unit **brief**, covered **tests**, **write_scope**, and **proof** commands. The pump validates the report and creates one fix unit per accepted cause. Invalid or failed audit receipts remain open for retry or operator attention.

An optional external gate runner can provide a result file for a pinned tip under the configured results directory. The pump consumes valid results through the same gate collector. It waits for such a result only while the runner heartbeat is fresh; otherwise it queues the read-only gate role job. The external file format and timing settings are described in the pump configuration section of the Vivi skill.

## Other role jobs

- **Promotion** jobs ask the release role, or merge role when no release role exists, to confirm the exact gated tip. The runner alone can publish it to the configured main ref.
- **Cadence** and **digest** jobs are addressed to the role whose schedule is due. Cadence and digest jobs always run read-only; their output is mail (wants and needs).
- **Investigation** jobs are read-only auditor jobs created for head-ceo needs marked **investigate** and containing **for: &lt;want-handle&gt;**. The need closes only after the recent auditor report reply lands on that want for head-ceo.
- **Reclaim** jobs are local-only cleanup jobs sent to the reclaim role or the hand role. They cannot spill onto remote pools.

## Retired tooling

The old deterministic in-process merger, **REMOTE-MERGE** flag, and **merge-jobs/** file-claim protocol have been removed. A source contract test fails if they return. Detached planner **.out.rc** reaper files are not part of the active planner path. Detached **sync push --drain** is mail transport, not job execution. Unit attempt branches are published by the privileged pool runner (`--publish-ref`); the pump does not `git push` those refs. Hand jobs may request that publication only under `refs/heads/vivi/unit/`. Merge jobs may publish assigned need or integration refs; release jobs may publish an exact expected SHA to main.
