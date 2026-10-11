---
name: speedboat
description: Execute a Vivi backlog with fast in-band models, small parallel Hand assignments, and minimum verification. Use when the operator chooses Speedboat or asks for break-fast, ship-fast delivery instead of Tugboat's planning and audit cycle.
---

# Speedboat

Optimize for useful changes landed per wall-clock minute. A 90%-good change
in five minutes is preferable to a 99%-good change in forty-five minutes.
Accept rough edges, limited verification, and concrete repair work. Finish
the requested scope. Never turn limited confidence into a claim of certainty.

**Once the requested change exists and its assigned check passes, stop
improving it and hand it over.**

## Operating contract

Speedboat is a separate protocol over the same Vivi backlog as Tugboat.
Selecting Speedboat selects this workflow for the admitted work:

```text
Operator -> Mind -> parallel Hands -> Mind integrates -> next small units
```

The Mind reads goals and phases, slices work, dispatches, integrates, and
grades turnaround. Hands implement one small unit each. The Mind owns
commits by default. Hands do not commit unless the operator authorizes it.

Replace Tugboat's Planner pass, delivery audit, automatic Auditor/CTO
reviews, paired verdicts as routine gates, phase freezes, warm-boot
reacquaintance reviews, and repeated merge handoffs. Do not import these
requirements through an old role charter. No automatic broad verification
jobs, cadence loops, polish passes, or hardening phase.

Keep settled architecture decisions, the full campaign completion contract,
foreign-work ownership, Git lock serialization, and authorization boundaries.
Keep any acceptance condition explicitly required by the operator. A broad
test ladder or release gate is Mind-owned integration work after the small
Hand changes land; it is never a Hand's finishing chore. Run the requested
gate rather than treating the Hand check limit as a reason to omit it.

## Tools and models

Use Vivi for assignments, dependencies, decisions, and receipts. Load the
Vivi skill for CLI semantics and check installed command help before relying
on exact flags. Use `--project <root>` for the intended mailspace. Do not
inspect or mutate Vivi's underlying stores.
Resolve that mailspace root once before dispatch (it may be the workspace
container rather than the Git repo) and include the same `--project` in every
Hand command and receipt.

Resolve every spawn through the canonical `model-availability` skill, by
**band**. A band is a model at an effort, not a model alone: the same model at
low and at xhigh lands in different tiers, so do not pin Hands to a thinking
level. Use Tugboat's role bands: Mind P1, Hand P2, Planner P1, Auditor P1,
Head P0. Set `band:` in the brief only when the unit is not the role default:
`band: P1` for a complex or architectural unit, `band: P2` for a Planner on a
low-complexity requirement, `band: P0` for an Auditor on a complex range.
Speedboat adds one preference inside the band: take the fastest healthy slug
(prefer S0), and do not seat a deeper band than the unit needs. Favor latency
and useful turnover over marginal answer quality. Do not inherit the Mind's
model or effort accidentally. Record the actual slug and effort selected. On
the Claude harness the spawn takes a model but no effort: effort comes from the
agent type, so a band is a choice of agent type (see `model-availability`
`references/claude-harness.md`).

Escalate only for a named obstacle the Hand could not resolve: retry the unit
at the next deeper band (`band: P1`), and give that seat the obstacle, not the
whole goal. Availability and provider restrictions still apply. If the harness
cannot select the requested model or effort, disclose the limitation instead
of claiming it did.

Hands need shell, Git, Vivi, editing, and narrow-check access. Use a
tool-capable agent type. An active assignment is a Vivi handle plus a live
process; an open handle alone is not running work.

## Mind loop

1. **Orient once.** Read `vivi boot`, live harness state, and repository
   status. Read the selected goal or phase and the code relevant to the next
   edit. Treat prose as scope and intent; code establishes what exists.
2. **Pick one small outcome.** Name a concrete behavior or edit, its files,
   immediate dependencies, and the cheapest useful check. Do not commission
   a Planner or research the entire goal before dispatching the first unit.
3. **Fill useful capacity.** Dispatch every ready, independent unit for which
   a write scope and integration path exist. Keep planning in the Mind brief
   and local to the next burst. Do not prepare a novel of future assignments.
4. **Integrate continuously.** Read the returned diff and receipt once for
   scope, obvious defects, and the declared result. Commit the intended paths
   or integrate the isolated lane. Do not replay the Hand's check without a
   new code change or a concrete contradictory result.
5. **Settle and continue.** Close the unit only after its done condition and
   required landing are satisfied. Apply the Vivi completion receipt using
   the installed CLI's lifecycle. Start eligible successors immediately.

Take the first reasonable implementation consistent with settled decisions.
Route an actual missing decision or observed failure immediately. Keep
unaffected units moving. Do not request a review merely to increase confidence.

Parallel writes must have disjoint scopes. Serialize a shared hot file or
isolate overlapping work in worktrees. Never erase another agent's changes.
Mind Git mutations are serialized; do not run competing staging or commits.
Reuse the project's integration mechanism without its optional review chain.

Keep a unit active while its diff awaits integration. A Hand's readiness
receipt is not a landing. Dependents requiring landed code must not unlock
on an unmerged diff. If existing project topology separates implementation
and merge handles, retain that dependency but let the Mind perform the
integration directly; do not add another merge Hand by default.

## Assignment size and dispatch

Size for a Hand to implement, check, and return in **under five minutes**.
One unit is one concrete change with enough nearby work to make it useful.
Avoid both whole-theme assignments and tiny edits whose paperwork costs more
than the implementation. Split at behavior, interface, or file boundaries.
Before dispatch, inspect the failure's exact file paths and give the Hand all
needed write paths and external log read paths. A missing companion file or
log scope burns the Hand's short budget on clarification.

Put the assignment in Vivi before spawning. Use a short body with these fields;
`done_when:` and `write_scope:` begin their own lines for Vivi's work graph:

```text
protocol: speedboat
goal: <parent goal/phase/need>
change: <one concrete outcome; likely edit and immediate caller>
done_when: <outcome, assigned check, and required landing>
write_scope: <allowed repo and paths; forbidden paths or roots>
check: <one exact scoped command, or inspection with a reason>
budget: <5 min target; stop at 10 min>
band: <optional; only when the unit is not the role default, e.g. P1>
return: diff, check result, elapsed time, known failures or blocker
```

Use graph dependencies for real prerequisites and activate the task at
dispatch so it is not offered twice. Record dispatch time and the live
process binding on the assignment. Send the Hand a pointer to this skill,
its role charter, and its Vivi handle. The task holds the brief. Say explicitly
that Speedboat replaces the charter's Tugboat planning and review procedure.
`vivi task send` may print both `created <role> <handle>` and `sent <handle>`.
The **created task handle** is the graph node and the Hand assignment; the
`sent` handle is a delivery record. Activate and later close the created
handle. Do not try to add it as a new graph node: sending the task already
places it in the backlog graph.
Keep control-plane output bounded: `vivi graph activate` can print the whole
ready frontier, so extract only the activated task's result from structured
output or cap the displayed response.

Do not bind an incomplete subset of units to a parent need whose automatic
join would then falsely close the whole need. Keep the full goal open until
every admitted requirement has landed. Small units sequence scope; they do
not reduce it.

## Hand procedure

You are judged on the speed of useful results, not the perfect solution.

1. Read this skill, your assignment, and the assigned files. Inspect immediate
   callers or definitions only as needed to make the edit.
2. Choose the first reasonable approach and edit. Do not survey alternatives,
   trace unrelated architecture, polish nearby code, or reread the whole goal.
3. Run the assigned check once. If it exposes a local defect, fix that defect
   and repeat the same check. Do not broaden verification after a failure.
4. Return the diff and a short receipt through the same handle. Stop. Let the
   Mind integrate and settle the unit; do not mark unlanded work complete.

If substantial research is necessary, return the precise missing fact or
decision immediately. If a fix exceeds the unit, return the observed failure
and partial diff. The Mind splits or routes a concrete successor. Successive
agents advance the artifact; they do not repeatedly reconsider it.
For read-only diagnosis, stop at the first code-backed causal path and name
the next repair seam. Do not spend the time budget surveying adjacent plans
or decisions once the failing behavior is explained.
Ask for Git history only when the live code and tests leave the intended
contract in conflict; otherwise history expands a short diagnosis past its
budget without changing the repair route.

## Hand minimum verification

The Mind assigns **one cheapest useful check**, not a check ladder.

- Prefer `cargo check -p <crate> --lib` over `cargo build`. Select the actual
  affected target when it is not a library.
- When behavior needs a test, run one fully qualified test in one named
  crate and target, with `--exact`. A filter matching a whole module is a
  suite, not a specific test. Confirm that the intended test actually ran;
  zero tests is not a pass.
- Never run a broad test suite, broad build, bare workspace check,
  `--workspace`, or `--all-targets`. Do not run both a build/check and a test
  as an automatic Hand finishing ritual.
- For a trivial change, the Mind may assign inspection alone and state why.
  New behavior should receive a focused test when that fits the unit.

Read existing timings before choosing a command. Estimate and bound any
command likely to exceed 30 seconds; capture actual elapsed time. Its timeout
must fit both the remaining assignment budget and the five-minute command
ceiling. A scoped command can still trigger costly compilation or lock waits.
Do not clean caches, rebuild everything, or increase a timeout to hide this.
Report the cost and route a concrete performance fix.

Repeat the same check only after a relevant edit or a specific transient cause
has been cleared. No second confidence pass. Never weaken a test or bypass an
invariant to get green. A failed, timed-out, or unrun assigned check is an
incomplete unit, with an honest failure receipt and repair work. Partial code
may be preserved or committed as WIP without claiming the goal is complete.

## Mind acceptance gates

After integrating the Hands, run any ladder or e2e proof the operator
explicitly requested. Split it by stage or target so each command stays
under the project's five-minute command limit. State estimates for longer
runs, use bounded timeouts, and record actual elapsed time. Preserve raw
failure logs on red and name their paths in the receipt; a summary count
alone is insufficient to route the next repair unit. Rerun the affected
gate after a fix, then continue to the next required gate.
Before settling a campaign, recheck every admitted unit's exact `done_when`
against live code and executed receipts. For each named fixture or parity
oracle, confirm the file exists, the owning gate invokes it, and the current
receipt shows it ran; a broad green ladder does not replace a missing unit
proof. When adding a script to a stage, update every source-closure or static
coverage manifest that inventories stage contents, then run the owning
inventory gate. Keep missing proof pending and the campaign active instead of
trusting stale ledger rows or old summaries.
Before a broad acceptance run, identify the exact integrated source revision
and the warmth of each available checkout's own target. When a clean campaign
packet has the integrated commit as an ancestor and its target is already warm,
fast-forward that packet to the integrated commit and run the ladder there;
record the exact revision in every receipt. Do not make the first broad gate
pay a cold main-checkout build when an exact-source warm packet is available.
Never copy or share target artifacts between checkouts. Keep a timed-out cold
run as its own finding, and continue acceptance in the synchronized warm
checkout instead of retrying the cold command unchanged or raising its bound.
During packet-based work, use that exact-source warm packet for focused
regression rechecks after integration too; do not trigger a cold main-target
compile solely to validate code that will be accepted in the packet.
On macOS, resolve the interpreter selected by each `#!/usr/bin/env bash`
script before invoking a gate. System Bash 3.2 mishandles expansion of an
empty array under `set -u`; when the script uses that pattern, invoke it
explicitly with Homebrew Bash instead of counting an interpreter failure as
a product-test result. A Bash 5 wrapper does not change a child script's
`env bash` lookup; put Homebrew's bin directory first in the child's `PATH`
or invoke the child with the explicit interpreter too.
If a pooled log names a failed test but omits its panic detail, rerun that
exact test with uncaptured output in the same isolated checkout and retain
the focused log.
If an aggregate gate reports failing paths without the underlying compiler
or tool messages, preserve its owned temporary output (when the harness
supports it), reproduce only that failed stage against the preserved tree,
and retain the raw diagnostic before changing code or an outcome ledger.
Remove only the exact temporary tree created for that reproduction after the
diagnosis is complete. A path-only failure is not enough evidence to add an
expected-failure row.
When the campaign changes the corpus, reconcile every newly added exemplar
against each enabled target's applicable expected-outcome ledger before
closing the E2E matrix. Failure classifications are target-specific: inspect
the failing stage's compiler or tool diagnostic for each lane, and do not
copy another target's outcome row as evidence.
If one requested stage times out because it serializes independent required
checks, repair or split that stage's orchestration while preserving every
check and failure status. A green isolated tail check does not make the timed
out aggregate green; rerun the aggregate after the scheduling repair.
When splitting an overlong stage, give every child proof a selectable,
failure-fatal ladder step and make the parent stage select it. Preserve the
old full-stage behavior; a new standalone command must not become an orphaned
check. Put owned scratch logs under a stable task/repository evidence path
before applying a timeout, since system temporary directories may disappear
after an interrupted process. Do not point `TMPDIR` inside a Cargo workspace:
tests use it for generated Cargo projects, which then inherit the enclosing
workspace and can fail for reasons created by the test invocation. Use a
stable scratch directory outside every affected repository root, or a
harness-specific log path that leaves `TMPDIR` unchanged.
Before a stage near the five-minute cap, inspect the live runner and latest
receipt for nested checks and serial waits. Estimate the critical path,
including required post-check work; overlap independent legs with separate
logs and exit results, or split them into bounded proofs before launch when
the combined path is too close to the cap. Do not spend a full timeout to
discover that the stage contains another long gate.
For E2E targets with multiple stages, default to one target-stage command at
a time unless a receipt for the exact current revision and power/cache state
shows the complete target will finish comfortably below the cap. A historical
whole-target duration does not establish that: inspect the current stage's
serial fan-out and all work after it. During a long run, compare live progress
with remaining time and switch to selectable stage commands before the cap
when the full target cannot finish. If a single stage itself exceeds the
bound, repair or split that stage while keeping every check failure-fatal.
For golden or other corpus-wide stages, inspect the per-case loop and corpus
item count before choosing a bound. Run one representative focused case and
estimate the serial critical path from its measured cost; do not infer that a
corpus walk is short from a fast single case or an old aggregate receipt. Long
walks must emit useful per-case progress often enough to distinguish active
work from a lock or startup stall. If the estimate approaches the command cap,
parallelize independent cases or split them into selectable, failure-fatal
ladder steps before launching the full walk, while preserving result ordering
and every existing floor/failure check.
Trace the caller through its final success return, including commands after
helper calls and branches reached only when earlier checks pass. A red run
may return early and skip required work, so record which checks ran and rerun
the whole accepted stage after fixing the first failure.

## Clock and Mind grading

Measure real wall time from dispatch to a returned, integration-ready diff
and receipt. Include startup, research, editing, checking, and paperwork.
Track dispatch-to-land time separately so integration delay stays visible.

| Hand wall time | Grade | Required response |
| --- | --- | --- |
| Under 5 minutes | Excellent | Turn over and dispatch the next useful unit |
| 5 through 10 minutes | Mind or Hand made a mistake | Report the cause; narrow later units or remove the bottleneck |
| Over 10 minutes | Unacceptable; Mind performance penalty | Interrupt, preserve work, and split or reroute |

At five minutes the Hand reports what remains and why it is late. It may
finish a concrete last step within the ten-minute limit; it may not start
another research or verification pass. At ten minutes stop the attempt.
The Mind watches deadlines and interrupts overdue workers. Confirm the old
writer has stopped before assigning its scope to a successor.

Keep timestamps, duration, grade, and delay cause in the task receipt. Grade
the Mind from the excellent/mistake/unacceptable counts and the observed
landing delay. Failed or partial attempts are not excellent completions.
Carry the elapsed cost of retries into the parent item's record. Renaming
or splitting a late task does not erase its lateness. Do not create a separate
scoring service or dashboard to operate this skill.

## Receipt and recovery

Keep a receipt short:

```text
result: ready for integration | partial | blocked
change: <what changed; diff paths or lane tip>
check: <command/inspection; pass, fail, timeout, or unrun; actual duration>
time: <dispatch, return, elapsed, grade; delay cause if any>
remaining: <known failure, missing decision, or none>
```

The Mind adds the landed commit and landing time when settling. Report commit
count, short hash, subject, and material verification limits to the operator.
Do not describe unrun broad checks as validated behavior. Keep integration
traffic moving; a growing queue of ready diffs is a Mind bottleneck.

### Worker and tool failure

Treat a worker startup error, provider usage-limit response, or process exit
without a receipt as an execution failure, separate from the code check.
Inspect the live process and exact diff first; preserve partial work, and do
not reassign its write scope until the old writer has stopped. Never retry a
provider/model that returned an explicit usage-limit error. Resolve a currently
eligible fallback through `model-availability`, or take over the small unit
in the Mind. Keep the original assignment open until its result is honestly
settled; a replacement does not reset its elapsed clock.

If a scoped Cargo command times out on a shared target lock, identify the lock
holder and measure the wait. If a broad test build times out while a cold
target is compiling, keep its partial artifacts and log; do not count it as a
test failure or rerun it unchanged in that checkout. When the selected
workflow already has an isolated packet, fast-forward it to the exact
integrated source commit and use its own target for acceptance. Report the
tested commit identity. Never set `CARGO_TARGET_DIR`, link targets, or share
compiled artifacts. Otherwise split the work or record the infrastructure
blocker and move to independent work.

After interruption or compaction, Mind reads this skill, Vivi boot, live
process state, and Git status. Resume existing handles. Do not respawn live
workers, reset clocks, replan admitted scope, or launch reacquaintance audits.
A Hand resumes only its own handle and remaining budget. Continue until the
full admitted scope has landed or a real external/operator dependency remains.
