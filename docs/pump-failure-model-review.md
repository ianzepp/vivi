# Pump failure model review, 2026-10-26

**Status**: draft. A design review for discussion. It settles nothing.

Read against `vivi/main` at `a4ec303b`, the live vivi pump on hub (vivi
22.0.0, exception window ending about 21:00 UTC), and the hub session's
day review for 2026-10-26. File references are under `src/pump/` unless
they say otherwise.

## Summary

The pump works. The failures it had today come from five gaps in its
design, not from the individual bugs that were patched.

1. **A failure has no class and no cause.** Every failed job collapses
   into the edge word `failed`. The only thing stored with it is
   `{"job": id}`. A 429, a lost job, a format error and real bad work all
   look the same to whoever reads the flow next.
2. **The only recovery is a new run, and a new run throws work away.**
   Every restart entry except `rework1` moves the need branch to
   `vivi/need-prior/*` and rebuilds from main. Any reply on the thread
   also restarts a live run at plan.
3. **The decider can't see anything.** Head-cto gets the verdict history
   but no failure causes. From run 3 it must name a `new_fact` or lock,
   so it locks. All five live locks in the current window give the same
   reason: "the brief omits…".
4. **The runtime record can't be queried.** The real per-job record
   (model, pool, outcome, detail, timings) is an untyped JSON blob in
   `meta['jobs']` that nothing reads. `step_jobs` holds ids only. Log
   lines have no date.
5. **Seat contracts contradict themselves in places.** Hands are parsed
   more strictly than other roles. Bridged seats are told to reply over
   MCP, but the pump reads only stdout. Bands always pick their first
   model.

There is one more gap, in the self-fix loop. **Code the pump lands is
not code the pump runs.** Hub runs 22.0.0, installed at 18:04. At least
two fixes landed after that are not live:

- c6634772 (attempt records in the CTO brief).
- 77d8323e (unique prune ids). The pump still raises `prune-046bb15e…
  already names different work` every few minutes.

So every pump fix only takes effect at the next release and install.

## How a need moves today

`flows/build.mmd` is the design. The code differs from it in these
places:

| Diagram says | Code does |
| --- | --- |
| Start at research | Fresh needs and the unlock default start at **plan** (`flow/load.rs:82`, `flow/unlock.rs:189`) |
| Parked: a newer reply restarts at plan | **Any** non-pump reply newer than the run's start restarts **any** live build run at plan, unless the run is locked or merging (`flow/thread_change.rs:1-9,100-126`) |
| CTO → New run: slice | Can't work. The slicer reads a plan only from the current run (`flow/build_front/slicer.rs:50-60`), so the step fails to start and is held after 3 ticks. A need-wide reader exists (`flow/verify_inputs.rs:149`) and is unused here |
| CTO → New run: rework, "on the existing need branch" | The rework seat is checked out on **main** (`flow/rework_step.rs:46`, `flow/step_inputs.rs:138-141`). Only one verify pass follows |
| (not shown) | Every non-rework restart renames the need branch to `need-prior/<need>-r<old>` and deletes it (`flow/need_branch.rs:175-196,281-404`) |

Today's numbers fit this exactly. 22k lines were written on need
branches and 8.3k landed. There were 21 CTO locks, 18 unlocks and 7
thread replies, and almost every one restarted at plan.

## Failure classes: what happens now, and what should happen

| Class | Example today | Code path now | Result now | Should be |
| --- | --- | --- | --- | --- |
| **Provider** | OpenCode quota 429, Z.AI 1302, overloaded | No rule matches 429, rate limit or quota (`envfail.rs`). Role-step exit codes are not checked, so an empty reply enters the correction loop as a format error (`flow/collect.rs:88-92`) | 2 corrections, then `failed`, then CTO, which locks for "missing evidence". A CTO job that hits a 429 locks the need itself (`cto: lock, failed`) | Retry the same step after a backoff, with a pool-level cooldown for that provider and model. It does not use an attempt, and it never reaches CTO |
| **Infra** | Lane refused fetch-by-SHA, `index.lock`, tool missing | `envfail.rs:41-48` → `flow-env-failure` + operator-hold (`flow/collect_step.rs:238-265`). The rung path ignores endpoint and provider markers (`jobs/rung_result.rs:93-132`) | Waits for the operator, with no retry | A bounded retry with backoff, then a hold. The hold says why |
| **Format** | Prose before `+++`, `[hand]` table header | Corrections (`MAX_CORRECTIONS=2`) use a one-line grammar and no example (`envelope/correct.rs:67-79,136-173`). The hand path skips the preamble tolerance (`flow/slice_dispatch.rs:594`) | `flow-envelope-failed` only after both fail. A rejection that a correction fixes is never counted | Tolerate the same things for every role. Give corrections an example. Count every rejection. When corrections run out, try once more on another model before calling it a work failure |
| **Budget / lost** | `agent_rc=124` after 1800 s, lost slot | One rerun at a raised budget, or `-a2` (`flow/collect.rs:96-127`) | After that, `failed` → CTO, with no cause | Same retries, but an exhausted budget arrives at the decider as "timed out at N s", not as an unexplained failure |
| **Work** | Failing tests, audit block, merge conflict | Flow edges | Correct | Unchanged. This is the only class that should spend a decider's judgment |

`transient.rs` already implements a host, timeout and conflict
classifier. `DispatchConfig.transient_max` and `transient_backoff`
already exist. The live flow uses neither: the only caller is the
legacy split planner (`collect/split_expiry.rs:8`), and
`routing/migrate_keys.rs` labels those keys "not read by vivi 21".

## The five gaps in detail

### G1. Failure has no class and no cause

- `flow/collect_step.rs:170-181` advances every failure with
  `json!({"job": job.id})`. The close letter carries only the word.
- `envfail.rs` is the only classifier, and it knows two kinds
  (ToolMissing, HostSetup). Its result goes only to operator-hold.
- The cause does exist: it's in `PoolJob.outcome{state, detail, receipt}`.
  It is dropped at the point where the step advances.

**Direction.** Give one classifier function to every collector. It
returns `{class, cause, detail}`, where class is one of `provider`,
`infra`, `format`, `budget`, `lost` or `work`. Store it with the job.
Route on the class first; only `work`, or another class that has used up
its own policy, becomes a flow edge. The edge's payload carries the
class and cause, not just the job id.

### G2. The only recovery is a new run, which discards work

- `restart()` (`flow/advance/restart.rs:42-129`) supersedes every open
  task and starts a fresh run. That's fine. The problem is what it does
  to the branch.
- `need_branch::restart` keeps the branch only for `rework1`. Every other
  entry moves it aside.
- In-place recovery exists only before a step routes: lost-job rerun,
  budget rerun, corrections, and operator requeue. Requeue refuses a
  step that failed at start ("has no job to requeue").

**Direction.**

- **Keep the branch by default.** Moving merged work aside should be an
  explicit, rare answer (call it `replan-clean`), not a side effect of
  every entry. A replan on a branch that has merged slices should give
  the planner the branch diff and the prior plan.
- **Resume a node in place.** Allow "rerun step X in run R". Infra and
  provider failures that used up their policy, and operator or mind
  unlocks, should land here, not in a new run at plan.
- **Stop auto-restarting on thread replies.** A reply is input to the
  next step that starts, or a park waiting for a decision. It isn't a
  restart trigger. (Needs b67c125b and 56d07dc5 are about who decides.
  This is about not discarding work while someone decides.)
- **Make the entries usable.** Slice should read the need-wide admitted
  plan. Rework, judge and CTO seats should be checked out at the need
  tip, not main. The rework preflight should check that the branch row
  is `live`, not just that a row exists.

### G3. The decider can't see anything

Head-cto's brief (`flow/cto_step.rs:71-126`) has verdict history,
key-reply front matter (only for admitter, auditor, judge and cto),
failing rung output, the block-ship audit, and slice attempt notes.

It does not have:

- why a step failed;
- exceptions for this need;
- correction history;
- role-job receipts and stderr;
- the model and pool used;
- planner, slicer and repairer replies.

The run-3 `new_fact` rule (`flow/collect.rs:147-156`) then forces a lock
whenever the facts are missing. Live evidence from the current window:

- 56d07dc5 r1: "The brief omits the need definition, admitted plan,
  escalation reasons, and failure diagnostics."
- 0164a429 r2: "agent_rc=124 after 1800 seconds … The revised plan and
  the original failed-build diagnostics are absent."
- abf4884a r3, b67c125b r3 and 7743bf04 r3: the same complaint.

Patching the brief one field at a time is what c6634772 did, for slice
attempts only. Each patch fixes one slot.

**Direction.** Build the CTO brief from the typed job record (G4) for
every step in the need's runs:

- for each failed step: its class, cause and receipt tail;
- the need's exceptions;
- correction errors;
- the admitted plan.

Once the brief carries causes, the `new_fact` rule becomes reasonable.

### G4. The runtime record can't be queried

- `step_jobs(job, need, run, node, task, attempt)` is unchanged
  (`store/flow_index.rs:54-61`).
- The real record is `State.jobs` serialized whole into `meta['jobs']`
  (`store/state_rows.rs:35`). Terminal jobs are never removed, so the
  blob grows without bound.
- `rung_runs` is created but nothing writes or reads it.
- `log.txt` and `exceptions.txt` use `%H:%M:%S` with no date
  (`host.rs:816-821`). The exceptions **table** is the only dated copy.
- The proxy hardcodes `client: None` (`src/proxy/server.rs:221`), so a
  token can't be tied to a job.
- Slot logs are deleted at collect. Only 256 KB of stdout and 4 KB
  stderr and run-log tails survive, inside a letter.
- No operator command or MCP tool lists jobs or attempts. `flow show`
  shows steps but not jobs.

**Direction.**

- Move the job record into typed pump.db columns: role, model, pool,
  dispatched, finished, state, class, cause, receipt tail, and rejection
  count. Retire the blob.
- Add one reader as both a CLI command and an MCP tool, for example
  `pump jobs --need <h>`. Head-cto, the mind and the operator all read
  the same thing.
- Date the log lines.
- Pass the job id as the proxy client.

This stays in pump.db. It is runtime state, as the operator ruled; the
event-letter idea (1ff84c2f) stays withdrawn.

### G5. Contract contradictions

- **Hand strictness.** Hands use strict `parse::<Hand>`. Other roles get
  `after_preamble` tolerance (`envelope/roles.rs:124-169` against
  `flow/slice_dispatch.rs:594`).
- **Bridge contradiction.** The bridged standing prompt says to report
  with `reply` (`src/pool/role_dispatch.rs:352-356`). The collector reads
  only `agent.out` (`src/pool/collect.rs:41-46`). A seat that obeys the
  prompt fails.
- **Bands.** `pool/bands.rs:55-77` always picks the first entry, and the
  code says there is no failover or health tracking. That's ecf3a569, and
  it's also where provider cooldown (G1) would sit.
- **Corrections lack an example.** The charters ship one; the correction
  prompt doesn't.

### Self-fix loop

- **Landed is not deployed.** Decide how pump fixes go live on hub. One
  option is the current way: an operator release, then an install.
  Another is a hub-only "run vivi/main for the pump" channel, with the
  operator still tagging releases. This is the operator's call. Until it
  is made, expect to need a release after each batch below.
- **The queue grows itself.** Slice follow-ups are filed as needs
  automatically (10 in the current window). The mind files audit
  findings as needs to the operator (5 open). A pump that is still
  unstable should file these as wants, or as one triage letter, not as
  dispatchable needs.

## Open needs against this review

| Need | Fits | Suggestion |
| --- | --- | --- |
| ced586d0 failed or parked step resumes | G2 | Fold into the G2 need |
| 3bcf5815 rework from need branch, keep merged work | G2 | Fold into the G2 need |
| c6634772 attempt records in CTO brief | G3 | Landed, not deployed. G3 replaces it |
| 850f0726 log every envelope rejection | G1/G4 | Fold into G1/G4 |
| 0164a429 back off rate-limited providers | G1, G5 | Fold into G1. Now locked |
| ecf3a569 bands spread across models | G5 | Keep, after G1 |
| f540ea4e seats bootstrap themselves | G5 | Keep. The bridge contradiction must be fixed first |
| b67c125b, 56d07dc5, abf4884a: decisions go to the mind | decision routing | Separate topic. Hold until G1–G3 make the decisions informed |
| f1aa4d4c, 7743bf04, 5b82843d, 6a9dad1f follow-up | bugs | Keep as they are |
| auto-filed follow-ups and audit findings | queue growth | Triage by hand. Don't dispatch them blind |

## Order for letting the pump fix itself

The pump will fix itself using the machinery being fixed, so the order
should cut waste first:

1. **G1, classify and back off.** Provider and infra failures stop
   reaching CTO, and a 429 stops locking needs. This also stops the
   correction rounds caused by empty replies.
2. **G4, typed job record and one reader.** G3 needs it.
3. **G3, CTO brief from the record.**
4. **G2, keep the branch and resume in place; no restart on thread
   replies.** This is the largest change. It is safer once restarts are
   rare (1) and decisions are informed (3).
5. **G5, contract fixes.**

Each item should be a small need dispatched alone at high priority, and
released before the next one where it matters. Until G2 lands, unlock to
`rework1` wherever merged work exists, and don't reply on a live need's
thread.

## Questions for the operator

1. How should landed pump fixes go live on hub: an operator release each
   time, or a pump-only channel that tracks `vivi/main`?
2. Should a thread reply ever restart a run automatically, or only park
   and wait for a decision?
3. Should G1–G4 be one design need (one coherent change to how jobs are
   collected and recorded), or four needs in sequence?
4. Do auto-filed follow-ups stay needs, or become wants until someone
   triages them?
