---
name: speedboat
description: Execute a Vivi backlog with fast in-band models, small parallel Hand assignments, and minimum verification. Use when the operator chooses Speedboat or asks for break-fast, ship-fast delivery. The slower, review-heavy alternative is the sibling tugboat skill.
---

# Speedboat

Optimize for useful changes landed per wall-clock minute. A 90%-good change
in five minutes is preferable to a 99%-good change in forty-five minutes.
Accept rough edges, limited verification, and concrete repair work. Finish
the requested scope. Never turn limited confidence into a claim of certainty.

**Once the requested change exists and its assigned check passes, stop
improving it and hand it over.**

## Operating contract

Speedboat is a delivery protocol over a Vivi backlog. Roles, handles, and
dependencies come from Vivi; this skill defines how work is sliced,
dispatched, checked, and landed:

```text
Operator -> Mind -> parallel Hands -> Mind integrates -> next small units
```

The **Mind** reads goals and phases, slices work, dispatches, integrates, and
grades turnaround. A **Hand** implements one small unit. The Mind owns commits
by default; Hands do not commit unless the operator authorizes it. (In a pool
lane the runner commits for its worker; see "Running Hands in a pool".)

Speedboat has no Planner pass, no delivery audit, no automatic Auditor or CTO
review, no paired verdicts as routine gates, no phase freezes, no
reacquaintance review after compaction, and no repeated merge handoffs. It
runs no broad verification jobs, cadence loops, polish passes, or hardening
phase unless the operator asks. A role charter that describes one of those
procedures does not bind a Speedboat unit: the brief's `protocol: speedboat`
line and this skill govern it.

Keep the project's settled architecture decisions, the full completion
contract for the admitted scope, ownership of other agents' work, serialized
Git mutations, and authorization boundaries; they live in the project's rules
(`vivi rule`) and agent docs. Keep any acceptance condition the operator
explicitly requires. A broad test ladder or release gate is Mind-owned
integration work after the small Hand changes land; it is never a Hand's
finishing chore. Run the requested gate rather than treating the Hand check
limit as a reason to omit it.

## Tools and models

Use Vivi for assignments, dependencies, decisions, and receipts. Load the
[Vivi skill](../vivi/SKILL.md) for CLI semantics and check installed command
help before relying on exact flags. Use `--project <root>` for the intended
mailspace. Do not inspect or mutate Vivi's underlying stores. Resolve that
mailspace root once before dispatch (it may be a workspace container rather
than the Git repo) and include the same `--project` in every Hand command and
receipt.

**Bands.** A role's model preference is a **band**, `P<n>-S<n>`: a quality
tier P (lower is deeper) and a speed tier S (lower is faster decode). A band
names a model at an effort, not a model alone: the same model at low and at
high effort lands in different tiers, so do not pin Hands to a thinking level.
Vivi stores the band as an opaque string in the role's `model` field. The Mind
resolves it to a slug and effort at spawn from the host's model inventory if it
has one, otherwise from the models the project configured (for a pool dispatch,
the pool's `models` allowlist). Never write a slug back into the role.

Default bands: Mind P1, Hand P2. Planner, Auditor, and Head seats are used only
when the operator asks, at their usual bands (Planner P1, Auditor P1, Head P0).
Set `band:` in the brief only when the unit is not the role default: `band: P1`
for a complex or architectural unit, `band: P0` for an Auditor on a complex
range. A bare P keeps the role's S; a full band (`band: P1-S0`) sets both.
Speedboat adds one preference inside the band: take the fastest healthy slug
(prefer S0) and do not seat a deeper band than the unit needs. Favor latency
and useful turnover over marginal answer quality. Do not inherit the Mind's
model or effort accidentally. Record the actual slug and effort selected. On a
harness whose spawn takes a model but no effort, effort comes from the agent
type, so a band is a choice of agent type.

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
its role charter, and its Vivi handle. The task holds the brief.
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

### Running Hands in a pool

A Hand can also run as a pool-dispatched worker instead of a host-spawned
seat. The Vivi skill's work-graph flow has the procedure; the points that
matter here:

- `vivi pool dispatch --pool <name> <task> --model <slug> --from <ref>
  [--branch <name>]` claims a slot and starts the worker in one step. The
  task body is the brief; the worker's standard output is the receipt.
- The runner commits and pushes (or commits a local branch) as the lane's bot
  identity. The Mind integrates the branch and closes the task; `wait` and
  `reconcile` never close it.
- A dependent unit may start from an unmerged base with `--from <base branch>
  --branch <new>`, once the base worker has been collected.
- Remote lanes scale Hand implementation; a local worktree pool with a
  persistent build directory is the place for integration and acceptance.
  Which work goes to which pool is the Mind's call.
- A review seat that needs a full shell but must change nothing runs with
  `--read-only`; its token cannot push.

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

- Prefer the narrowest compile or type check of the affected target over a
  full build (in a Cargo workspace, `cargo check -p <crate> --lib` rather than
  `cargo build`). Select the actual affected target when it is not a library.
- When behavior needs a test, run one fully qualified test in one named
  package and target, matched exactly. A filter matching a whole module is a
  suite, not a specific test. Confirm that the intended test actually ran;
  zero tests is not a pass.
- Never run a broad test suite, broad build, bare workspace-wide check, or
  all-targets run. Do not run both a build/check and a test as an automatic
  Hand finishing ritual.
- For a trivial change, the Mind may assign inspection alone and state why.
  New behavior should receive a focused test when that fits the unit.

Read existing timings before choosing a command. Estimate and bound any
command likely to exceed 30 seconds; capture actual elapsed time. Its timeout
must fit both the remaining assignment budget and the project's single-command
ceiling. A scoped command can still trigger costly compilation or lock waits.
Do not clean caches, rebuild everything, or increase a timeout to hide this.
Report the cost and route a concrete performance fix.

Repeat the same check only after a relevant edit or a specific transient cause
has been cleared. No second confidence pass. Never weaken a test or bypass an
invariant to get green. A failed, timed-out, or unrun assigned check is an
incomplete unit, with an honest failure receipt and repair work. Partial code
may be preserved or committed as WIP without claiming the goal is complete.

## Mind acceptance gates

After integrating the Hands, run any ladder or end-to-end proof the operator
explicitly requested, and only that.

- **Fit the clock.** Split a gate by stage or target so each command stays
  under the project's single-command limit. State an estimate for any longer
  run, use a bounded timeout, and record the actual elapsed time. Before a
  stage near the limit, inspect the live runner and the latest receipt for
  nested checks and serial waits; overlap independent legs with separate logs
  and exit results, or split them into bounded proofs. Do not spend a full
  timeout to discover another long gate inside the stage. For corpus-wide
  walks, estimate from one measured representative case, not from a fast
  single case or an old aggregate.
- **Keep the evidence.** Preserve raw failure logs on red and name their paths
  in the receipt; a summary count cannot route the next repair. If an
  aggregate gate reports failing paths without the underlying diagnostics,
  preserve its temporary output, reproduce only that stage, and retain the raw
  diagnostic before changing code or an outcome ledger. A path-only failure is
  not evidence for an expected-failure row.
- **Rerun, then advance.** Rerun the affected gate after a fix, then continue
  to the next required gate. A red run may return early and skip required
  work, so trace the caller through its final success return, record which
  checks ran, and rerun the whole stage after fixing the first failure.
- **Repair orchestration, not coverage.** If a stage times out because it
  serializes independent required checks, repair or split the orchestration
  while preserving every check and failure status. A green isolated tail
  check does not make a timed-out aggregate green.
- **Prove each admitted unit.** Before settling a campaign, recheck every
  admitted unit's exact `done_when` against live code and executed receipts.
  For each named fixture or parity oracle, confirm the file exists, the owning
  gate invokes it, and the current receipt shows it ran; a broad green run does
  not replace a missing unit proof. Keep missing proof pending and the campaign
  active instead of trusting stale ledger rows or old summaries.
- **Name the revision, use a warm checkout.** Identify the exact integrated
  source revision and record it in every receipt. Run broad acceptance in a
  checkout at that revision whose own build cache is already warm (a local
  pool slot with a persistent build directory is one); do not make the first
  broad gate pay a cold build. Never copy or share build artifacts between
  checkouts. A timed-out cold run is its own finding: continue in the warm
  checkout rather than retrying the cold command unchanged or raising its bound.
- **Scratch logs live outside the repositories,** in a stable task or evidence
  path rather than a system temporary directory that can vanish after an
  interrupted process.

Language- and toolchain-specific acceptance lessons belong in the project's
own agent docs, not here.

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
provider or model that returned an explicit usage-limit error. Resolve a
currently eligible fallback from the host's model inventory, or take over the
small unit in the Mind. Keep the original assignment open until its result is
honestly settled; a replacement does not reset its elapsed clock.

If a scoped command times out waiting on a shared build lock, identify the
lock holder and measure the wait. If a broad build times out while a cold
cache is compiling, keep its partial artifacts and log; do not count it as a
test failure or rerun it unchanged in that checkout. Use an isolated checkout
at the exact integrated commit, with its own cache, for acceptance, and report
the tested commit. Never share or link build caches between checkouts.
Otherwise split the work or record the infrastructure blocker and move to
independent work.

After interruption or compaction, the Mind reads this skill, Vivi boot, live
process state, and Git status. Resume existing handles. Do not respawn live
workers, reset clocks, replan admitted scope, or launch reacquaintance audits.
A Hand resumes only its own handle and remaining budget. Continue until the
full admitted scope has landed or a real external or operator dependency
remains.
