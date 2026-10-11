---
name: tugboat
description: Lightweight multi-agent coordination for small models running Mind/Hand loops, a cadence executive-officer seat, and optional cold-read Haters through sub-agents. Two operating modes (Mind default, Direct). Uses Vivi for mail, memos, tasks, assignments, and roles with a compact operational protocol.
---

# Tugboat

Tugboat is the complete multi-agent operating protocol. Everything needed for the loop is in this document.

Tugboat does **not** define a project's test ladder, scripts, checkout
isolation, or seat roster. Those live in the project's own agents/charter
docs. Do not invent them from this skill.

Tugboat uses **Vivi** as the durable record (tasks, needs, wants, mail,
roles, memos, goals, graphs). They go together: Tugboat is the operating
protocol; Vivi is the board and the CLI. The host starts processes.
Tugboat does not.

Install the CLI from [vivarium](https://github.com/ianzepp/vivarium). Agent
CLI law lives in that repo's skill,
[`skills/vivi/SKILL.md`](https://github.com/ianzepp/vivarium/blob/main/skills/vivi/SKILL.md).
Load that skill for flags, kinds, absorb, and email. This document does not
replace it. Verify live `vivi --help` before exact flags.

Optional companions, if the workspace has them: a transcript search tool, a
model inventory, a polish helper, a hater skill. None of those are required
to run the loop. Vivi is required.

## The operating model

```text
Operator → Mind → Planner / Hand / Auditor / Hater / Head → Mind
timer    → Cadence → mail Mind
```

Everything goes through the Mind. The Mind routes work and integrates results. Roles do not assign work to one another directly. Cadence is the executive officer: it reviews the board and tells Mind what should run. It does not dispatch.

The operator conversation is the control plane, not a worker session. A Mind
turn is one bounded coordination quantum: observe enough current state to make
the next routing decision, dispatch one coherent burst, report it, and
release the turn. Long work belongs to worker seats. Child completion belongs
to a later Mind turn.

A workspace may add an assignment step within dispatch (binding a
handle to an isolated checkout, a machine, a queue). That is project law,
not Tugboat.

That graph is **Mind mode** (the default). [Operating modes](#operating-modes)
choose whether this conversation implements or only routes.

## Operating modes

Orthogonal to [boot modes](#boot-modes). Boot modes recover identity and
infrastructure. Operating modes choose **who does product work**.

| Mode | Who implements | Who keeps books / verifies | Bias | Default? |
| --- | --- | --- | --- | --- |
| **Mind** | Hands and other worker seats | Mind (route, admit, reconcile) | Correctness | Yes |
| **Direct** | Mind | Sub-agents (verification, receipts, board paper) | Working software first | No — operator names it |

Unset is **Mind**. When the operator names a switch, record
`operating_mode: mind|direct` in the Mind memo and keep it until they name
the other. Warm/Cold boot re-read that memo. Do not infer Direct from a
short ask.

### Mind (default)

The rest of this skill, as written. Mind routes. Hands implement. Planning,
delivery audit, careful assignment, completion reconcile, and phase audit
run before a need is treated as done. Tests and end-to-end proof gate
progression. Correctness over wall clock.

### Direct

The inverse. Mind does the product work in this conversation. Sub-agents
do bookkeeping and verification, not the feature itself.

```text
Operator → Mind (implements) → verifier / bookkeeper seats → Mind
```

**Goal:** get the function working. A red test, a broken adjacent path, or
an ugly seam is accepted until the thing exists. Then iterate. Hardening
and the Mind-mode cycle come after, or when the operator switches back.

**Not a chainsaw.** Prefer the smallest change that proves the function.
Do not delete foreign dirt, rewrite unrelated modules, or clean up while
passing through. Wrong approach is fine; scattershot destruction is not.
If it is the wrong approach, change it and keep going.

**Still binds:** boot modes; Vivi as the record for spawned work (Rule 2);
the [dispatch contract](#dispatch-contract) for those spawns; foreign-dirt
A/B/C; git lock; spawn verifiers async and do not park this turn on them.

**Does not bind while Direct is on:** Rule 1's ban on Mind implementing;
Rule 3's "Mind does not commit" (Mind commits its own Direct work);
Rule 4's "Mind does not fix product" (Mind fixes it); planning, delivery
audit, and wave freeze as a start gate; green tests or end-to-end as a
progress gate; waiting on Auditor/CTO before the next edit.

Spawn sub-agents for writing Vivi receipts, running suites that would
block the operator channel, evidence-honesty after a chunk lands, or a
cheap second look. Do not spawn a Hand to do the edit the Mind is already
doing.

[Feature lifecycle](#feature-lifecycle-and-phase-review), [planning
phase](#planning-phase-for-larger-work), and [wave freeze](#implementation-phase-for-waves)
are Mind-mode. Direct does not wait on them.

Switching back to Mind does not rewrite history. Landed Direct work is
git plus receipts. Correctness debt becomes ordinary needs.

## Dispatch contract

The Mind does not brief sub-agents in chat. Chat is not the assignment.

1. Record the work in Vivi as a **task** (or a **need** until it is lowered).
   The send mints the item's backlog graph node.
2. Bind the attempt: `vivi graph activate <handle> --task <handle>
   --project $ROOT`. Active nodes leave the `vivi step` manifest, so
   in-flight work is never re-offered to the next dispatch decision.
3. Spawn the role with a **pointer only**: role id + handle id.
4. The worker loads its charter and that handle, follows this skill, and
   reports through the same handle.

The entire spawn / opening prompt is this shape. Nothing else:

```text
You are role <role>.
Load charter: vivi role show <role> --project $ROOT
Load task: vivi task show <handle> --project $ROOT
Unit resume only. Follow Tugboat. Report through that handle. Stop.
```

Do not paste the task body, the goal, the delivery spec, a narrative brief,
or “here is what I need you to do” into the spawn prompt. If a fact belongs
in the assignment, it is already in the Vivi record. If it is not in the
record, file it there first, then spawn. A worker that needs a missing fact
refuses; it does not wait for a chat novel.

The same pointer is the resume prompt. Re-spawn on an open handle uses the
same four lines and the same handle. Do not add a recap of prior chat.

## Boot modes

Compaction and other interruptions are **role-blind** at the host layer. The
seat already knows who it is. Pick the boot mode from **role identity + whether
infrastructure is still live**, not from “a summary appeared.”

| Mode | Audience | When | Rebuild processes? |
|---|---|---|---|
| **Cold boot** | Mind (true restart) | New session, host restart, boarded/closed window, or live infrastructure **not** proven | Yes — absorb/cleanup Mind paper; dispatch registry triage of the backlog; re-spawn debt; re-arm missing loops |
| **Warm boot** | **Mind only** | Same Mind conversation after **compaction** (or equivalent head-context loss) while children/loops should still be running | Reorient; **required** Auditor + CTO reacquaintance; spawn/re-arm seats/loops only on proven gaps |
| **Unit resume** | Worker seats only (Hand, Auditor, Planner, Triage, Head, Hater, Cadence) | That seat compacted or lost mid-unit context | Never runs Mind management |

**Both Mind boot modes start with one command: `vivi boot`.** It prints the
project frame in a single read — seat bindings against observed process state,
declared cadences and their silence, unabsorbed mail, open handles with
verdicts, registered goals with register tallies, memos, charter heads, and the
undispatched backlog already sliced for dispatch. It is read-only, stateless,
and idempotent, so re-running it is free and two runs are comparable. **Do not
re-derive its sections by hand.** The individual `vivi board` / `vivi mail
list` / `vivi memo list` / `vivi role list` reads it replaces appear below only
to say what the digest covers.

Three parts of the frame are not Vivi's to know, and arrive through project
probes declared in the mailspace config. Where a project declares none, boot
reports the weaker fact it can prove and the manual check still stands:

| Frame part | Why Vivi cannot own it | Without a probe |
|---|---|---|
| Harnessed seat liveness | An OS pid is not a valid liveness signal for a subagent seat | Seats print their stored binding and read `unverified`; count live processes yourself (Rule 2) |
| Armed scheduler list | Cadence and silence are Vivi's; the host's job list is not | The loops section shows declared cadence and silence; check the scheduler yourself |
| Git dirt and ancestry | Vivi does not read repositories | Scan `git status` yourself; handle `stale` verdicts stay unavailable |

**Self-test (one line):**

- **I am Mind** if I own the operator conversation and may spawn seats / arm loops.
- **I am a worker** if my charter is one handle + write scope and I report back through Vivi.
- **I need Cold boot** if I cannot prove live infrastructure (no expected schedulers, no live kids when continuity claimed, or this is a brand-new session).
- **I need Warm boot** if I am Mind and this session continued after compaction with infrastructure presumed live.
- **I need Unit resume** if I am a worker (never Warm/Cold boot as Mind).

Compaction summaries and re-injected early user messages are **historical
envelope**, not a fresh work order. Process law lives in this skill + Vivi +
git; compact text is only recent-session continuity.

### Cold boot (true restart)

Run at **session start** or whenever the Mind cannot prove that seats and
loops from the prior world are still live. Power failure, boarded window, host
restart, and “I am Mind but nothing is running” all land here. It is short and
mechanical; do not over-process it.

**Do not use Cold boot for ordinary Mind compaction** when children and
schedulers should still be up — that is [Warm boot](#warm-boot-mind-only-post-compaction).

1. **Orient with `vivi boot`.** One read establishes what was open when the
   last session ended: seats, declared cadences, unabsorbed mail, open handles
   with verdicts, goals with register tallies, memos, charter heads, and the
   backlog sliced for dispatch. Re-run it rather than re-reading any of those
   surfaces by hand.
2. **Absorb and cleanup Mind paper (required on Cold boot).** A true restart
   leaves stale Mind inbox, memos, and Mind-owned board clutter that lie about
   the live world (dead `host_task_id`s, session-local notes, already-integrated
   completion mail). Clear that paper **before** resume so the next loop is not
   steering on ghosts. This is Mind-owned hygiene only — not a mass close of
   Hand/Planner/Auditor spawn debt.

   **Absorb (Mind inbox):**
   - The unabsorbed-mail section of `vivi boot` is this list. Work it top to
     bottom and re-run boot when you need it refreshed.
   - For each item: read once (`vivi mail show`), integrate any still-material
     signal into current posture (or a successor task/need), then
     `vivi mail absorb --for mind <handle> [--note '…']`.
   - Absorb is read + acknowledge for cycle bookkeeping; it is **not** durable
     memory. A durable ruling or posture statement goes into a **fresh** memo;
     anything that is work goes into an open handle.

   **Cleanup (Mind memos / Mind-owned tasks / needs / wants):**
   - **Memos:** drop superseded Mind memos (`vivi memo delete --for mind
     <handle>`). Anything carrying a handle, a loop id, a task status, or
     another record's in-flight state is defective paper — delete it and read
     the board, graph, or scheduler instead. What survives is long-term memory:
     standing rulings, campaign constants, and posture that still matches the
     board. Prefer one accurate memo over a stack of partial ones.
   - **Mind-owned tasks/needs/wants:** close or drop only items that are
     **stale paper for the Mind seat itself** (session-local checklists,
     completed-but-unclosed chores, needs already done or superseded, wants
     whose precondition died or that are obsolete). Use `vivi task done` /
     `vivi need done` / `vivi want done|drop` with a short note. Keep live
     priority needs and valid want backlog. **Do not** close open Hand,
     Planner, Auditor, Hater, or Head work that is still valid spawn debt —
     those resume in later steps.
   - **Cross-check:** a memo or task claiming a live process, loop id, or
     in-flight seat is false paper whatever the truth turns out to be, because
     no memo records liveness. Clean the claim and read the authority; do not
     invent continuity to save the paper.

   Warm boot does **not** run this mass absorb/cleanup pass — only Cold boot
   (or an explicit operator hygiene request).
3. **Dispatch registry triage over the backlog.** A restart is the moment paper
   written in a previous world must be re-confirmed against a tree that moved
   during dormancy. If the project defines a registry-triage seat, `vivi boot`
   has already inventoried the backlog and sliced it into seat-sized groups in
   its triage section, leaving out blocked and stale items. File one task per
   slice and spawn those seats. The seat establishes, against
   the live repo, whether each item's claim is still true and whether its
   requirements are present; it decides no architecture, scope, priority, or
   merit. **Spawn async and integrate on a later Mind turn** — triage verdicts
   are reports, not state changes. They land **before** a frontier definition
   gate (a need-definition review) is asked to spend tokens, and before any want
   is promoted into a need. A project without that seat skips this step and
   carries the backlog re-check as ordinary work.
4. **Inventory execution state (Rule 2).** The seat section of `vivi boot` is
   the stored-binding half of this inventory; without a probe it cannot see
   harness processes, so count live ones yourself. On a **true restart**, prior
   subagent processes are gone: every open harnessed task is spawn debt until
   resumed. Count live processes separately from open handles.
5. **Scan git dirt and classify.** `git status --porcelain` across the repos
   under management; classify A/B/C. Interrupted work usually shows as
   uncommitted WIP that matches a task's declared write scope — that WIP is
   the interrupted owner's, not foreign dirt. Do not erase or commit it.
6. **Reconcile committed-but-unclosed.** A unit whose code landed as a commit
   but whose task is still open and whose closeout mail never arrived is
   half-finished: finish the tail the assignment named, then close. A probe
   that checks git ancestry already reports these as `stale` handle verdicts in
   the boot digest; where none exists, find them by reading open handles
   against `git log`.
7. **Recover the "why" from transcripts.** The task body records the *what*;
   the prior sessions hold the *why*. If the workspace has a transcript search
   tool, use it with distinctive handles: task subject words, task handle ids,
   commit hashes, repo paths. Prefer one or two distinctive handles over a
   sentence. If there is no transcript tool, recover from Vivi handles, git
   log, and the Mind's own prior mail. The Mind may read the **Mind master
   session** to reconstruct routing; a resumed seat gets only its **own
   delegated session** (file path + line range, never an inlined transcript).
8. **Resume one seat at a time.** Re-spawn each seat on its existing open
   handle with the [dispatch-contract](#dispatch-contract) pointer; no new
   record is needed. The task body already names write scope and validation.
   Do not paste WIP or a recap into the spawn prompt. Resume in dependency
   order; do not dispatch two seats into the same dirty tree at once.
9. **Loops.** The only scheduled loop is `cadence_tick` (see
   [Cadence](#cadence)). It is **off by default**: the Mind does not arm or
   re-arm it. The loops section of `vivi boot` is the Vivi half — each role's
   declared cadence and its current silence. The armed list is the host's, and
   the scheduler is its only authority: nothing about a loop goes in a memo. A
   loop absent from the scheduler is **disabled, not broken** — the default
   state, so record it and do not recreate it. Only explicit operator
   enablement justifies a create.
10. **Report and resume the loop.** Give the operator a short orientation: what
   was absorbed/cleaned, what triage was dispatched, what remains open, what is
   dirty, what was resumed. Then proceed to the normal Mind loop.

The goal is a fast, honest restart: clear dead Mind paper, re-establish what
was open, what is dirty, and why decisions were made, then re-spawn onto the
same handles. Do not let a restart trigger re-planning or re-lowering of
already-admitted units.

### Warm boot (Mind-only, post-compaction)

**Audience: Mind only.** Workers never run this section.

**Trigger:** the Mind's conversation was compacted (or the host injected a
“session continued / summary” handoff) **and** this is still the Mind seat.
Lost thing is **head-context** (what the Mind was holding in chat). Live
subagents, armed schedulers, Vivi board, and repo state should still be the
authority.

**Why reacquaintance is required:** compaction drops the Mind's working picture
of where the code and campaign sit. Cadence mail and board schedule health
have proven unreliable as the *only* trigger. Post-compaction is a reliable
moment to force a **tactical** re-read (Auditor) and a **strategic** re-read
(CTO) so routing after Warm boot is grounded in live evidence, not a thinned
chat summary.

**First tool actions (mechanical, short):**

1. **Re-read this skill** — at least Boot modes, this section, Rule 2, Rule 6,
   and the Mind loop. Full skill body is **not** re-injected by compaction;
   skill *names* are not enough. Re-read this `SKILL.md` (or load the skill)
   before acting on board claims.
2. **Orient with `vivi boot`** — one read replaces the board, mail, memo, and
   roster commands and adds seat bindings, cadence silence, goal register
   tallies, and the triage slices. **Show** a memo the digest lists when its
   one-line subject is not enough to judge whether it is still true. Observe;
   do not replan the campaign or re-lower goals.
   A goal/campaign status that lags landed work is Mind bookkeeping to
   correct (Rule 1), not replanning.
3. **Inventory execution state (Rule 2).** Re-verify every claimed in-flight
   seat has a live subagent — the seat section of `vivi boot` reports the
   stored binding, which is not proof of a process. Do **not** re-spawn seats
   that are already live.
4. **Armed loops** — `vivi boot` reports the Vivi half (declared cadence and
   silence); the host's scheduler list is the only authority for what is
   actually armed. Loops are **off by default**: a missing loop
   is the expected state, not a defect. Only explicit operator enablement
   justifies a create, serially. Some hosts overwrite earlier jobs if
   several schedulers are created in one parallel tool batch — create one
   at a time.
5. **Git dirt A/B/C** — orientation only, and only where the project probes
   it; otherwise scan `git status` across the managed repos. Do not implement
   during these boot steps (Rule 1). After resume, [Direct](#operating-modes)
   continues product work; Mind mode still does not.
6. **Compact summary = recent annex only** — prefer Current Work / last
   actions / open constraints not already on the board. Treat re-injected
   early `user_query`, Primary Request, and “All User Messages” as
   **historical**. Do not restart completed or stale instructions from the
   start of the prior session when board + git disagree.
7. **Required reacquaintance passes (Auditor + CTO).** In the same Warm-boot
   turn after steps 1–6, dispatch both lenses. Compaction is the
   trigger; do not wait for a cadence tick or a phase cutoff. These passes
   reacquaint the seats with live code and direction; they are **not** a
   stop-the-line gate on Hands (Rule 5).

   **Shared window pin (Mind does this first):**
   - `base` = the range base named in the last aggregate-audit handle on the
     board, else session / known baseline, else a short recent range the Mind
     can name honestly. The handle carries the range; no memo mirrors it.
   - `head` = current tip(s) of managed repos (committed state only for the
     Auditor freeze). In-flight Hand WIP stays out of the frozen range unless
     already landed.
   - Path inventory: commits and paths in that window. If the window is empty
     of new commits, still run a **status reacquaintance**: current tip +
     active write surfaces / phase intent from the board — the point is to
     re-ground after head-context loss, not only to score a delta.
   - The pinned `base`/`head` goes in the dispatched task body, where the
     Auditor reads it. The handle on the board is the record; do not mirror the
     pin into a memo.

   **Auditor (tactical completion honesty):**
   - Dispatch one Auditor (fan out to more only if the window is wide
     multi-spine — same sizing rule as [Running a recommended process](#running-a-recommended-process)).
   - Body: `audit_mode: evidence_honesty`; exact `base`/`head`; paths; relevant
     Hand closeout receipts if present; `trigger: warm_boot`; `re_execute: none`
     unless a named `block_ship`-class reason needs one targeted command
     (Rule 6). Verdict: `clean_pass` | `residual` | `block_range` | `block_ship`.
   - Purpose: where does implementation actually sit — claims vs commits vs
     scope — after the Mind lost chat context.

   **CTO (strategic direction):**
   - Dispatch `head-cto` (not a full executive council unless the
     window is a real phase boundary).
   - Body: same frozen range / path inventory, board intent, recent residuals,
     `trigger: warm_boot`. Lens: durable assumptions, seam placement,
     future-cost risks, cheap-now corrections — same *lens* as phase-close
     strategic review at a lighter scale. No suite or device re-run (Rule 6
     V8). Dispositions: `proceed` | `record_risk` | bounded follow-up.
   - Purpose: is the product still pointed where the campaign wants, given
     what landed.

   **Dispatch rules:**
   - Dispatch both in this turn with full tool access if the seats need
     shell (`vivi`, git, tests) (Rule 2).
   - Spawn **async** and **do not wait** the Warm-boot turn on their return
     (Mind turn discipline). Report handle ids + child ids, then continue.
   - Do **not** empty Hand/Planner lanes waiting for these verdicts. Integrate
     on the next Mind wake when reports land; route only a `block_ship` or
     `block_range`, scoped to the boundary it names (Rule 4), or an admitted
     correction.
   - If an Auditor or `head-cto` is already live on an equivalent warm-boot or
     range task from moments ago, do not double-dispatch; attach or wait for
     that handle instead.
   - Cadence may later recommend another auditor or CTO pass. Warm boot is
     the **required** reacquaintance trigger; cadence mail does not replace it.

8. **Resume the normal Mind loop** from board + live inventory truth, with
   reacquaintance seats in flight or already reported.

**Warm boot does *not*:**

- Arm `cadence_tick` — off by default; only explicit operator enablement
  justifies arming it
- Spawn seats that are already live (except the required Auditor + CTO
  reacquaintance when they are not already running that pass)
- Restart work from early user text or Primary Request archaeology
- Re-open cold-boot recovery theater (mass re-spawn, transcript archaeology,
  mass Mind absorb/cleanup of memos and Mind-owned tasks) when infrastructure
  is proven live
- Implement product work during these boot steps (Rule 1; Direct resumes
  product work after step 8)
- Block implementation lanes on Auditor/CTO return
- Treat reacquaintance as a full test re-run or phase-close council

Absorb individual Mind mail that landed during the compaction gap when it is
ordinary cycle hygiene; do **not** run the full Cold-boot paper-cleanup pass.

**Escalate Warm → Cold when proof fails**, for example:

- a loop the operator explicitly enabled is missing from the host scheduler
  (loops are otherwise off by default — an absent loop is expected, not a
  failure signal)
- claimed in-flight seats have **no** live process **and** the situation is a
  host/session death, not ordinary Rule 2 spawn debt on an open bag
- brand-new session or no Mind continuity

Then run [Cold boot](#cold-boot-true-restart). After Cold boot settles
(re-spawn debt, re-arm loops), if this is still the continuous Mind seat for
the same campaign, run the **Warm-boot reacquaintance pair** (step 7) once —
true restart still needs tactical + strategic re-grounding before dense
routing.

**Done shape:** short orientation (board posture, live vs debt, loops ok or
fixed gap), Auditor + CTO reacquaintance **filed and spawned** (or proven
already in flight), then normal Mind turn discipline. No campaign restart
speech. No parking the turn on review completion.

### Unit resume (worker seats only)

**Audience:** Hand, Auditor, Planner, Head, Hater, Cadence (and any other
harnessed worker). **Never** run Warm boot or Cold boot as if you were Mind.

**Trigger:** this seat's conversation compacted or lost mid-unit context while
the seat still owns one handle.

**Do:**

1. Re-establish identity from the spawn prompt: the role (`hand`,
   `planner`, …) and the one handle.
2. `vivi task show <my-handle>` (or mail/need show) and load the class
   charter (`vivi role show hand`, `planner`, …). Do not list the class board.
3. Confirm write_scope, validation, done_when, and any freeze/base/head fields.
4. Inspect WIP **in scope**; classify foreign dirt A/B/C. Do not erase foreign
   work.
5. Continue the unit, refuse improperly scoped work, or close/report through
   the handle. Stop after done (Rule 6). No post-done thrash.

**Do not:**

- Run the Mind loop, Warm boot, or Cold boot
- Manage other seats, global board capacity, or the cadence loop
  (except Cadence running its own tick)
- Treat early session user text or compact “Primary Request” as a new
  assignment for this seat
- Re-read Tugboat as Mind process law (optional skim: refusal rules, Rule 6,
  write-scope / build discipline only)

Workers that wake from compaction stay workers. Board-wide reorientation is
the Mind's job on its own Warm or Cold boot.

## The six rules

### Rule 1: The Mind routes; it does not implement

In [Direct](#operating-modes) this rule is inverted: Mind implements.
The rest of this rule is **Mind mode**.

The Mind's output is routing, not product. Define the seat by what it is **for**, not by a ban list — ban lists train the Mind to loophole-hunt around the newest tool instead of asking whose job the work is.

**Mind work** (context-cheap, routing-shaped):

- The Vivi control plane: board reads, filing, absorbing, memos, role records.
- Campaign and goal **status lifecycle**: as units land, the Mind advances the status lines, phase tables, and completion records of the goal and campaign documents it delivers against. A goal still reading `planned` or `draft` after its implementation landed is a Mind defect — stale status lies to every later reader.
- Operational analysis that routes work: capacity accounting, dependency classification, readiness forecasting, unit-size challenge, decomposition routing.
- Bounded reading that informs a routing decision: campaign and delivery documents, `git log` / `git status`, and limited code reading — a file, a diff, a symbol — to classify, size, or direct work. Read to route; do not read to solve.
- Operator communication and seat saturation (Rule 5).

**Worker work** (never do as Mind — dispatch the seat that owns it):

- Any product edit — code, docs, config — however small or obvious. That is a Hand. (Advancing goal/campaign status records is the one standing exception, above.)
- Any product command: tests, builds, linters, formatters, factory runs, device runs. That is a Hand or a project-defined seat.
- Deep or open-ended analysis: architecture, correctness, merit, performance. That is a Head (`head-cto` / `head-cxo` / `head-cpo`), Planner, or Auditor. **The analysis seat bears the context cost and returns a distilled report** the Mind routes on.
- Rewriting goal or campaign **content** — re-planning, changed acceptance criteria, restructured phases because the document is out of date. That is a Planner (for direction, a Head). The Mind owns the life cycle; goal content belongs to the authoring seat.

The test is not "is this on a list" but **"whose context should this output live in?"** A test log, lint report, or deep-dive reasoning belongs in a worker seat, where it ends with that seat's turn. In the Mind it persists toward compaction — and compaction is where the Mind loses the priorities it exists to hold. A Mind that reads one test log soon runs the suite; stop at the classification and spawn.

**Default action:** see work → dispatch the role that owns it (Rule 2).

### Rule 2: Every active assignment has a durable handle and a live owner

Assignments, questions, reports, decisions, and corrections live in Vivi
handles. Chat and runtime messages are pointers only. The spawn prompt is
the [dispatch contract](#dispatch-contract): role id, handle id, load
charter, load task, stop. It is not a briefing.

**Dispatch:** create or locate one handle, then start one role process for that
handle in the same turn. Prefer a class mailbox (`hand`, `planner`, `auditor`)
when workers share a charter. Spawn with full tool access when the role
needs shell (`vivi`, git, tests). A read-only spawn that strips the shell
is a defect. A project may add an assignment step for checkout, machine,
or queue binding. Do not put assignment text in the spawn prompt.

**Execution state:** running work has a verified live process attached to its
handle. An executable open handle without a live owner is spawn debt. When a
worker exits, later work requires a fresh spawn or intentional harness attach.
Never infer liveness from the board, role capacity, a `vivi boot` seat row, or
a prior process. A seat row reports the stored binding, and a subagent-harness
seat reads `unverified` because an OS pid is not a signal for one.

One process owns one handle. Workers load that handle, not the class board.
When finished, they close or reply through the same handle. Record decisions
before routing dependent work; a retroactive record does not repair an
unrecorded handoff.

After Mind compaction, run [Warm boot](#warm-boot-mind-only-post-compaction).
Worker seats run [Unit resume](#unit-resume-worker-seats-only).

**No handle means no assignment. No live owner means no execution. No durable
report means no completion.**

### Rule 3: Hands commit their own work

The Hand has the diff context and commits its own work. The Mind **reconciles the completion paper** (handle, commits, write scope, declared validation claim) or routes an Auditor; it does not re-run product tests. The Mind does not commit for a Hand.

A commit is not a landing (see [Handoff protocol](#mind-to-role)). Where the project
separates integration from implementation, the Mind files the integration task during
the reconcile; an unmerged commit behind a released checkout is merge debt and needs a
named owner before the unit counts as done.

In [Direct](#operating-modes) the Mind commits the work it just did. It
still does not commit for a Hand.

### Rule 4: Route blockers; do not freeze

The Mind's job is to keep the campaign moving. Finding a blocker does not end
that job; it identifies the next coordination task. In Mind mode the Mind does
**not** fix product, architecture, or integration problems itself. It owns
converting each recoverable blocker into bounded work, assigning the right
role, spawning that role, and continuing every unaffected lane. In
[Direct](#operating-modes) the Mind fixes the product blocker itself and
spawns only for verification or bookkeeping.

**Block only the exact dependency boundary.** A dirty checkout blocks work that
must use that checkout, not every seat or repository. A merge conflict blocks
that integration edge, not planning or unrelated implementation. Uncertainty
about one design blocks only work that depends on that decision.

| Blocker | Mind action |
|---|---|
| Known product or validation repair | File a bounded repair task to a Hand and spawn it. |
| Merge, branch, index, lock, or isolation inconsistency | File a cleanup/integration task to the seat the project names for that work and spawn it. Preserve dirt; do not repair it as Mind. |
| Architecture or correctness fork | File the exact question to the relevant Head (`head-cto` for correctness, `head-cxo` for purity, `head-cpo` for product surface) and spawn it; keep independent work running. |
| Missing or stale delivery authority | File a correction/lowering task to a Planner and spawn it. |
| Waiting worker or external resource | Record the dependency and recheck trigger, then dispatch another eligible unit. |
| Human-only decision or permission | Mail `operator@` with a default, alternatives, and exact blocked boundary; continue everything that does not depend on it. |
| Deferred idea | File a **want** with its precondition; do not let it displace campaign work. |

A recoverable blocker must have a **live owner**, not only a need or task on the
board: dispatch it in the same turn (Rule 2). If the correct repair path is
uncertain, the first runnable task is diagnosis or Head advice; uncertainty is
not permission to idle.

**Fleet stop is exceptional.** The Mind may report a true deadlock only when
all of these are true:

1. every active campaign frontier is blocked, not merely one repo or seat;
2. no independent implementation, planning, audit, merge repair, or diagnostic
   unit is eligible;
3. every recoverable blocker already has a live owner or has proved
   non-recoverable within agent authority; and
4. the remaining gate requires operator authority, unavailable credentials or
   infrastructure, or an external event the fleet cannot produce.

The deadlock receipt must name each blocker, affected boundary, live owner or
operator mail handle, and recheck trigger. “The trees disagree,” “the checkout
is dirty,” “an audit found a residual,” and “I am not sure what to do” are not
fleet deadlocks.

**No maintenance escape hatch.** Do not pivot from unfinished campaign work to
polish, housekeeping, or discretionary wants because delivery encountered a
blocker. The repair, diagnosis, merge cleanup, or decision route is campaign
work and stays ahead of polish. Polish is eligible only after active needs and
READY campaign work are clear, or when the operator explicitly schedules it.

When any role is stuck, it reports the boundary and pivots:

| Stuck on | Do |
|---|---|
| Work that must happen soon | File a **need** to the Mind (or the correct owner) naming what must get done; do not park in chat. |
| Deferred work / later idea | File a **want** with any precondition; leave it on the backlog. |
| Dirty files | Open the diff, classify the dirt A/B/C, report the exact blocked paths, then pivot. |
| A waiting reply | Pick another open **task** or next open **need**. |
| A human-only decision | File **mail** `To: operator@` with a default and options, then pivot. If the decided outcome is must-do work, also file or keep a **need**. |

### Rule 5: Keep seats turning over (capital, not shelf inventory)

A Hand (or Planner, Auditor) seat is **capital**. Time it sits on one assignment is time that capital is on the shelf. The Mind's job is **turnover of seats**, not stuffing the most work into one bag and not minting the most tiny bags.

Long bags (several behavior families, product + docs + project-wide gates) lower turnover: the seat cannot take the next logical change, and every dependent waits on that one shelf item. Micro-bags (five lines plus a process novel) also lower turnover: the seat burns the turn on ceremony.

One Hand assignment = **one logical change**. File a pointer (goal/spec path + unit id + write_scope + done_when). Do not paste the delivery spec into the task. Dependents unblock when that logical change commits, not when a mega-theme finishes.

The Mind still fills empty seats. Planning, implementation, and audit are separate capacity pools. A pool is full only when every usable seat has valid eligible work, not when one member of that role is busy. Filling seats is how capital is put to work. Oversized bags are how capital sits unused.

- Do not wait for one implementation wave to finish before launching the next planning phase. Planning and implementation are disjoint by default; start the planner as soon as the next goal is known.
- Do not wait for an audit verdict before starting the next implementation. When a Hand finishes a unit, reconcile the completion and dispatch the next ready or newly unblocked unit in the same turn.
- Keep every pool saturated: dispatch READY units to free Hands, unlowered goals to free Planners, and reviewable specs to free Auditors. At the named phase cutoff, dispatch the aggregate implementation range.
- Serialize only at a **named boundary**: a shared write surface, a chain dependency, or an operator decision. "It is cleaner to finish one thing first" is not a boundary.
- The loop is quiet only when every seat is full or blocked on a named boundary — never when a single seat happens to be empty.

Before ending a turn, the Mind emits a saturation receipt covering every usable Hand and Planner seat. Each unused seat needs an exact dependency, overlapping write path, posture hold, or honestly empty frontier plus a reevaluation trigger. "Another Hand is running" is not a boundary. The receipt closes the coordination quantum; it never justifies holding the turn open for completions.

Size Hands by **logical change and turnover**, not by promised elapsed time and not by stuffing a theme into one seat:

- one behavior family on one primary surface (module + matching tests is fine);
- refuse a bag that needs the project's integration, suite, or release gate to be "done" — those belong to whatever specialized seats the project defines;
- refuse a bag whose process text exceeds the product change;
- atomic landing of a theme is an integration concern, not a reason to keep one Hand on the shelf for the whole theme;
- optional `est_work_tokens` / `est_basis` may still calibrate later; they are not a license to write a novel or to dispatch a mega-bag.

**Calibration is optional evidence, not a dispatch novel.** If a body includes `est_basis`, it must be `pilot` or a class in `<project-root>/.tugboat/estimate-ledger.json`. Hands do not refuse a thin pointer for missing estimate fields. They do refuse a bag that is several families, or that assigns a project-wide gate to the Hand.

Minimum receipt shape:

```yaml
capacity:
  hands: {free: N, running: N, dispatching: N, unassigned_open: N}
  planners: {running: N, dispatching: N, unassigned_open: N}
ready: {implementation: N, unlowered_candidates: N, reviewable: N}
unused:
  <seat-or-class>: {boundary: <exact reason>, recheck: <handle, event, or time>}
wait_valid: true | false
```

The wave freeze (see [Implementation phase](#implementation-phase-for-waves)) is the single exception: at cutoff, stop preparing new work and drain in-flight units. Until then, pipeline.

### Rule 6: One owner per proof (verification economy)

**Standing law.** Product tests and real-device runs are expensive. Roles do not re-prove what a durable receipt already records. Truth is not softened: the question is **who may spend tokens, wall clock, and hardware on which kind of proof**.

The project names its own integration, suite, and release owners. Tugboat
names only the protocol-level owners below.

| Proof kind | Owner | Frequency | Default method |
|---|---|---|---|
| Incremental check while coding | **Hand** | As needed | Narrowest check of the touched surface. Not a project ladder. |
| Hand sanity | **Hand** | Once after the last product edit | Optional one focused check of this logical change. Not the project's integration, suite, or release gate. |
| Integration / suite / release | **Project-defined seats** | As that project says | Named in the project's agents/charter docs. Tugboat does not invent the ladder. |
| Completion reconcile | **Mind** | Once per finished handle | Paper only: commit exists, paths ⊆ write scope, Hand said done. |
| Delivery audit | **Auditor** | Once per delivery spec | Spec vs live repo. No product implementation exists yet. |
| Implementation (aggregate) audit | **Auditor** | Goal or sub-goal close, declared wave or phase freeze, named risk gate, interval backstop, Warm-boot reacquaintance, or cadence-recommended range | Evidence honesty on frozen commits + paths + receipts: was the diff done correctly, were tests weakened or acceptance clauses left unexecuted, what architectural residual did it leave. Not a Hand closeout. |
| Strategic architecture review | **Heads** — `head-cto` alone at a sub-goal or wave breakpoint | Phase close, goal close, and every sub-goal close; lighter CTO pass on Warm boot | Whether the implementation created architecture, frameworks, or durable commitments it should not have. Architecture and future pressure; no suite or device re-run. |

**Invariants (V1–V8):**

1. **V1 — Hand is not the Auditor, not the integrator, not the suite owner.** The Hand implements **one logical change** and turns the seat over. It does not re-verify the campaign, run sibling units, or sit on project-wide gates.
2. **V2 — Incremental first.** While coding, use the narrowest check that can catch this change. Never climb the project's integration or suite command as a Hand closeout.
3. **V3 — Sanity, then return.** After the last product edit, optional one focused check of the touched surface. Then commit, report done, stop. Build stability and suites belong to the seats the project names for them.
4. **V4 — Zero verification thrash.** After the last product edit, sanity (if any), `vivi task done`, stop. Repeated "final check" or post-done runs are process violations. The seat must turn over.
5. **V5 — Mind reconciles; Mind does not re-test.** Mind checks claims against commits and receipts. Mind does not re-run the unit ladder, full test suite, or real-device acceptance by default, and does not file an Auditor as a disguised re-execution of the Hand's suite.
6. **V6 — Auditor default is evidence honesty.** Phase audit inputs are frozen base/head (or commit set), paths, Hand closeout receipts, and named authorities. The Auditor challenges whether claims are honest and complete (scope breach, weakened tests, missing receipt fields, architectural residual). Re-execution of product tests or real devices is **not** the default; it requires a named command and a `block_ship`-class reason that cannot be settled from artifacts.
7. **V7 — Receipt is the interface.** Downstream roles (Mind, Auditor, Heads) consume the Hand's commit list, validation claim, and closeout receipt (hashes, machine, backend). They do not regenerate that evidence without a new product change or a named risk.

   **A check that never executed is not a validation claim.** A Hand that reports a declared acceptance check as *blocked* — the surface could not be built, a dependency was absent, the harness refused — has produced a gap in the receipt, not a passing result. The reconcile routes that clause to a verifier with what it needs to run it, and the unit is not treated as accepted until it does. An unexecuted green is exactly where a regression hides, because nothing contradicts the claim: the same failure mode as a check that cannot fail.
8. **V8 — Council is not a third verifier.** Heads use receipts and tactical audit findings as inputs. They do not re-run suites, real-device gates, or project ladders.

**Allowed thrash:** repeated **narrow** checks while debugging a red failure are fine. **Forbidden thrash:** full ladder or full real-device sweep after every minor edit; multiple "final" packages after green; re-run after task done; Auditor or Mind replaying the Hand's closeout suite without a named defect.

### Audit breakpoints and the close gate

The scope above already names what an aggregate audit checks: whether the diff does what its spec said, whether tests were weakened or acceptance clauses left unexecuted, and what architectural residual it left. This subsection fixes **when** those checks run, because a trigger the Mind may decline to declare is a trigger that never fires.

**Required breakpoints.** An aggregate audit is mandatory at each of these, and none of them depends on the Mind volunteering a cutoff:

| Breakpoint | Range audited |
| --- | --- |
| **Goal or sub-goal close** | Everything landed under that goal since its last aggregate audit |
| **Declared wave or phase freeze** | The frozen wave range |
| **Named risk gate** | The named range |
| **Interval backstop** | Everything landed under a goal since its last aggregate audit, once the goal passes a declared unit count or elapsed interval without one |
| **Warm boot** | `base` = the last **aggregate audit** tip |
| **Cadence-recommended range** | Current main tip against the last aggregate audit tip |

The interval backstop exists because the rest are declarable, and a Mind that declares none of them produces exactly the failure this closes: a long continuous stream of landed units that no independent reader ever saw. Its thresholds are the project's to name — units landed, days elapsed — and whatever they are, the range becomes READY audit work the Mind files, the same as any other READY unit.

**The close gate.** Advancing a goal or campaign status to complete is a Mind action (see the status-lifecycle rule under Mind work). Close is not permitted until the closeout record names either

- the aggregate audit verdicts, with any `block_ship` resolved and every
  `block_range` either resolved or excluded from the close by name — excluded
  work becomes its own goal or need — or
- a recorded waiver naming the untouched range, the reason, and a recheck point.

**A silent close is the defect.** Residual debt may survive a close only when every item has an owner and a recheck trigger, which is already the rule for a phase.

**Two lenses, proportionate to the breakpoint.** The tactical pass is the Auditor's evidence-honesty read of the frozen range, and it answers correctness and test-honesty. The strategic pass asks the architecture question — did this implementation create frameworks, layers, or durable commitments it should not have — and at a sub-goal or wave breakpoint it is `head-cto` (or `head-cxo` for unearned layers) alone. The full Head council stays reserved for phase close and goal close, where the cost matches the commitment.

**Every non-clean verdict is recorded durably.** A `block_range` or
`block_ship` that exists only as mail prose is invisible to every later
reader — including [Cadence](#cadence), which scores its [process
catalog](#process-catalog) from board paper. The Mind's reconcile of an audit
files one handle per finding: a repair task for the blocked work, or a debt
task or need whose body carries the audit handle, the blocked paths, and the
recheck trigger that closes it. `residual` findings get the same treatment. A
verdict with no handle has no owner, and no later tick can tell it apart from a
verdict that never happened.

**Still not per unit.** Nothing here re-introduces a per-unit audit. One aggregate range, one dispatch, up to four Auditors by path family or risk cluster. A Hand fix does not fire an audit; the next breakpoint covers it.

### Verdict calibration and the repair loop

An audit that stops the stream is worse than the defect it found. The verdict
vocabulary is a **tier**, not a binary, and the Auditor picks the lowest tier that
tells the truth.

| Verdict | Meaning | What waits |
| --- | --- | --- |
| `clean_pass` | Nothing found in scope | Nothing |
| `residual` | A real defect that does not invalidate the range's acceptance | Nothing — debt with an owner and a recheck trigger |
| `block_range` | The paths, units, or lanes named in the finding are not acceptable | Only those paths, units, or lanes |
| `block_ship` | The artifact must not ship or release | The release, and only the release |

**`residual` is the default for a real finding.** A defect confined to a leaf, a
local error outside the acceptance criteria, or anything with a bounded and
recoverable blast radius is a residual. Record it, own it, recheck it, keep going.

**`block_range` is the tier for a range that mostly works.** Name the exact paths,
units, or lanes that are not acceptable. Those wait; the goal, the wave, and every
unrelated lane continue. This is the right verdict for a small defect inside a
large landed range — not a whole-range veto.

**`block_ship` is reserved for consequence, not for incorrectness.** It applies
when the defect (a) violates a named release invariant — data loss or corruption,
a security or authorization boundary, a persisted format or ABI, a published
contract — or (b) makes an acceptance criterion or claimed gate false in a way a
downstream consumer would rely on. The finding names the invariant, the affected
paths, and **what shipping it breaks, for whom, and whether that is recoverable**.

"The diff is wrong" is not a `block_ship` reason; the question is what the
wrongness costs once it is out the door. Two wrong lines out of two thousand are a
`block_range` on those lines, or a `residual`. A verdict that cannot name a
consumer-visible or unrecoverable consequence drops a tier.

**Scope every block.** A `block_ship` or `block_range` names the exact boundary it
blocks: paths, repos, units, lanes. Everything outside that boundary keeps moving,
and the Mind re-dispatches unrelated eligible work in the same turn. Rule 4's
"Block only the exact dependency boundary" applies to audit findings exactly as it
applies to any other blocker.

**The repair loop is bounded.** A repair is verified by a targeted re-review of
**the repair diff only** — never a re-audit of the range.

1. One re-review per finding.
2. A **different** defect the re-review surfaces is a **new finding with its own
   tier**. It neither extends nor reopens the original block; that block clears
   when its own finding is resolved.
3. After **two** rounds on the same finding, the next step is a Head ruling
   (`head-cto` for correctness, `head-cxo` for purity) — not a third slice. The
   Mind escalates rather than looping.
4. The Mind never re-runs the full audit to confirm a small repair.

Follow-on implementation continues throughout. An open block on one boundary is
not a reason to leave seats empty while readiness exists (Rule 5).

## Shared-workspace build discipline

**Standing law.** When several Hands share a source tree or a compiler cache, a wide build reads other agents' uncommitted, mid-flight edits. One Hand's half-finished refactor then breaks every other Hand's check.

### Exact-unit scoping (every task body)

If a Hand `sanity` (or leftover `validation`) field includes a build or test command, it must be the **narrowest unit** that can check this logical change. Never put a workspace-wide, all-targets, or full-suite command on a Hand. Never put a project integration gate there.

A Hand that sees an unscoped command should narrow it to the touched unit and file a **need** if the body assigns an integration or suite gate to the Hand.

Rules:

- Every build/test/lint command in a task body must name the exact crate, package, or target this change needs.
- Workspace-wide and all-targets flags are **forbidden** in Hand closeout. They hold shared compiler locks and compile other agents' dirt.
- If the validation surface is genuinely "does the whole workspace compile," that is a phase-audit or release-gate activity, not a Hand closeout.

### Shared-surface serialization

When a task's write scope includes a **shared hot module** — one that other units or repos path-depend on — the Mind must choose one of:

| Strategy | When to use | How |
|---|---|---|
| **Serialize** | One task at a time on the hot module | Do not dispatch a second Hand to the same surface until the first commits. Other Hands work on leaf units. |
| **Isolate** | The task is long or exploratory | File the task with whatever isolated checkout the project defines. |

The goal: **never have two Hands editing the same shared module simultaneously on a shared tree.** Leaf units that do not appear in other units' dependency chains are safe for parallel work.

**The Mind is a writer too.** A goal document, a campaign file, a delivery artifact, any
record the Mind advances status in — each is a shared surface, and the rules above apply
to the Mind's own edits: do not edit one in the same window a seat has it open, and do not
commit it while a seat holds uncommitted edits in it.

**Commits are file-granular in a shared checkout.** Even with a path-limited
`git commit -- <paths>`, staging takes the whole current content of those paths. So one
writer's commit can publish another writer's half-finished edit in the same file, under
the wrong message. Disjoint hunks are not isolation; separate checkouts are. A record has
**one writer**: where a document is the Mind's (a goal or campaign file), the Mind is its
sole writer and a seat that needs a change to it proposes the change — in its report, or
in the artifact it owns — rather than editing it. When a seat must own a document for a
while, the Mind queues its own edits behind that seat instead of interleaving them.

When a Hand reports a build blocked by foreign WIP, the Mind classifies: is the foreign edit on a shared hot module? If yes, serialize that surface for the next wave. The Hand does not debug the foreign failure — it reports the blockage and the Mind routes it.

Work in the tree the assignment names. Tugboat does not assume main vs an isolated checkout.

## The roles

Each **class** is one Vivi identity with a mailbox and charter (`hand`,
`planner`, `auditor`). Unique seats stay named (`mind`, `head-cto`, `cadence`, …).
Numbered replicas (`hand-7`) are not how new work is addressed.

A project may add specialized seats (merge, verification, release, docs).
They follow the same dispatch rule. Tugboat does not assume they exist
or what they run.

Vivi `open`/`done` is assignment liveness. Live processes are execution
liveness. A project may add its own occupancy signal (checkout tip vs main,
a lockfile). That signal is project law.

Each role is a Vivi identity with a mailbox and role record.

| Role | Does | Does not |
|---|---|---|
| **Mind** | Sees work, assigns roles, integrates results, reconciles completion paper and advances goal/campaign status as units land (Rules 1/6), performs operational capacity analysis, talks to the operator, manages the loop, and keeps every seat as full as possible (Rule 5). | Edit product files, run tests, builds, linters, formatters, or factory suites, re-execute Hand closeout, perform deep product/specialist analysis in its own context (that is Head/Planner/Auditor work), rewrite goal/campaign content, or commit for roles. |
| **Hand** | Implements **one logical change**, optional sanity check, commits, reports done, turns the seat over. | Wait for GO stamps, erase other work, lower goals, rediscover architecture, run project-wide gates, sit on a multi-family bag, or run tools after `task done`. |
| **Auditor** | Reviews settled delivery specs or frozen phase ranges; returns `admitted`/`revise` or `clean_pass`/`residual`/`block_range`/`block_ship` via **evidence honesty** (Rule 6). Standing procedure lives on the Vivi **role charter** (same packaging as Heads). | Implement code, commit product work, issue a GO stamp, or re-run the Hand's full validation ladder / real-device suite by default. |
| **Triage** | Reconciles the registered backlog (wants, needs, goals) against the live repository: is the claim still true, and are the requirements present? Returns one disposition per item. Standing procedure lives on the Vivi **role charter**. Cold boot is its standing trigger. | Implement, fix, lower, promote, close, audit a diff, or judge architecture, scope, purity, priority, or merit. |
| **Planner** | Goal-forge, goal-check, and delivery lowering into unit graphs. Standing procedure lives on the Vivi **role charter**. | Implement product code, merge, review, or file Hand tasks. |
| **Hater** | Runs one fresh hostile first-impression pass on one bounded surface for one skeptical audience and reports raw perception evidence. Standing procedure is the Vivi role charter, plus a hater skill if the workspace has one. | Judge merit, inspect hidden rationale, implement, audit, create tasks, or block acceptance or launch. |
| **Head** (`head-ceo`, `head-cmo`, `head-cpo`, `head-cso`, `head-cto`, `head-cxo`) | Advises on a strategic question; the Mind may select a smaller council of Heads for the periodic phase-close strategic architecture review (see [Feature lifecycle](#feature-lifecycle-and-phase-review)). Standing persona lives on the Vivi **role charter**. | Lower goals, implement, prepare tasks, merge, block production, or re-run product/device verification. |
| **Cadence** | Periodic board/schedule/occupancy/registered-goal review. Looks ahead past the current stage (`pull_forward`). Forms a ranked opinion of what Mind should run. Files **mail** to Mind (and a **need** only for new must-do work). Suggests memo cleanup and inbox absorb when unabsorbed Mind mail is ≥ 20. See [Cadence](#cadence). | Dispatch, implement, lower, audit, file memos, absorb mail, arm loops, talk to the operator, or spawn any other seat. |
| **operator@** | Handles human escalations. | Provide routine status updates or route work. |

### Standing charters (spawn / unit resume)

Auditor, Planner, Hater, Head, and Cadence seats carry **full standing procedure on the Vivi role charter**. The Mind does not re-invent their job in the task body.

On every spawn or [unit resume](#unit-resume-worker-seats-only), the Mind
sends only the [dispatch-contract](#dispatch-contract) pointer. The worker
then loads standing law from the charter:

```bash
vivi role show <role> --project "$ROOT"
vivi task show <handle> --project "$ROOT"
```

| Seat | Charter must include | Task body must include |
| --- | --- | --- |
| **Auditor** | Freeze → inventory → lenses → **evidence-honesty** verdict schema (Rule 6); re-execute only when named | Handle, repo, base/head (or tip after commit), paths, risk reason; `audit_mode: evidence_honesty` unless a single named `re_execute` command is justified |
| **Triage** | Read-only bounds, repository-is-truth law, commit-ancestry checks, requirements checklist, per-kind disposition vocabulary, escalation table, report schema, refusals | The item handles (or a slice definition), and the report path if one is wanted |
| **Planner** | Goal-forge, goal-check, delivery unit fields, refusals | Goal path or description, repo root, planning scope |
| **Hater** | Cold isolation, raw report schema, zero authority | Exact surface, exposure limit, one audience with priors, excluded context |
| **Head** | Lens, report schema, boundaries | Question or assignment only |
| **Cadence** | Observe sources (including `vivi goal list`), process catalog, **`pull_forward` lens**, mail body schema, completion stub, refusals | Tick time only (scheduler fires have no inbound task) |

**Anti-pattern:** a Mind task body that says "check for bugs and sign off" or "re-run the full suite" is not an audit. The auditor charter forbids GO stamps, requires a frozen range, and defaults to evidence honesty (Rule 6).

All role reports go to the Mind. If a role needs something from another role, it asks the Mind to route the request.

### Using roles

The Mind uses the full role system through Vivi:

- Roles have memos, capacity records, and mailboxes. Capacity includes `provider`, `model`, and `thinking`. The Vivi `model` field is a **band** (`P2-S0`), not a slug. `provider` and `thinking` stay empty. The band is not liveness.
- File **tasks** to the seat that will execute, then **spawn** that role with
  the [dispatch-contract](#dispatch-contract) pointer. Put scope and
  instructions in the task body or a linked file — never in the spawn
  prompt. File **needs** and **wants** to the Mind (or the backlog owner)
  when discovering work; the Mind lowers them into tasks before spawn.
- Hands execute and report back. They close tasks and reply with their result.
- Review is optional in the simple loop but required when the planning or wave protocol calls for it.
- Planners lower goals. A raw goal goes to a Planner, not a Hand.
- Heads advise. They do not implement or lower goals.
- Haters provide cold perception evidence. Run each pass in a fresh context and send its raw report to a separate Head for merit extraction.

Hands, Haters, and Cadence do not use memos. Vivi permits memos for every identity, but memos are reserved for durable context used by the Mind and Heads. Cadence cites existing Mind memos for cleanup; it does not file its own. A Hater's ignorance of durable internal context is part of the test.

**A memo is long-term memory, not a tracking sheet.** It carries what stays true across sessions and cannot be re-derived: standing rulings (the operating mode, operator holds), campaign constants, posture that still matches the board, and role context a seat can read nowhere else.

It carries **no** short-term state. Specifically, never:

| Never in a memo | Authority to read instead |
|---|---|
| Handles, or which handle depends on which | The backlog graph — `vivi graph show`, `vivi graph ready`, `vivi board --graph` |
| Which tasks are in flight, or which seats are live | The board, plus a live-process check (Rule 2) |
| Loop or scheduler ids, intervals, `last_fired`, `enabled` | The host scheduler |
| Status or completion state for work | The handle itself — open, closed, absorbed |
| Timestamps of in-flight events (boot times, dispatch times) | The handle, or the audit range it belongs to |

Recording a dependency list or a set of in-flight handles in a memo is the
clearest form of this defect, and it is always unnecessary: the graph holds the
dependencies, the board holds the open work, and `vivi boot` prints both at
startup. A memo whose subject names a handle, a loop id, or a task status is
defective paper — delete it and read the authority. A memo is also not the
place to work around a surface that does not exist: if the dependency or status
you want is on neither the board nor the graph nor a probe, that is a missing
surface to raise, not a memo to write.

### Model P and S (Mind spawn pick)

A role's Vivi `model` is a **band** (`P2-S0`), not a provider slug.
Quality is **model P** (lower = deeper). Speed is **S** (lower = faster
decode). These are not planning P1 Forge / P2 Check / P3 Delivery.

On spawn, Mind reads the role band and resolves it from the **host's
model inventory** (a model-availability skill if the workspace has one,
otherwise the host's configured slugs). Pick the cheapest **healthy**
slug in that band. Throttle, 429, or session death → next slug in the
same band, same handle. Climb P only if the whole band is dead. Clear
`provider` and `thinking` on the role; do not write a slug back into
`model`.

A throttle is a **present** condition, not a property of a band. Check the host's
quota signal before treating a band as unavailable — a rate limit observed an hour
ago says nothing about now — and prefer moving to another pool inside the band over
parking the work. An exhausted **quota** and a saturated **concurrency** ceiling (too
many live seats on one provider) are different constraints: the first changes which
slug you pick, the second only how many, and neither is a reason to leave dispatchable
work undispatched.

| Role | Band | Per-task `band:` |
| --- | --- | --- |
| Mind | `P1-S2` | none |
| Head | `P0-S2` | none — always P0 |
| Planner | `P1-S2` | `P2` for a low-complexity requirement |
| Auditor | `P1-S1` | `P0` for a complex or risky range |
| Triage | `P2-S1` | none |
| Hand, test, e2e, merge, release, canary | `P2-S0` | `P1` for a complex or architectural unit, or after a failed unit |
| Lint, docs, website, Hater | `P3-S0` | `P2` if the surface is a product gate |
| Cadence | `P4-S0` | none |

A band names a model **at an effort**, not a model: the same model at low and
at xhigh sits in different P bands, so resolve a band to a slug *and* effort
from the host's inventory. The role band is the default. The Mind overrides it
for one assignment with an optional `band:` field in the task body: a bare P
(`band: P1`) keeps the role's S, a full band (`band: P0-S2`) sets both. The
Mind sets it when filing, from the unit's complexity; the seat never chooses
its own. The Mind and Heads take no override.

Using a mid-P high-decode slug as a Head is a quota exception, not the
Head band. Project bans live in that project's agents doc.

### Offline fallback

If the WAN is down or reconnecting is unavailable, Mind does not wait
for cloud slugs. It switches new spawns to the **host's configured local
model**.

1. Each Mind turn that will spawn, and Cold/Warm boot, do one short
   network check if the host provides one. The result is per-turn state and
   belongs nowhere but the turn — do not record it in a memo.
2. `state=offline` → every new spawn uses the host's local model. Same
   handle, same write scope. Accept slower decode. If no local model is
   available, say so and do not pretend work is running.
3. Do not kill an in-flight seat to switch models. Let it finish or
   die; the next spawn on that handle uses the current posture.
4. Recheck on the next Mind turn (and whenever the operator messages).
   Do not arm a cadence loop for this. Cadence cannot fire if the host
   is offline.
5. `state=online` again → new spawns resolve the role band as usual.

This is a fallback, not a new role. Offline work is still Tugboat.

## The Mind's loop

The list below is a priority order across turns, not a checklist to drain in
one turn. On each turn, the Mind performs the short observation pass (steps
1–4), chooses **one** routing branch from step 5, emits the receipt, and
releases the operator channel.

**Observation is bounded and named.** The pass reads fixed-shape surfaces,
not raw dumps: `vivi step --json` (the dispatch/exception manifest over the
backlog graph), `vivi board --process --graph --json` when liveness matters,
`vivi mailspace watch --once --write-cursor` for deltas, and `vivi task show`
on the one handle selected for work. See [Context economy](#context-economy).

1. Reconciles execution state under Rule 2: **running** (live process), **dispatching** (handle + spawn this turn), **idle** (no process), or **spawn debt** (executable open handle without a live owner). Use the project's occupancy signal when it has one.
2. Inventories the Mind board by kind ([Board kinds](#board-kinds)): open **needs** (priority backlog), open **wants** (deferred backlog), open **tasks** (active assignment / spawn debt), and **mail** (communication to integrate). Drain needs before wants.
3. Consumes the step manifest: `dispatches` are the ready independent implementation units (readiness is computed from the graph, including later-stage and cross-goal items whose named dependencies are met — the `pull_forward` fact); `exceptions` route by reason — `want_requires_promotion` (promotion stays a requested action), `lowered_awaiting_units` (dispatch the units, not the parent), `no_done_when` (back to the Planner), `item_missing` (hygiene), `not_settled` (settle first). Unlowered goals, completed frozen ranges eligible for audit, and spawn debt complete the inventory.
4. Checks operator/role **mail**, dirty files; classifies dirt A/B/C. Integrate mail into needs/wants/tasks when it discovers work; do not leave must-do items as chat-only. For every claimed blocker, name the exact affected dependency boundary and separately inventory unaffected work. A blocker with no live repair/diagnostic/decision owner is the highest-priority spawn debt (Rule 4).
5. Chooses the first material branch below and performs only that coherent batch:
   - **Returned reports:** integrate one completed batch and route only its immediate repairs or successors. Integration means Rule 6 reconcile of paper (handle, commits, scope, validation claim) — plus advancing the goal/campaign status records for the landed units (Mind-owned lifecycle, Rule 1) — not re-running product tests. Dispatch a recoverable blocker to the correct Hand, Planner, Head, or project integration seat; unaffected seats remain eligible. Deferred follow-ups become **wants**.
   - **Open needs:** lower the highest-priority compatible need into assignable task(s), then dispatch that coherent burst. Needs precede wants. A blocker need requires a live owner.
   - **Ready implementation:** dispatch the current independent READY batch across idle Hands.
   - **Thin implementation frontier:** dispatch one coherent goal/child batch across idle Planners.
   - **Reviewable planning:** dispatch the current settled delivery-spec batch to idle Auditors. Implementation audits still wait for the named stage/phase boundary, a cadence recommendation, or a declared risk gate.
   - **Eligible wants:** when needs are clear and a want's preconditions are met, admit and dispatch one coherent want batch.
6. Emits the saturation receipt and **ends the turn**. Open needs without a disposition are not "quiet board." Unresolved spawn debt is either dispatched or named with a recheck. Unchosen eligible branches are next-turn work, not permission to keep this turn open.

Haters are demand-driven advisory probes, not throughput lanes. Do not invent Hater work to keep a seat busy or list an idle Hater as starvation.

A cycle is incomplete until every material signal has a disposition: acted, delegated, escalated, or validly deferred.

Seat discipline (Rules 4–5): keep every available seat full across successive
Mind turns, while each turn fills only its chosen coherent batch. Dispatch the
READY Hand batch, blocker-repair batch, planning batch, or delivery-audit batch
selected by step 5, then release the turn; take the next
eligible branch on the next wake. Do not wait for an implementation audit or
one repair before dispatching independent work. Only a named boundary (shared
write surface, chain dependency, operator decision, or declared phase-audit
cutoff) permits the affected seat to stay empty across turns; it does not idle
unaffected seats. Apply Rule 2 to every capacity claim.

Check the board each turn. A valid saturation receipt permits the Mind to end
the turn; it never permits the Mind to park on a wait tool. `wait_valid: true`
means the remaining idle capacity is genuinely blocked after recoverable
blockers have live owners and unaffected work is exhausted. It never means
“the Mind found an inconsistency and stopped.”

### Mind turn discipline: never block the operator

This rule is MIND-ONLY. It protects the operator's ability to send a message
without waiting minutes for the Mind to finish waiting on background work. It
does NOT apply to worker subagents — a Hand, Auditor, or other worker must run
its unit synchronously inside its own session and report through its Vivi
handle before ending its turn (a spawned subagent has no "next wake"; ending
the turn kills the session, and an unreported run is no completion under Rule
2). Only the Mind releases its turn early, and only because the operator can
always message again.

**The operator channel has priority over throughput bookkeeping.** The Mind
must yield often enough for the operator to correct direction while work is in
flight. Do not try to finish a whole Tugboat cycle, drain every completion, or
keep every newly discovered follow-up inside one host turn. Preserve the rest
in Vivi and continue on the next wake.

**One coordination quantum per turn:** after the first dispatch burst, or
after integrating one coherent batch of completed reports and routing its
immediate successors, emit the short status/saturation receipt and end the
turn. Do not poll any child launched in that turn. Do not begin a second
observe → dispatch → wait → integrate cycle in the same turn. If the host can
accept steered operator input at tool boundaries, absorb it before any further
routing; operator correction supersedes the queued local plan.

| Rule | Do | Never |
|---|---|---|
| **Spawn async** | Spawn so the call returns immediately with a task/child id | Blocking spawn that holds the Mind turn until the child finishes |
| **End the turn** | After the one dispatch/integration burst, report ids + what is in flight, then **end the turn immediately** | Keep routing, polling, or housekeeping because more work could fit in the same turn |
| **Never poll launched work** | Consume completion notifications on a later turn | Any `wait`, `join`, output poll, or sleep for a child launched in the current turn, even with a short timeout |
| **No long foreground tools** | In Mind mode, delegate builds, tests, network work, broad scans, and servers to a worker seat. In [Direct](#operating-modes), the Mind may run the product edit and a narrow check in this conversation; still spawn long suites async | In Mind mode, run a foreground command that may take more than a quick local read or control-plane write |
| **Allowed sync** | Only the short dependent calls needed to form one dispatch or integrate one returned batch | A second cycle, broad board hygiene, opportunistic review, or "one more check" before release |

**Done shape when children are in flight:** name the subagent/task ids, their
goals, and how you will pick up results on the next wake or operator message —
then stop talking and release the turn. Do not park on wait tools.

**No-child done shape:** if the Mind only oriented or reconciled paper, report
that result and release the turn. If the frontier appears blocked, first apply
Rule 4: dispatch the recoverable repair/diagnostic/decision owner and inventory
unaffected work. Report “blocked” with no child only for a true deadlock that
meets Rule 4's four conditions and names the operator/external gate. A quiet
board is not permission to hold the channel open. Scheduler wake-ups and
completion events start later turns; the Mind does not remain resident waiting
for them.

**Why:** a blocked Mind cannot receive operator steering or scheduled-loop
messages; it freezes multi-agent coordination. Completion is an event for a
later turn, not a reason to hold this one.

**Worker subagents are the opposite:** they must NOT release the turn until
their unit is complete and reported. A worker that ends its turn early (for
example, backgrounding a long suite and exiting "to report later") has no
session left to report from — the run is lost. Long-running validation runs
synchronously inside the worker's own session.

## Context economy

The Mind's context is capital on every host — hosted Minds pay in latency
and compaction loss; local Minds pay in decode speed and rebuild minutes.
The law is the same for both.

**Three classes of held context:**

- **Law** — this skill and the posture card. Capped and stable.
- **Picture** — open handles, readiness, liveness, dirt. **Derivable.**
  Re-derive per pass from the bounded reads below; never accumulate it in
  chat or memory.
- **Proceedings** — tool output, report bodies, the operator conversation.
  The growth region; the only class that legitimately occupies context
  long-term.

**The rules:**

1. **The picture comes from named bounded reads.** `vivi boot` is the frame
   read — at boot, at reorientation, or any time the whole picture is the
   question; `vivi step --json` is the intake;
   `vivi board --process --graph --json` when liveness matters;
   `vivi mailspace watch --once --write-cursor` for deltas; `vivi task show`
   on the one handle selected for work. Raw dumps and graph exports are
   audit surfaces, not loop intake. Reading a raw listing where a bounded
   command exists is the defect, whatever the host.
2. **Pointers and verdicts, never bodies.** The dispatch contract bounds
   what goes out; the settle flow bounds what comes back — children settle
   with fixed-field receipts, transitions record `step_decision` events,
   and the Mind holds handles plus the manifest, not payloads. A report
   body over a cap lives on its Vivi handle and is read by the seat that
   needs it.
3. **One posture memo, rebuilt on the watermark — never at the window
   edge.** The card carries only what is genuinely Mind-held and durable: the
   standing posture, operator holds, campaign constants, and the memo's own
   rebuild watermark. Live handles, branches, readiness, audit tips, and armed
   loop ids are the Picture and do not belong on the card; they are re-derived
   each pass from the board, the graph, and the scheduler. Riding the
   context window to its edge turns a bounded rebuild into an expensive
   one on every host that has compaction.

Enforcement of these rules is host-side (hooks where the host supports
them); the prose is the law, not the gate.

## Handoff protocol

Dispatch under Rule 2, let the role execute, and require its report through the
same handle. Full instructions live in the Vivi item. The runtime prompt is
only the [dispatch contract](#dispatch-contract). A long spawn prompt is a
protocol defect.

### Mind to role

The Mind creates one of these items:

- A **task** for work that is being assigned now (done-when, write scope, owner).
- A **need** when discovering or accepting must-do-soon work that is not yet an
  active assignment (priority backlog; often filed *to* the Mind by operator or
  Heads, then lowered into tasks by the Mind).
- A **want** for deferred backlog work until a precondition or spare capacity.
- **Mail** for a question, report, or other communication.

The Mind sends the item to the correct class role, such as `hand`, `planner`, `auditor`, `head-ceo`, `hater`, or `operator`. The Mind must not widen, replace, or add requirements through chat or through the spawn prompt. If the assignment is incomplete, fix the Vivi record and spawn again. Do not “clarify” in the opening message.

Do not route roles by implication. Create a separate Vivi handle for each transition in the implementation flow:

```text
raw goal
  → Planner: goal-forge / goal-check / P3 delivery
  → Auditor: delivery audit
  → Mind: admit or return for lowering
  → Hand: implement admitted units and unblock successors
  → Mind: file the integration task — a project merge/release seat lands the commit
  → stage/phase cutoff: freeze aggregate range
  → Auditor: aggregate implementation audit — also at goal and sub-goal close
  → Mind: close, record residual debt, or route blocking repair — gated on that
    verdict or a recorded waiver ([audit breakpoints](#audit-breakpoints-and-the-close-gate))
```

**A commit is not a landing.** Where the project separates integration from
implementation (a merge or release seat), the Mind files that task as part of the
completion reconcile (Rule 6), exactly as it files the next unit. A checkout or lane
released while its work is still ahead of the integration branch is **merge debt**:
unowned, invisible on the task board, and it does not resolve itself — an orphaned
commit sits behind a released lock and nothing in the loop will look for it. A seat
that closes with unmerged work names it as merge debt in its report, with the branch
and commit, so a Mind without that seat's context can act on it.

The Planner produces the delivery artifact; it does not file Hand tasks. The Mind owns admission and creates Hand tasks only from an audited delivery unit. The Auditor receives either a settled delivery specification or the frozen aggregate range at the named phase boundary, never an informal request to "check it."

Every implementation handoff must identify its current state and predecessor handle. A Hand task must point to the delivery-audit result; a phase audit task must point to the aggregate Hand commit receipts and frozen range; a repair task must point to the Auditor's finding. This makes the lineage reconstructable from Vivi without making each unit wait for review.

A Hand's task body is labeled fields, not a narrative and not a spawn brief:

- `goal` or `delivery`: path to the goal/spec;
- `unit`: the exact reference id in that document;
- `predecessor`: prior handle and commit, when lineage matters;
- `write_scope`: files this logical change may touch;
- **the edit**: which function/seam changes, in one or two lines — not a research question;
- `done_when`: acceptance for **this** change;
- optional `band`: a model-band override for this unit (`P1` for a complex
  unit; see [Model P and S](#model-p-and-s-mind-spawn-pick)) — omit it for the
  role default;
- optional `sanity`: one focused check of the touched surface;
- `do_not`: surfaces and units this bag must not touch;
- optional `check-first`: if already true, close with evidence.

The worker reads those fields from `vivi task show`. The spawn prompt does not repeat them. A bag that asks the Hand to re-derive architecture, re-read the campaign, or "figure out how this compiles" is unfinished lowering, not a Hand assignment.

Do **not** put the project's integration, suite, or release gate on the Hand. Do not paste the delivery spec. Do not assign a whole theme because landing must be atomic — file the integration gate separately.

If the body is missing a pointer, covers several families, assigns a project-wide gate to the Hand, or requires rediscovering the unit, the Hand refuses and Mind sends it back to a Planner. A Hand does not infer architecture from a raw campaign goal. A Hand that cannot start changing `write_scope` files stops and files a **need** — it does not tour the tree.

### Role to Mind

Every role reports to the Mind through the handle it received. Tone matches
the record: labeled fields and short receipts, not essays.

A Hand completes a task by:

1. Changing only files in its write scope.
2. While coding: narrow checks only (Rule 6 V2).
3. After the last product edit: optional one sanity check of this change (Rule 6 V3). Not a project-wide gate.
4. Committing its work.
5. Settling the task with `vivi task done --verdict <v> --repo <repo> --tip <sha>`
   — the receipt flags are the validation claim the record carries; the item's
   graph node completes and dependents unlock mechanically.
6. Replying one line: `done handle <handle>. commit abc123. unit <id>.`
7. **Stopping** (Rule 6 V4). The seat turns over. No further tests after done.

On the settle notification, the Mind (or host wrapper) runs
`vivi step --apply <handle> --project $ROOT` once: idempotent, never settles
work itself, records the `via=step-apply` decision, and — when a judgment
provider is configured — screens the receipt into the calibration corpus in
shadow. The Mind's completion reconcile is then reading the step decisions
and exceptions, not re-deriving readiness.

A blocked Hand is also one line (what blocked, what follow-up). If the bag is
wrong, file a **need** with must-do / repro / boundary, then stop.

An Auditor closes with `clean_pass`, `residual`, `block_range`, or `block_ship` (delivery:
`admitted` or `revise`). The reply on the assignment handle is a **doorbell**:
verdict, frozen range, mail handle of the full report if the body is long.
Put the YAML report on that handle when it fits, or as a second mail to Mind
that cites the assignment handle. Do not leave the only evidence in the host
completion text.

A Hater replies with a hostile-cold-read report and `authority: none`. It does not add merit classifications or suggested fixes.

A Head answers a named fork with a **ruling** (option letter + next seat), not
a strategy essay. Discovering a defect is a **need** to Mind, not a memo. It
does not report directly to the operator.

Cadence reports by filing **mail** to Mind (see [Cadence](#cadence)). A tick
has no inbound task. The host completion text names the mail handle. Mind
shows that mail before acting.

If a role finds work that must happen soon, it files a **need** to the Mind
naming that work. If the work is real but can wait or has a precondition, it
files a **want**. If it needs a human or Mind *decision* (not the work itself),
it files **mail** with a default and options. It then moves to other open work.

A role that finishes without replying to its Vivi handle has not finished the unit. A runtime notification is a wake signal, not durable completion. The Mind reconciles every runtime notification to its Vivi handle before advancing.

### Refusal is part of the protocol

Refusing an improper assignment is correct behavior. A role refuses when, for example:

- the Mind sends a task without a goal/spec path and unit id;
- the Mind sends a task body that is missing a pointer, covers several families, or assigns a project-wide gate to the Hand;
- the Mind sends a Hand a raw goal or a delivery unit without a delivery-audit result;
- the Mind sends a Hand a bag that requires rediscovering architecture, re-verifying the goal, or surveying sibling crates before any edit (that is Planner work; the Hand refuses);
- the Mind sends an Auditor a mutable checkout, an uncommitted diff, or a review request without a frozen aggregate base/head and path scope;
- the Mind sends an Auditor a default "re-run the full suite / all device gates" without a named `re_execute` command and `block_ship`-class reason (Rule 6 V6);
- the Mind treats a `block_ship` finding as acceptance, treats a minor `residual` as a reason to stop unrelated work, or stops the whole stream for a `block_range` instead of the boundary it names;
- a Head is asked to implement or to re-run product/device verification; or
- a Hand is asked to merge work.

The role states why it refused. The Mind then corrects the process by routing the work to the right role, preparing a valid task, or making the merge decision. No role overrides a refusal. A blocked role files a **need** (must-do follow-up), a **want** (deferred), or **mail** (decision request) as appropriate, and pivots; it does not silently stall.

### Handoff example

```bash
# 1) File the work in Vivi first — this mints the handle.
vivi task send --project "$ROOT" \
  --from mind --to hand \
  --subject 'unit U-17: parser input validation' \
  --body 'goal: <goal-doc-path>
unit: U-17
predecessor: task-handle-prior commit abc111
write_scope: crates/parser/src/validate.rs, crates/parser/tests/validate.rs
edit: reject malformed parser input at validate.rs::parse_input
done_when: malformed parser input is rejected with the expected error
sanity: cargo test -p parser --test validate
do_not: lexer; release gate
check-first: if already true, close with evidence'

# 2) Spawn with the pointer only. Filing does not start a Hand.
# Entire opening prompt:
# You are role hand.
# Load charter: vivi role show hand --project "$ROOT"
# Load task: vivi task show task-handle-abc --project "$ROOT"
# Unit resume only. Follow Tugboat. Report through that handle. Stop.

vivi task done --project "$ROOT" --for hand task-handle-abc \
  --note 'commit def456. paths crates/parser/src/validate.rs crates/parser/tests/validate.rs.'

vivi mail reply task-handle-abc --project "$ROOT" \
  --from hand \
  --body 'done handle task-handle-abc. commit def456. unit U-17.'

vivi task send --project "$ROOT" \
  --from mind --to auditor \
  --subject 'audit phase P-4 implementation' \
  --body 'audit_mode: evidence_honesty
trigger: phase
predecessors: task-handle-abc, task-handle-xyz
base: abc111
head: phase-tip-789
paths: crates/parser/src/validate.rs, crates/parser/tests/validate.rs
receipts: hand closeout notes on those handles
re_execute: none
verdict: clean_pass | residual | block_range | block_ship
do_not: implement; full ladder'
# Spawn auditor with the same four-line pointer, handle of this task.
```

## Feature lifecycle and phase review

An individual feature runs a fixed role cycle; multiple features compose a **phase** of work within an overall goal. This is the standard path for any feature (for larger work, the serial gates in [Planning phase](#planning-phase-for-larger-work) map onto steps 1–2).

### Per-feature cycle

1. **Planner** forges the goal (goal-forge) → **Mind** verifies intent.
2. **Planner** lowers to delivery (goal-check → P3 delivery) → **Auditor** verifies the delivery spec.
3. **Mind** admits each audited delivery unit → **Hand** implements and commits it. Completed units may unblock successor units immediately.
4. At the named stage/phase boundary and at goal close, **Auditor** reviews the aggregate frozen commit range → **Mind** closes the phase or goal, or routes only blocking repairs. A goal may not be closed without that verdict or a recorded waiver ([audit breakpoints](#audit-breakpoints-and-the-close-gate)).

The Mind routes each step and integrates the result. An Auditor never issues a GO stamp; closing is the Mind's call.

The two Auditor handoffs are different and must not be collapsed:

- **Delivery audit:** the Auditor checks a settled P2/P3 specification before implementation — scope, dependencies, acceptance criteria, and validation against the live repository. Its result is `admitted` or `revise`; it does not inspect an implementation that does not exist.
- **Implementation audit:** at the named stage/phase boundary, the Mind freezes the aggregate exact base/head (or commit set) and paths, attaches Hand closeout receipts, then sends that range to the Auditor under Rule 6 (`audit_mode: evidence_honesty` by default). The Auditor returns `clean_pass`, `residual`, or `block_ship` with evidence. A mutable worktree, an uncommitted diff, or an unspecified tip is not an audit target. Per-unit audit is an exception for a declared high-risk gate, not the default flow. The same frozen-range pattern is **required** on every [Warm boot](#warm-boot-mind-only-post-compaction) (post-compaction reacquaintance, with a paired CTO strategic pass). When [Cadence](#cadence) recommends `auditor_range`, Mind dispatches the same audit without waiting for a formal phase cutoff. The Auditor does **not** re-run the Hand's full validation ladder or real-device suite unless the task names one targeted `re_execute` command and a reason that cannot be settled from artifacts.

The implementation audit is not a permission for the Auditor to edit. A `residual` finding is normally non-blocking: the Mind records it as audit debt, creates a bounded follow-up task, and allows downstream work to continue when the dependency graph permits. **Not re-running a project suite the Hand receipt already records is not itself a residual.** The Auditor must mark a finding `block_ship` only on the tier test in [verdict calibration](#verdict-calibration-and-the-repair-loop): a named release invariant, or a false gate a downstream consumer would rely on, with the consequence of shipping stated. A defect that is merely wrong, confined to named paths, and recoverable is `block_range` on those paths, or `residual`. For `block_ship`, the Mind creates a repair task containing the finding, affected paths, and revised done-when condition. The Hand commits the repair and re-runs **only** the failed declared validation for that repair; the Mind schedules a targeted re-review of the repair diff alone, never a re-audit of the range; a minor repair does not automatically reopen the whole implementation cycle, and rounds are capped ([verdict calibration](#verdict-calibration-and-the-repair-loop)).

When a feature has a first-contact marketing surface, the Mind may run several Hater passes concurrently with different audiences. Each receives a fresh context and the same frozen exposure boundary. Raw reports go back to the Mind, then to `head-cmo` by default for separate merit synthesis. `head-cpo` may assess product-value implications and `head-cxo` may assess complexity signals. Neither the Hater pass nor its synthesis is an automatic implementation gate.

### Phase-close strategic architecture review

When a phase's implementation and aggregate tactical audit are complete, run a strategic architecture review before **admitting or committing to** the next phase's features. The same review is required at goal close even when no phase was ever declared, so the strategic lens cannot be removed by an undeclared tactical one. The current phase may be operationally complete, but the transition is not final until this review has a disposition. Discovery, provisional candidate selection, code-fact research, and draft lowering should already be running while the current phase implements.

- **Who:** a smaller council of Heads chosen by the Mind (the executive team — any subset of `head-ceo` / `head-cmo` / `head-cpo` / `head-cso` / `head-cto` / `head-cxo`).

  **Differentiator:** Heads review **strategically**; Auditors review **tactically**. The Auditor asks whether the implementation satisfies the current intent and acceptance criteria. The council asks whether the architectural commitments made by the implementation preserve plausible future product paths.
- **Inputs:** the original vision and current phase intent, the implemented architecture and dependency graph, the tactical audit, current product direction, and plausible future capabilities or operating conditions. Future capabilities are review inputs even when they are not written in the goal and are not in scope for implementation.
- **Questions:**
  - What assumptions, abstractions, protocols, or data shapes did this phase make durable?
  - Which likely future requirements would those choices make expensive, awkward, or rewrite-prone?
  - Are the important extension seams in the right places, or did the vertical slice accidentally hard-code a narrow case?
  - Is there a small architectural correction that is cheap now but would be disproportionately expensive after later phases land?
- **Non-expansion:** the council identifies future-facing risks; it does not turn every plausible capability into current implementation scope. A concern must identify the present commitment, the future pressure, the affected boundary, and the reason the risk deserves action or explicit recording.
- **Output:** the council reports a disposition for each material concern: `proceed`, `record_risk`, `correct_before_next_phase`, or `reopen_phase`. Each recorded risk names its trigger, affected architecture, owner, and recheck phase. `correct_before_next_phase` creates a bounded architectural task; `reopen_phase` is reserved for a material direction failure or a likely rewrite that the current boundary can still cheaply prevent.

The Mind integrates the council's report into the next phase plan and records the disposition before admitting or committing the next phase. The review is not a second Auditor pass and not a third product-verification pass (Rule 6 V8). The council does not implement, lower goals, issue a code-level sign-off, or re-run suites, device gates, or project ladders; it consumes receipts and tactical findings.

**Council dispatch:** dispatch every selected Head under Rule 2 with
full tool access if the seats need shell. After Mind compaction, apply
Warm boot before relying on any prior council state.

## Planning phase for larger work

Use this phase when work involves multiple Hands, multiple repositories, shared hot files, or a strategic goal that is too broad for direct tasking.

The Mind opens a decomposition frontier and sends the strategic goal to Planners. Planners break it into candidate child goals. Each candidate must be small enough for one independently verifiable **logical change** and must define:

- one logical change (not a theme, not a five-line process novel);
- a bounded write scope;
- explicit acceptance criteria;
- no hidden intra-unit phases (those are more Hands or an integration gate);
- whether the commit is integrable alone.

The admitted ready floor scales with usable Hands and Planner lead time. A fixed candidate count is not a capacity target. Independent candidates should be lowered by different Planners concurrently; ownership remains singular per candidate.

Accepted candidates pass three serial gates:

1. **P1 Forge** defines the intent, outcome, boundaries, non-goals, and decision owner.
2. **P2 Check** defines the done-when criteria, exact validation commands, dependencies, allowed read and write paths, forbidden paths, and factual claims that require an audit.
3. **P3 Delivery** defines the ordered executable units, scopes, dependencies, done-when conditions, and validation methods.

An Auditor independently checks the P2 and P3 outputs against the live code, tests, and named authorities. Required delivery-spec findings are corrected and checked again before admission. This is the normal quality gate; do not repeat it after every Hand unit.

Already-lowered work may skip this phase only when its intent, code facts, dependencies, write scopes, and validation commands are still current.

## Implementation phase for waves

When the planning phase admits multiple units, run them in bounded waves. A wave has these stages: prepare, launch, flow, drain, freeze, and close.

Declare the cutoff before launch by admitted set, deadline, or explicit operator decision. Do not declare a discretionary cutoff while ready eligible work and usable capacity remain. A systemic failure may stop its affected family with a recorded reason.

- The Mind prepares a burst that fits available capacity and uses non-overlapping exact write scopes. Same-repository work may run concurrently when paths, hot modules, lockfiles, and generated artifacts are disjoint.
- The Mind routes completions, Planner refills, and dependency-unblocking work through the event-driven flow loop. It defers the aggregate implementation audit until the declared cutoff unless a risk gate says otherwise.
- Each Hand executes one admitted unit at a time.
- For each completion, the Mind reconciles its Vivi handle, commit receipt, scope match, validation **claim**, and dependency impact (Rule 6). A per-unit audit result is not required for ordinary units. Mind does not re-run the unit's suite on completion.
- The declared phase audit must cover architecture, authority, security, persistence, ABI, shared-spine, and repair risks that are in scope, via **evidence honesty** against frozen commits and Hand receipts (Rule 6). Audit debt is classified as blocking or residual; residual debt is tracked and does not stop unrelated downstream work.
- A systemic failure stops the affected family, propagates to related units, and requires focused repairs.

At the cutoff, the Mind stops preparing new work and drains the units already in flight. It then freezes the wave and reconciles audit debt, aggregate findings, campaign state, and repository evidence against the accepted unit set. A freeze is mandatory for anything called a wave.

A wave freeze is not an implementation audit by itself, and a freeze may be recorded before the audit returns — that is what keeps wave bookkeeping from turning every wave into a serial audit barrier. A freeze is not a **close**: the wave's range must be audited, or waived, before the wave closes and before its goal advances status ([audit breakpoints](#audit-breakpoints-and-the-close-gate)).

The Mind creates a freeze receipt that names the baselines, admitted, landed, accepted, repaired, and excluded units; records the aggregate Auditor verdict and any residual debt; records validation evidence; and states the next posture. A phase may close with residual debt when no `block_ship` finding remains and every debt item has an owner and recheck trigger.

Hands implement, optional sanity, commit, and turn over (Rule 6). Project-defined seats own integration and suites. The Mind records the Hand's done receipt; it does not run validation itself.

Closeout reconciles wave-owned temporary state, records foreign dirt, and files maintenance work only when it is justified. After closeout, the Mind runs the [phase-close strategic architecture review](#phase-close-strategic-architecture-review) before admitting the next phase's already explored provisional candidates.

## Cadence

Cadence is the executive officer. Mind is the commander.

One Vivi role (`cadence`). One host loop (`cadence_tick`). The old
`mind_wake` / `auditor_range` / `cto_range` / `polish_analyzer` reminder
loops are retired. Their processes live in the catalog below.

The loop is **off by default**. The Mind does not arm or re-arm it. A missing
loop is the expected state. Only explicit operator enablement creates
`cadence_tick`.

**Warm boot is still the reliable Auditor + CTO trigger.** Cadence mail is
the between-compaction picture. It does not replace
[Warm boot](#warm-boot-mind-only-post-compaction) reacquaintance.

### What cadence is (and is not)

| Cadence **is** | Cadence **is not** |
|---|---|
| A periodic **board review** that tells Mind what should run | A second Mind that dispatches Hands, Planners, Auditors, or Heads |
| A **mail** to Mind (same report path as a Head) | A memo writer, a task assigned *to* Mind, or a frozen scheduler essay |
| Context-aware: it reads the live board, role `cadence` fields, schedule health, needs, wants, occupancy, Mind memos, unabsorbed Mind inbox, and **`vivi goal list`** | A baked "CTO review is due" nudge with no picture of the fleet |
| A **look-ahead**: later-stage and sibling-goal work whose named deps are already met | A serial reader of "the current stage" that waits for earlier stages to finish |

Vivi role field `cadence` (`15m`, `4h`) and board `schedule` are **sensors**.
The `cadence` seat **reads** them. It does not replace them.

### The role

Unique seat `cadence`. Prefer kind `steward`. Standing procedure lives on the
Vivi charter. Cheap model is fine.

A tick is [Unit resume](#unit-resume-worker-seats-only) if compacted mid-tick.
It is not Warm boot and not a Mind loop. Reload the charter and finish the
tick: observe → file mail if needed → emit the completion stub → stop.

### The one loop

Mind creates exactly one scheduler job after the operator enables it.
Default interval: **15–30 minutes**. Background fire if the host
supports it. Payload is a pointer, not a process novel:

```text
You are role cadence.
Load charter: vivi role charter show cadence --project $ROOT
Tick. Review. File the opinion as mail to mind. Stop.
```

Changing "what security review means" is a charter edit. Do not kill the loop
to change the catalog.

If a host can only wake Mind, the wake text is one sentence: `Cadence tick:
dispatch cadence.` Never `Auditor range review is due.`

Do not create `mind_wake`, `auditor_range`, `cto_range`, or `polish_analyzer`
loops beside this one. Do not create multiple scheduler jobs in one
parallel tool batch; some hosts overwrite earlier jobs.

The host scheduler is the only record of this loop — its id, its interval, and
whether it is armed. Nothing about it goes in a memo: a `cadence_tick` entry
in a memo is defective paper, and `vivi boot`'s loops section already reports
the declared cadence and its silence.

A loop absent from the scheduler is **disabled, not broken** — the default
state, since the loop is off by default. Do not recreate it without operator
enablement.

### A tick

1. Load charter + previous cadence→mind mail (if any).
2. Read `vivi boot`. It carries the board, every role's declared cadence and
   silence (the firing schedule the due checks below need), open needs and
   wants, Mind memos, the unabsorbed count, the registered campaign working
   set, and the project's probe facts — occupancy, where the project declares
   one. Reach for `vivi role list --json` only when you need role fields the
   digest does not print.
3. Score the [process catalog](#process-catalog) against live evidence,
   including the `pull_forward` lens over registered goals; every audit handle
   whose verdict is `block_range` or `block_ship` and whose blocked paths have no
   owner on the board (`unhandled_block`) — read the handle, since an absorbed
   verdict is still unowned; and any blocked range whose recheck trigger has
   elapsed (`stale_block`). Also score neglected needs/wants, stale Mind memos,
   and `mail_hygiene` when the unabsorbed Mind inbox count is ≥ 20.
4. Dedup against open tasks, needs, live processes, and the previous mail.
5. If something new is due, file **mail** `cadence` → `mind`. File a **need**
   only for must-do work that is **not** already a need. Cite existing
   need/want handles; do not clone them.
6. Emit the completion stub. Stop.

Cadence **does not file memos**. Suggest memo deletes in the mail. Mind deletes.
Cadence **does not absorb mail**. Count the inbox. Suggest absorb. Mind absorbs.

Quiet ticks file nothing and return `quiet: true`. That is success.

If the previous cadence mail is still unabsorbed and the ranked actions did
not change, file nothing and return `unchanged` plus that mail handle.

Cap the action list. Three `now` / `this_cycle` items is enough. The rest
stay in the mail as `later` or `already_covered`.

### Mail and completion stub

A scheduler tick has no inbound task. Do not invent a task to Mind. Mail is
the briefing. The host completion is a doorbell.

Completion text — handle first, short enough to survive truncation:

```text
cadence tick 2026-08-16T18:40:00Z
mail: a1b2c3d4
needs: (none)
quiet: false
actions: 3
```

Unchanged:

```text
cadence tick 2026-08-16T18:55:00Z
unchanged
mail: a1b2c3d4
```

Mail body (existing handles cited, no new paper for work that already exists):

```text
actions:
  - fill_lanes  now
    20 lanes, 2 assigned; planner idle
  - need 4f21aa90  now
    open 11h, no live owner, no spawn
  - want 88c0de12  later
    precondition met; still sitting
  - cto_range  this_cycle
    head-cto overdue 17h; 4 merges since last review
  - pull_forward  now
    gol_634a0417d02c510f MODEL-02 named deps met; planner idle
  - mail_hygiene  this_cycle
    85 unabsorbed in mind inbox (≥20); absorb unneeded

memos_to_drop:
  - m9aa1100  dead host_task_id map
  - m22bb3344  superseded posture note

already_covered:
  - security 09088eab (last CSO pass 1h ago)
```

### How Mind picks it up

1. Host wakes the Mind turn with the child's final message (usually a
   summary). That is a wake, not the record.
2. Read `mail: <handle>`. If the stub is missing, `vivi mail list --for mind`
   and take the latest from `cadence`.
3. `vivi mail show <handle>`.
4. Take one coherent dispatch burst, or absorb-and-skip with a reason.
5. Absorb the mail.

Cadence may suggest a role and a one-line subject. It does not write the Hand
task body and does not pin frozen ranges unless it already has the facts.
Range pinning stays Mind's (or the executing seat's) work.

A cadence mail is not a gate. Hands do not wait on it. Unabsorbed cadence
mail is the wake. A stack of unabsorbed cadence mail is a Mind defect, not a
reason for cadence to shout louder. `mail_hygiene` is the catalog form of
that defect when the whole Mind inbox (not only cadence mail) is ≥ 20.

### Process catalog

Tugboat names the default rows. A project may add rows on the cadence
charter. Skip a process when the window is empty. Do not invent work.

The detection half of `fill_lanes` (READY units), `pull_forward` (named
deps met, including later-stage items), and unlocked wants is a query, not
an inference: `vivi graph ready` / `vivi step --json` compute readiness
across kinds from the backlog graph's edges. The cadence seat reads the
computed frontier and applies judgment to what remains — ranking,
neglect-versus-valid-deferral, hygiene — rather than re-deriving readiness
by reading.

| Process | Due when | Recommend |
| --- | --- | --- |
| `pull_forward` | A registered goal (`vivi goal list`) has a later-stage, sibling-track, or other-goal item whose **named** deps are already satisfied, and usable seats are free (or Mind is treating a stage number as a gate) | File + spawn the named role. Cite `gol_*` + stage/unit. Tell Mind it can run now. |
| `fill_lanes` | Free usable seats and READY units or orphan open bags | File + spawn Hands / claim lanes |
| `planner_backlog` | Unlowered goals and planner idle | File + spawn planner |
| `spawn_debt` | Open harnessed tasks, no live process | Those handles are spawn-dead; re-spawn |
| `neglected_need` | Open need with no live owner and no disposition | Cite the need; Mind must lower + spawn |
| `unhandled_block` | An audit verdict of `block_range` or `block_ship` on an audit handle — absorbed or not, since absorption does not retire a handle — whose blocked paths have no owner: no open repair task, no debt handle, no recorded waiver | Cite the audit handle, the verdict, the blocked paths, and the missing owner. Mind files the repair, the debt, or the waiver. Do not dispatch. |
| `stale_block` | A `block_range` whose repair landed but whose blocked paths were never re-audited, and whose recheck trigger has elapsed | Cite the block handle, the paths, and the repair commit. Mind re-audits those paths or records the waiver. |
| `unlocked_want` | Want whose precondition is now true, needs are clear | Cite the want; consider promote/dispatch |
| `auditor_range` | Last **aggregate audit** tip older than ~1h **and** new commits on managed mains | File + spawn auditor (see below). Delivery audits, re-reviews, and verification reports do not advance this tip; only an aggregate implementation audit does. |
| `cto_range` | Last CTO tip older than ~1h **and** (new main merges **or** `head-cto` schedule overdue) | File + spawn `head-cto` |
| `security_review` | Last security pass older than the CSO cadence **and** (new surface or overdue `head-cso`) | File + spawn `head-cso` (or the project's security seat) |
| `memo_hygiene` | Mind memos that are superseded, duplicated, or carrying short-term state (a handle, a dependency, a loop id, a task status) | List handles to delete; Mind deletes |
| `mail_hygiene` | Unabsorbed Mind inbox count ≥ 20 (`vivi mail list --for mind --folder inbox --status unabsorbed`) | Suggest Mind absorb unneeded mail (cite count + a few sample subjects). Do not absorb. Do not file a need. |
| `polish` | Needs clear, READY campaign work clear, polish interval elapsed | Suggest analyzer only |

### Running a recommended process

Mind runs these under normal Tugboat rules after it accepts the mail (or on
Warm boot / operator request). Cadence does not run them.

**`auditor_range`.** Pin `base` = the last **aggregate audit** tip — read it
from the last implementation-audit handle on the board, never from the last
auditor mail and never from a memo; `head` = current committed tip. Inventory
paths. Dispatch **one to four** Auditors by path family, crate, or risk
cluster. File
`audit_mode: evidence_honesty`, exact `base`/`head`, paths, receipts,
`re_execute: none` unless a named `block_ship`-class reason needs one
targeted command; then **spawn**. Integrate `clean_pass` / `residual` /
`block_range` / `block_ship`. Also required on every Warm boot (`trigger: warm_boot`).

**`unhandled_block`.** Read the audit handles covering the goal's range and their
verdicts. Read the handle, not the inbox copy: absorbing the mail does not retire
the handle, and an absorbed verdict is still an unowned one. For each `block_range`
or `block_ship`, find its owner on the board: an open repair task, a debt
task or need, or a recorded waiver. File whatever is missing, carrying the
blocked paths, the audit handle, and the recheck trigger — or record the
waiver. Do not re-dispatch the audit to rediscover a verdict that is already
written down.

**`stale_block`.** For each block whose recheck trigger has elapsed, check
whether its blocked paths have been re-audited since the repair landed. If not,
re-audit those paths — not the whole range — or record the waiver. A
`block_range` closes when its paths are read again, not when no one objects.

**`cto_range`.** Same kind of window. File + spawn `head-cto` (small council
only if the window warrants it). Lens: durable assumptions, seam placement,
future-cost risks, cheap-now corrections. Dispositions: `proceed` /
`record_risk` / bounded follow-up. Does not replace phase-close council.
Also required on every Warm boot.

**`polish`.** Skip while active needs, READY campaign units, or unowned
blockers exist. If the project has a polish helper, use it to produce a
suggested path list. Mind admits and dispatches ordinary Hand polish
tasks. Analysis does not edit. If there is no helper, skip the process.

**`pull_forward`.** Cadence reads `vivi goal list` (that list is the campaign
working set; do not walk goal files on disk). For each existing
non-archived goal it reads the Status line and the stage / unit /
depends-on tables, then asks what can run *now* even if it sits in a
later stage or another registered goal. Stage numbers are routing order,
not blockers. A later item is blocked only by a named dependency (Depends
on, Gate, write-scope overlap, missing predecessor receipt, operator
hold). Recommend planner if the item still needs lowering, Hand if an
admitted unit is ready, or the project's docs/test/lint seat when that is
the work. Cite `gol_*` + stage/unit. Do not invent units. `now` only when
usable seats are free or Mind is serializing on a false stage-number
gate. Ordinary Rule 5 fill once Mind accepts the mail.

**`fill_lanes` / `planner_backlog` / `spawn_debt` / neglected need or want.**
Ordinary Mind-loop branches (Rule 5). Cadence only names them when Mind has
gone heads-down and left them sitting.

**`mail_hygiene`.** Mind absorbs unneeded inbox items (`vivi mail absorb
--for mind <handle>`), integrating any still-material signal first. Cadence
only cites the count and a few subjects. If the previous cadence mail already
recommended `mail_hygiene` and the only change is the pile is still ≥ 20,
return `unchanged`. A stack of cadence mail does not justify another shout.

## Board kinds

Board kinds are the backlog and execution surface. They are **not** synonyms
for "I need help" or "I want something from someone."

| Kind | Means | Role on the board |
|---|---|---|
| **task** | Work that is **currently assigned** (or about to be) and is being worked — a concrete unit with a done-when condition, write scope, and an owner role | Active work. Dispatch = file task + spawn the seat. |
| **need** | Something that **must get taken care of as soon as possible** — priority backlog. "This needs to happen." | Priority intake. Mind drains **needs before wants**. |
| **want** | Something **queued** to work when appropriate or when a precondition is met — general backlog, not a fire drill | Deferred intake. Work only after open needs are cleared or honestly deferred with a recheck. |
| **mail** | **Communication** — questions, findings, reports, handoffs that are not themselves the work queue | Signals and conversation. Not a substitute for need/want/task. |

Work items are **graph citizens at send**: every task/need/want mints a node
in the project's `backlog` graph, `--depends-on` (any work-kind handle)
creates the prerequisite edge, and readiness is computed across kinds. An
unlocked want surfaces as a ready want node — promotion remains a requested
action (`want promote`) and never fires because a dependency completed.
Lowering is a graph fact: `vivi need bind` attaches unit tasks, and the
need auto-completes when every unit lands.

### Who files what to whom

Operator, Heads, Cadence, or any role that discovers required or deferred work
may file a **need** or **want** **to the Mind**. Cadence usually files **mail**
and only files a need when the must-do work is not already on the board. Those
items show on the Mind's board.
The Mind then:

1. Prioritizes open **needs** (must-do soon).
2. Only then works the **want** backlog (when capacity and preconditions allow).
3. Lowers each admitted item into one or more **tasks** (or planner/auditor
   work) and dispatches the executing seat under Rule 2.

### Processing order (Mind standing law)

```text
open needs  →  drain / lower / dispatch first
open wants  →  backlog; after needs (or when a named precondition unlocks one)
tasks       →  currently assigned work; keep seats full on READY tasks
mail        →  read, integrate, absorb; do not confuse with the work queue
```

- An open **need** outranks an open **want**. Do not polish the want backlog
  while needs sit idle without disposition.
- A **want** may name a precondition ("after phase P-3", "when a hand seat is free",
  "after API lands"). Leave it open until the precondition is true; do not
  promote it early to steal capacity from needs.
- Promote a want into a need only when urgency changes (operator or Mind marks
  it must-do-soon). Promote need/want into **task**(s) when the Mind is ready
  to assign and run the work.
- **Mail** can *discover* a need or want ("we found X") but the durable backlog
  item should be filed as need or want so it stays on the board after absorb.

### What a need is *not*

A need is **not** "a decision the Mind cannot make alone." That was wrong
language. Human decisions and questions go as **mail** (often `To: operator@`
or the deciding role), with a default and options when useful. If the outcome
of that decision is work that must happen soon, file or keep a **need** for
the work itself.

## Dependency grammar

There are two kinds of "depends on" and they live in different places.

- **Structural dependencies** cite a handle: `--depends-on <task|need|want>`
  on any work-kind send becomes a prerequisite edge in the backlog graph,
  validated before the send creates anything. Readiness across kinds — a
  task waiting on a need, a want waiting on a task — is computed from these
  edges, not inferred by reading. Multi-recipient items are one work item:
  cite any copy; canonicalization and sibling completion are automatic.
- **Ambient conditions** — "when a seat is free", "after the operator
  approves", time windows, capacity — stay prose in the body and are
  evaluated by the host at dispatch. They are **never encoded as topology**:
  an edge named "seats free" manufactures false readiness. Eligible means
  `graph ready` **and** the ambient checks pass; both are required, neither
  substitutes for the other.

**Lowering is binding, not importing.** When a need breaks into recorded
tasks, `vivi need bind <need> <task>...` attaches the units and the need
auto-completes on the join (reopening a unit re-opens the parent). Imported
Mermaid graphs (`graph import` / `apply`) are for non-item topology —
pipelines, environment flows — not for delivery waves, so work is never
double-represented in two graphs.

## Vivi command reference

This is Tugboat's working subset, not a second Vivi manual. For kinds, absorb,
roles, goals, graphs, and email, load the Vivi skill
([`skills/vivi/SKILL.md`](https://github.com/ianzepp/vivarium/blob/main/skills/vivi/SKILL.md))
and current `vivi --help`.

Set `ROOT` to the project root and use `--project "$ROOT"`. Prefer class
identity tokens such as `mind`, `operator`, `hand`, `planner`, `auditor`,
`cadence`, and `head-ceo`. Numbered names are legacy addresses.

```bash
# The project frame in one read (Mind boot, reorientation, and Cadence's tick)
vivi boot --project "$ROOT"

# List open items for a class role (Mind view — workers do not do this)
vivi task list --for hand --project "$ROOT" --status open
vivi need list --for mind --project "$ROOT" --status open   # priority backlog
vivi want list --for mind --project "$ROOT"                 # deferred backlog
vivi mail list --for mind --project "$ROOT"          # messages to Mind
vivi mail list --for mind --project "$ROOT" \
  --folder inbox --status unabsorbed                 # cadence mail_hygiene count
vivi mail list --for operator --project "$ROOT"      # human escalations

# Show an item by handle
vivi task show <handle> --project "$ROOT"
vivi mail show <handle> --project "$ROOT"

# View the class board (Mind). A worker uses task show on its handle only.
vivi board --for hand --project "$ROOT"

# Registered campaign working set (Mind + Cadence look-ahead)
vivi goal list --project "$ROOT" --json
vivi goal show --project "$ROOT" gol_<id>

# Send a task to the class mailbox (file first; spawn after)
vivi task send --project "$ROOT" \
  --from mind --to hand \
  --subject 'task: implement X feature' \
  --body 'done_when: 1) … 2) …'

# Need / want → Mind backlog (operator or Head discovers work)
vivi need send --project "$ROOT" \
  --from operator --to mind \
  --subject 'need: fix auth token refresh on API 401' \
  --body 'must-do: users stuck after hour. repro: … success: refresh works; no extra login.'
vivi want send --project "$ROOT" \
  --from head-cto --to mind \
  --subject 'want: extract shared retry helper' \
  --body 'precondition: after auth fix lands. non-urgent cleanup.'

# Decision / question → mail (not a need)
vivi mail send --project "$ROOT" \
  --from mind --to operator \
  --subject 'decision: ship with residual polish debt?' \
  --body 'default: ship. options: ship | hold. residual: …'

# Reply to an existing handle
vivi mail reply <handle> --project "$ROOT" \
  --from hand \
  --body 'finding: …'

# Close or reopen work
vivi task done --project "$ROOT" --for hand <handle> \
  --note 'evidence: tests pass, commit abc123'
vivi task reopen --project "$ROOT" --for hand <handle>
vivi need done --project "$ROOT" --for mind <handle>
vivi need reopen --project "$ROOT" --for mind <handle>
vivi want done --project "$ROOT" --for mind <handle>
vivi want drop --project "$ROOT" --for mind <handle>
vivi want promote --project "$ROOT" --for mind <handle>   # when a want becomes must-do (need)

# Check for new events in one pass
vivi mailspace watch --for mind --project "$ROOT" \
  --once --write-cursor --cursor-file "$ROOT/.vivi/mind-watch.cursor"

# Bounded observation: the dispatch/exception manifest (loop intake)
vivi step --project "$ROOT" --json
vivi step --apply <settled-handle> --project "$ROOT"   # idempotent; records via=step-apply

# Dependency substrate (any work-kind handles; fail-fast validation)
vivi task send ... --depends-on <handle>               # also need send / want send
vivi graph activate <handle> --task <handle> --project "$ROOT"   # dispatch binding; bare id = backlog
vivi need bind <need-handle> <task-handle> --project "$ROOT"     # lowering join

# Memo policy: long-term memory for the Mind and Heads only — never handles,
# dependencies, loop ids, or in-flight status (Hands, Haters, Cadence: no memos)
vivi memo list --project "$ROOT" --for mind
vivi memo save --project "$ROOT" --for mind \
  --subject 'posture: growth campaign; operator holds' \
  --body 'true operator blocks only; release stays operator-run'
vivi memo search --project "$ROOT" --for mind "keyword"
vivi memo delete --project "$ROOT" --for mind <handle>   # Cold-boot cleanup: drop superseded or tracking-style memos

# Absorb Mind inbox mail after integrate (Cold boot step 2; also ordinary cycle hygiene)
vivi mail absorb --project "$ROOT" --for mind <handle> [--note '…']

# Trace communication lineage
vivi trace <handle> --project "$ROOT"
vivi mail thread <handle> --project "$ROOT"

# Audit-level task dump
vivi task dump --project "$ROOT" --status open
```

## Harness notes

The protocol is harness-neutral. Hosts differ in what a spawn can carry, what a
seat can run, and how completion reaches the Mind. This section records those
differences per harness. Machine paths, installed tool versions, and model
slugs belong in the host's own agent file, not here. A harness that is not
listed follows the protocol as written and files its own notes here.

### Claude (Claude Code, Agent SDK)

**Seats.** A seat is an `Agent` tool call. The agent *type* is a tool set, not a
role: a seat that files Vivi paper, runs git, or runs tests needs a full-tool
type (`general-purpose`, or a project-defined implementation type). Read-only
types (`Explore`, `Plan`) cannot run `vivi` or `git` and are never seats. The
role comes from the charter the seat loads.

**Spawn.** Start seats with `run_in_background: true`. The completion arrives
as a notification on a later Mind turn: do not poll, do not hold the turn, and
do not write a waiter loop. A loop of the form `until ! pgrep -f "<cmd>"; do
sleep …; done` matches its own shell and never exits; wait on a pid
(`kill -0`), a marker file, or the notification. Commands that exceed about two
minutes are moved to the background automatically; read their output file
instead of re-running them.

**Where a seat works.** The `Agent` tool takes no working directory. A seat
inherits the Mind's current directory at the moment of the call, so the Mind
`cd`s into the claimed packet or lane, then spawns that one seat. Spawn one
seat per directory, in sequence: two seats spawned in one parallel tool batch
share one directory. The harness option `isolation: "worktree"` creates a
worktree of the *current repository only*. It carries none of the sibling
repositories, so gates that read a sibling (grammar ids, host runtimes,
cross-repo test includes) cannot run there. For a multi-repository project use
the project's packet mechanism instead; keep `isolation: "worktree"` for
single-repository work.

**Model and effort.** The `Agent` call takes a `model` but no effort argument.
A seat's effort comes from its agent definition's `effort:` frontmatter, and
from the session's effort when the definition sets none. Thinking is inherited
from the session. A [band](#model-p-and-s-mind-spawn-pick) therefore resolves to
an agent type plus a model alias, and a per-task `band:` override selects among
agent types. A seat spawned from a definition that pins nothing runs at the
Mind's own model and effort: pin the definitions the fleet uses.

**Brief.** The opening prompt is the [dispatch contract](#dispatch-contract):
role, charter, handle, stop, plus the reply and close commands for the seat. The
assignment lives in the Vivi task body, and a long structured body is fine; Claude
seats work well from one. A task body carries: kind, unit and the need it
answers, the defect with the exact code or text, write scope, a proof that
demonstrates the failing case before the fix (red, then green), `done_when`,
`do_not`, and the report shape. Every body that describes a defect also carries
a **check-first** clause: reproduce it on current main, and if it does not
reproduce, or the premise is wrong, report that and stop. In practice a
meaningful share of defect briefs rest on a wrong premise, and a seat that
reproduces first catches it before it edits anything.

**Reply and close.** `vivi mail reply <handle> --from <role> --body '…'` (the
body flag is `--body`; a wrong flag costs a turn), then `vivi task done --for
<role> <handle> --repo <name> --tip <sha> --verdict <v>`. A seat reports merge
debt explicitly: a commit on a lane branch is not a landing.

**Handles.** One `send` yields a created handle and a recipient copy, held by
different identities. Claim, bind, and activate with the created handle, and
close each copy with the identity that holds it. Binding a need to a task closes
the need when the task is done, so a follow-up task needs `need reopen` first.

**Operating mode.** Declare it in the Mind memo and keep it. A Claude Mind that
also edits, runs gates, and resolves conflicts is in Direct whether or not it
says so, and its context fills with logs until compaction takes the priorities
with it. Prefer Mind mode: routing, scoping, reading receipts, and status
bookkeeping stay in the Mind; integration, ledger regeneration, and gates go to
a merge seat and the project's verification seats.

**Concurrency.** Cap parallel seats by provider quota and by what the machine
sustains, and write the measured cap in the host's agent file. Do not
estimate: time the gates once and record them.

**Shell quirks that cost turns.** The default shell may not split an unquoted
variable into words (zsh): put loops and word lists in `bash -c` or use arrays.
A heredoc carrying non-ASCII text can fail: write such scripts with the file-write
tool and a coding header. Compound commands that chain a check after a hung
step keep the hang: bound every wait.

**Compaction.** The compact summary is an annex, not the record. After it, run
Warm boot from Vivi (`vivi boot`), and keep `operating_mode`, posture, and
standing rulings in the Mind memo so they survive.

## What Tugboat keeps

- **Boot modes:** Cold boot (true restart — includes Mind absorb + cleanup of stale memos/tasks/etc. **and registry triage of the backlog**), Warm boot (Mind-only post-compaction reorient **plus required Auditor + CTO reacquaintance**), Unit resume (workers). Compaction is role-blind; seats pick the mode by identity + live infrastructure.
- **Operating modes:** **Mind** (default — route, Hands implement, correctness gates) and **Direct** (operator-named — Mind implements, sub-agents verify and keep books, working software first). Orthogonal to boot modes. See [Operating modes](#operating-modes).
- **Vivi:** required companion. Tasks, needs, wants, mail, roles, memos, and
  the board are the **record**, not the executor (Rule 2). CLI law is the
  [Vivi skill](https://github.com/ianzepp/vivarium/blob/main/skills/vivi/SKILL.md).
  Mind drains needs before wants; dispatch of work = task + spawn.
- **Role hierarchy:** Mind, Planner, Hand, Auditor, Triage (project-defined registry reconciliation), optional Hater, Head, Cadence, and `operator@`. A project may add specialized seats; Tugboat does not assume they exist.
- **Model P + S:** Vivi `model` is a band (`P2-S0`) — a model at an effort, never a model alone. Mind resolves it to a slug and effort from the host's model inventory, and may override the role band per task with `band:`. Not planning P1/P2/P3.
- **Offline fallback:** WAN down → new spawns use the host's local model. Recheck each Mind turn. Do not interrupt in-flight seats.
- **Memos:** long-term memory for the Mind and Heads only — standing rulings, campaign constants, durable posture. Never handles, dependency lists, loop ids, or in-flight status: the graph, the board, and the scheduler are those authorities. Cadence does not file them.
- **Shared-workspace rules:** classify dirt A/B/C and never erase foreign work.
- **Audit loop:** plan → delivery audit → implement and unblock → aggregate audit at every required breakpoint (goal and sub-goal close, wave or phase freeze, risk gate, interval backstop, Warm boot), never per unit → close gated on that verdict or a recorded waiver → accept or repair only blocking findings, scoped to the boundary they name.
- **Cadence:** one `cadence` seat and one `cadence_tick` loop. Cadence reviews
  the board and `vivi goal list`, looks ahead past the current stage, and
  mails Mind a ranked opinion. It does not dispatch. **Off by default.**
  Warm-boot Auditor + CTO reacquaintance is required and is not
  cadence-dependent.
- **Verification economy (Rule 6):** one owner per proof kind; Hand implements and turns over; project-defined seats own ladders; Mind paper reconcile; Auditor does not replay suites; Heads do not re-verify.
- **Seat turnover (Rule 5):** a Hand assignment is capital on a shelf until done; size one logical change so seats turn over. Do not maximize throughput by stuffing a theme into one bag, and do not mint micro-units whose process exceeds the product.
- **Vivi-first communication:** keep routing on the board, not in chat. Spawn
  prompts are the [dispatch contract](#dispatch-contract) only.
- **Backlog graph substrate:** work items are graph citizens at send
  (`--depends-on` any work-kind handle = edge; readiness computed across
  kinds); lowering is `need bind` with join completion; dispatch binds with
  `graph activate`; the Mind observes through `vivi step` and the bounded
  reads of the [Context economy](#context-economy). The authority map,
  verdict vocabulary, and close gate are unchanged — what changed is the
  cost of observation and routing.
- **Dispatch contract:** file the work, then spawn role + handle. No narrative
  brief in the opening prompt. Worker reports through the same handle.
- **Blocker routing (Rule 4):** recoverable blockers become live Hand, Planner, Head, or project-integration assignments immediately; block only their exact dependency boundary and keep unaffected seats moving. A fleet stop requires a proven operator/external deadlock, not uncertainty.
- **Seat saturation (Rule 5):** dispatch instead of idling when honest work exists. Fill every pool (planning, implementation, audit) while READY work exists.
- **Head boundary:** Heads advise. They do not lower goals or implement.
- **Harness notes:** per-harness differences (what a spawn carries, where a seat works, how completion arrives) live in [Harness notes](#harness-notes), kept out of the protocol body; machine specifics stay in the host's agent file.
- **Cadence boundary:** Cadence advises Mind via mail. It does not dispatch, file memos, or spawn seats.
- **Hater boundary:** Haters expose hostile first impressions. They do not decide merit, create tasks, or gate work.

## Anti-patterns

- **The Mind implements (Mind mode):** The Mind sees a bug and fixes it, runs the failing test, or applies the formatter "just to check" instead of filing a task for the right seat. Deep analysis done in the Mind's own context instead of delegated to a Head, Planner, or Auditor is the same defect (Rule 1). In Direct this is the job, not a defect. Mixing both — routing a Hand *and* editing the same surface — is a defect in either mode.
- **Direct without being named:** Implementing as Mind because it is faster, without the operator naming Direct.
- **Direct as a chainsaw:** Unrelated rewrites, deleting foreign work, or treating "tests can wait" as "trash the tree."
- **Direct still running the waterfall:** Filing P1–P3 and waiting on delivery audit before the first edit while claiming Direct.
- **Stale goal status:** Implementation landed, but the goal/campaign document still reads `planned`/`draft` because the Mind never advanced the status line. The Mind owns the status lifecycle; the goal inventory and the board must agree. The inverse — the Mind rewriting goal content itself instead of delegating to a Planner/Head — is the same defect from the other side.
- **No board item:** Work is assigned through chat without first creating a task, need, want, or mail item.
- **Narrative spawn prompt:** The Mind pastes the goal, the delivery spec, or a
  “here is what I need you to do” brief into the sub-agent opening message.
  That text belongs in the Vivi task. The spawn prompt is four lines: role,
  charter, handle, stop. Clarifying in chat instead of fixing the record is
  the same defect.
- **Sleeping with open work:** The Mind claims the board is quiet while open **needs**, READY tasks, or orphan task bags remain.
- **Want before need:** Draining the want backlog (nice-to-have / preconditioned work) while open needs sit without disposition.
- **Need means "I need a decision":** Treating need as a question form. Questions and human decisions are **mail**; must-do work is a **need**; deferred work is a **want**; active assignment is a **task**.
- **Freezing on one item:** The Mind waits for confirmation that it has not requested instead of filing mail/need/want as appropriate and pivoting.
- **Recoverable blocker becomes fleet stop:** A dirty checkout, merge conflict, failed gate, or uncertain next step causes the Mind to stop all work. Route a bounded repair/diagnostic/decision task with a live owner, block only the affected dependency edge, and continue unaffected seats.
- **Blocker without an owner:** The Mind records a recoverable blocker but does not dispatch the Hand, Planner, Head, or project integration seat that can clear it.
- **Polish escape from unfinished delivery:** The Mind encounters a campaign blocker and switches capacity to polish, housekeeping, or discretionary wants instead of routing the repair. Maintenance never outranks active needs, READY campaign work, or recoverable blockers without live owners.
- **False deadlock:** The Mind says operator intervention is required before exhausting agent-owned repair, merge cleanup, diagnosis, planning correction, and Head advice, or while unrelated campaign work remains eligible.
- **Hand rediscovers:** The Mind files an implementation bag whose real job is to re-read the goal, historical proofs, and compiler topology before editing. Planning and lowering already paid for that. The Hand then burns wall clock and tokens and produces no diff. If the delivery did not name the edit, send it back to a Planner — do not dispatch a researcher in a Hand seat.
- **Raw goals go to Hands:** A raw goal goes to a Planner for lowering.
- **Heads lower goals:** Heads advise. Planners lower goals.
- **Warm Hater:** Reusing a context that already knows the rationale makes the Hater too charitable; start a fresh pass.
- **Hater as verdict:** Raw hostility is treated as truth, a task list, or a launch gate instead of perception evidence for separate merit synthesis.
- **Foreign dirt is erased:** Git is used to clean files that may belong to another agent.
- **Chat is the record:** A decision or handoff is treated as real even though it was never recorded on the board.
- **Per-unit audit serialization:** Do not send every completed unit through an Auditor before starting its successors. Reconcile the completion, unblock eligible work, and defer ordinary implementation review to the aggregate phase audit or a cadence-recommended range.
- **Close without an aggregate audit:** A goal, sub-goal, or wave advanced to complete with no independent read of its landed range and no recorded waiver. The per-unit ban above is about not serializing every unit; it is not a licence to close unaudited.
- **Audit deferred because nothing was declared:** Treating "no cutoff was declared" as a reason no aggregate audit is due. The interval backstop fires without a declaration.
- **Block_ship inflation:** Using `block_ship` for a defect that violates no named release invariant and that a downstream consumer would never rely on. Two wrong lines in a leaf are a `block_range` or a `residual`, not a ship gate.
- **Repair-audit loop:** Re-auditing the whole range after each `block_ship` repair, so every fix uncovers the next finding and the pipeline never advances. Repair verification is scoped to the repair diff, and the third round is a Head ruling.
- **Whole-stream stop for a scoped finding:** Freezing unrelated lanes because the blocked boundary was never named. Rule 4 scoping applies to audit findings.
- **Cadence mail as a gate:** A cadence opinion must not freeze Hands or empty seats. It is a briefing, not a stop condition.
- **Cadence dispatches:** The cadence seat and the timer must not spawn Auditors, Heads, Hands, or polish seats. Cadence mails Mind; Mind routes.
- **Cadence files memos:** Cadence cites Mind memos to drop. It does not add another memo.
- **Cadence absorbs mail:** Cadence counts the Mind inbox and suggests absorb at ≥ 20. Mind absorbs. Cadence never runs `vivi mail absorb`.
- **Cadence clones needs:** Cite the existing need or want. Do not file a second copy.
- **Cold boot after ordinary Mind compaction:** Treating compact as “everyone is dead” and mass re-spawning or re-arming loops when children and schedulers are still live. Use [Warm boot](#warm-boot-mind-only-post-compaction); escalate to Cold only when infrastructure is not proven.
- **Warm boot without Auditor + CTO:** Reorienting board/loops after compaction but skipping the required reacquaintance pair. Compaction is the reliable trigger for tactical (Auditor) and strategic (CTO) re-grounding; cadence mail does not substitute. File + spawn both (or prove equivalent passes already in flight).
- **Warm-boot reacquaintance as a seat freeze:** Parking Hands/Planners until Auditor/CTO return, or re-running full suites as “reacquaintance.” Spawn async, keep seats full, evidence honesty / strategic lens only (Rule 6).
- **Cold boot without Mind paper hygiene:** Restarting seats while leaving unabsorbed Mind inbox, dead `host_task_id` memos, and stale Mind-owned tasks/needs as if they were current. Cold boot step 2 (absorb + cleanup) is required; only worker spawn-debt handles stay open for resume.
- **Cold boot without registry triage:** Restarting after dormancy without re-confirming the registered backlog against the tree that moved while nothing was running. Wants and needs written in a previous world then keep their old claim, get promoted on it, and reach a frontier gate carrying a premise that died weeks ago. Step 3 covers want/need/goal claims; paper hygiene (step 2) is Mind-owned state and never substitutes for it.
- **Frontier seat on triage work:** Sending backlog reconciliation to a Head, a CTO, or an Auditor — or hand-walking it in the Mind's own context — because the verdict sounds like a judgment. Deciding *whether an item is still true and startable* is shallow, wide, high-volume work on the cheap band; deciding *whether the approach is right* is the frontier question, and it happens later, on the items triage kept.
- **Cold-boot cleanup closes worker debt:** Deleting or `task done`-ing open Hand/Planner/Auditor/Head work because “the session died.” That work is spawn debt; re-spawn it. Cleanup targets Mind paper and proven-stale Mind-owned items only.
- **Worker runs Mind boot:** A Hand/Auditor/Planner/Head/Cadence that compacted starts Warm/Cold boot, board management, or loop re-arm instead of [Unit resume](#unit-resume-worker-seats-only). Workers stay on their handle (Cadence: finish the tick).
- **Compact Primary Request as current work:** Restarting early-session user text after compact while board and git show different mid-stream state. Compact annex is recent only; Vivi + git + Warm/Unit resume own truth.
- **Read-only spawn for shell work:** Spawning an Auditor, Planner, Hand, or Head without shell access when the task needs `vivi`, git, or test commands. The seat stalls instead of working. Spawn with full tools; reserve read-only for tool-free inspection.
- **N process loops beside cadence:** Do not arm `mind_wake` / `auditor_range` / `cto_range` / `polish_analyzer`. One `cadence_tick`. Never create multiple scheduler jobs in one parallel tool batch.
- **Re-arming cadence on boot:** Recreating a `cadence_tick` that is absent from the scheduler without operator enablement. The loop is **off by default**; a missing loop is the expected state, and the scheduler — never a memo — says whether it is armed.
- **Tracking work in a memo:** Recording handles, handle dependencies, which tasks are in flight, or loop ids in a Mind memo so a later turn can "remember" them. The graph holds dependencies, the board holds open work, `vivi boot` prints both at startup, and a memo that names a handle or a status is defective paper. Memos are long-term memory.
- **Polish analyzer implements:** Analysis suggests paths; only Mind-filed Hand polish tasks may edit.
- **Serializing on a stage number:** The Mind treats "we are in Stage 2" as a reason not to start later-stage or sibling-track work whose named dependencies are already met. Stage order is routing, not a gate. Cadence `pull_forward` exists to catch this.
- **Cadence ignores registered goals:** A tick that never reads `vivi goal list` cannot see later-stage work. The working set is that list, not the open-task board alone.
- **Serializing disjoint planning and implementation:** Start the next planning phase while implementation runs when the scopes are disjoint.
- **A seat sitting empty while another is busy:** Fill the empty seat (planning, implementation, or audit) instead of concentrating on the busy one.
- **Closing a phase without a strategic architecture review:** Explore and draft the next frontier concurrently, but run the phase-close strategic architecture review before admitting or committing to those candidates; skipping it drops future-compatibility and direction feedback that final selection depends on.
- **Sizing by promised elapsed time:** Size by one logical change and seat turnover, not by stuffing a clock estimate into a mega-bag.
- **Mega-Hand / shelf inventory:** Filing one Hand for a whole theme, several families, or product + docs + project-wide gates. That parks capital and blocks dependents. Split; integration owns atomic landing.
- **Micro-Hand / process novel:** Five to ten lines plus a 500-line task body. Merge it into the logical change it belongs to.
- **Project gate on a Hand:** The project's integration, suite, or release command in a Hand `validation`/`sanity` field.
- **Cosmetic seat saturation:** Do not send document fixes to Planners or invent audit preparation for unfinished implementation merely to occupy a role.
- **Verification thrash (Rule 6):** Hand runs a project-wide gate or a second "final" check, or runs tools after `task done`. Mind re-runs product tests. Auditor treats "I did not run the suite the receipt already records" as a defect. Heads re-run suites.
- **Unscoped build in task body:** A task validation field says a bare workspace build, causing the Hand to compile everything and all path deps. Every build command in a task body must name the exact unit.
- **Workspace-wide flags in Hand validation:** All-targets / whole-workspace flags in a task-body validation command. These hold the compiler lock for minutes and are forbidden outside phase-audit or release-gate contexts.
- **Two Hands on the same shared hot module:** The Mind dispatches two Hands to edit the same shared module simultaneously on a shared tree, guaranteeing foreign-WIP compile failures for both. Shared hot modules must be serialized or isolated.
- **Treating every receipt as zero trust:** Durable commit + validation claim + closeout receipt is the interface. Challenge honesty and completeness; do not regenerate the same evidence without a new product change or named risk.
- **Inventing the project's ladder from this skill:** Tugboat does not name scripts, stage numbers, or specialized lanes. Read the project's agents/charter docs.
- **Slug in the Vivi model field:** `model` is a band (`P2-S0`). Writing a provider slug there collapses the pool. Resolve the slug only at spawn.
- **Arming cadence to watch Wi-Fi:** Offline fallback is a per-turn network check. Cadence cannot fire if the host is offline.
