# Orchestration reset: what keeps failing, and a plan

**Status**: operator ruling 2026-11-18: **direction B (Temporal) is chosen, and Phase 0 has started.** The other open decisions at the end are still open.

## Sources

- Five research seats, 2026-11-18:
  - hub Claude transcripts (about 1,200 operator messages, Oct 29–Nov 16)
  - laptop vivi, pump and faberlang transcripts
  - the live hub pump through read-only `vivi` subcommands (10,704 jobs)
  - disk wear on every host
  - established orchestration engines, from current documentation
- [`../pump-failure-model-review.md`](../pump-failure-model-review.md), the Oct 26 review.
- [`merge_queue_model.py`](merge_queue_model.py), run for this document.

Numbers are observed unless marked *(inferred)*. Code and data are the ground truth here, not earlier docs.

## The underlying problem, in one paragraph

You want a short process: take a need, plan it, cut it up, build the pieces on the cluster, check them, review them, and merge them. To get it, the project has built three hard infrastructure systems at once, inside a fast-moving project, and tested them on the real workload:

1. a **distributed job runtime**: the pump, pool and slots;
2. a **CI and merge system**: rungs, need branches, gates and landers;
3. an **LLM management hierarchy**: Mind, CTO, `vivi mind`, and Claude on hub.

Each of the six or seven generations (fleet → tugboat → graph/step → speedboat/pool → pump → flow → hub scripts → lab-hardened flow) rebuilt part of the same primitives:

- leases
- retries
- failure classes
- checkpoints
- job history
- merge gating

Each one dropped some of what the last one had learned. The workload also exposes something no runtime fixes: **the landing math does not close**. Every merge strategy tried so far either can't keep up or lets regressions onto main. Units also reach the merge unchecked against the main they will join.

## Problem inventory

### Runtime: dispatch, retry, slots

| Problem | Evidence |
|---|---|
| Failure has no class. A 429, a lost job, a format error and bad work all become `failed` | Oct 26 review, G1. 128 + 31 HTTP 429s in 14 days. A 429 became a format error, then a CTO step, then a lock |
| A timeout loses the work | 42% of repeat rounds (Nov 6 sample) hit the 55-minute cap, and the hand-in was lost. 110 agent-hours went to rc=124 kills |
| Retries re-run the same thing blindly | Need 39dab91b was "dispatched" 23 times and never ran. One subject was dispatched 62 times. An audit task ran 85 times with no change. A red main rung was re-dispatched 14 times in 10 h. A corpus need reached attempt 408 |
| Recovery means a new run that throws work away | In the vivi project, 105 restarts across 48 needs. 77% of runs entered at plan. Only 24% of slice steps merged; 810 were superseded |
| Slots leak or sit uncollected | Leaks in 19.0.2 and 19.0.3. 11 mini slots sat finished but uncollected. 435 stale cadence jobs were launched on resume. 25 dispatches were never collected |
| The job record can't be queried | It is an untyped `meta['jobs']` blob, and log lines carry no date. A console was built on Nov 14 just "to see what is failing" |
| Landed code is not deployed code | Pump fixes go live only at the next release and install. The pump also fixes itself, using the machinery being fixed |
| Learned handling is lost in rewrites | `transient.rs` and `envfail.rs` (Oct 18) are not used by the flow engine |
| Plumbing noise | 1,205 "already names different work" lines, 1,176 deferred dispatches of closed tasks, and 3,455 "waits on unplanned prerequisite" lines |

### LLM pipeline: roles, envelopes, judgment

| Problem | Evidence |
|---|---|
| Too many lossy handoffs per need | About 20 flow nodes and 9 roles. Vivi-project needs took a median of 25 jobs and a p90 of 132 |
| Strict text envelopes from weak models | 37% of vivi hand jobs needed a correction round, almost all on glm-5.3-flash. Hand replies came back as Markdown, bad TOML, or tables below the fence |
| Judgment is used as error handling, and the judge can't see the cause | 103 CTO steps produced 54 lock verdicts. Every lock reason was some form of "the brief omits…". 37 operator unlocks |
| "Done" self-reports are unreliable | Of 24 needs closed on Nov 5, 11 met their requirements. Mini "done" verdicts held for 13 of 35 |
| Waste concentrates in needs that never land | 60% of vivi-project agent time went to 22 withdrawn needs. About 20% of all need-work agent-hours (276 of 1,342) produced nothing |
| Units are too big for the cap | 30% of repeat rounds were "scope unmet". Needs were sliced into 100+ slices, and one into 39 |
| Cheap models fail more often | Failure rates by model: glm-5.3-flash 14.3%, glm-5.3 12.8%, luna 5.9%, sol 1.7%. GLM hands timed out on 22–31% of jobs (Nov 1) |

### Landing: merge, verify, backlog

| Problem | Evidence |
|---|---|
| Every merge strategy failed one way or the other | **Batch proofs (Nov 3–12):** 5–11 units, then a serial 30–60 min proof. Main moved mid-proof, which forced a second proof. Combined batches broke 70–112 tests at a time, though each unit passed alone. **One at a time (Nov 13–14):** about 3 min per unit and about 25–30/h, but it tested changed crates only. 3 regressions reached main, and each needed a bisect of about 9.4 min per step. 289 attempts: 120 landed, 94 conflicts, 71 red |
| Batches merged without bisection, or without the gate | Bisection was done by hand, hours later. Nov 14: "merge every landing regardless of green" merged 47 branches with no tests run. Oct 18: a queue 81 deep. Nov 3: about 15/h landed against about 60/h returned |
| The gate shares the machine with agents | 29 `faber` failures under load against 0 when run alone |
| Units reach the queue unchecked | Only 48% of proofs were green on the first try (Nov 6). 31% were red overall (Nov 5–10) |
| Parallel units edit the same hot crates | Conflicts were the largest single cause of failed landings: 86 of 153, mostly `radix-mir-clif` (26) and `corpus/reference` (18). 12% of repeat rounds were conflicts |
| The backlog grows | 219 unmerged `vivi/unit/*` branches on radix (+88k lines). About 6,170 branches on GitHub radix. 765 open faberlang needs |
| The oracle is noisy | Nov 14: 0 of 24 new corpus "reds" were real (pin file, wording, and unexpected passes). Landing checks skipped dependent crates, so three emitter commits turned every later unit red. Public CI had been red since 1.16 without anyone seeing it. The release gate takes 2.5 h |

### Disk and builds

See [Disk wear](#disk-wear).

| Problem | Evidence |
|---|---|
| SSD wear | The studio Lanes SSD took 151 TB in about 21 days: about 7.2 TB/day and 7% of rated life, which means about 5 months at that rate. 5.6 TB was written in one day |
| Cold builds | 26 of 37 proofs were cold fresh-clone builds. A cold build writes 30–40 GB. Proof work used more CPU (31,111 CPU-s) than agent work (20,998 CPU-s) |
| Trees pile up | 560 trashed build trees sat in the lanes. Proof `mktemp` leftovers filled the studio system disk to 96% |

### Control: supervisors and fabric

| Problem | Evidence |
|---|---|
| Control lives in an LLM session | 7c4c95d4 went through 13 compactions in about 460 ticks. It stalled for hours asking "shall I start?" because two instruction files conflicted. A proven-green fix sat 7 h. Operator: "things kept falling through the cracks" |
| Rulings live in prose and get lost | "Every day I have to have this discussion again." The pin file grew 987 → 1,305 entries after the Nov 6 ruling to retire it |
| Cost has no bound | About 21,400 Codex requests (about 1.5B tokens) used a week's quota in about 8 hours, followed by 143 failures in a row |
| The network fabric is fragile | 8,037 DNS failures (vivi pump) and 4,177 "SSH unreachable" lines. Tailscale outages lost 8 h on Nov 6–7. `hub.tailc3379f.ts.net` does not resolve *on hub* today. sshd `MaxStartups` dropped connections |
| Load is lopsided | The studio ran 7,504 of the pump's 10,704 jobs. studio-02 ran 110. The mini sat idle because no work was sized for it |

## Root causes

Every problem in the inventory traces to one of these five.

1. **The runtime is home-built, and it lacks durable-execution primitives.** These are:
   - typed failures with a retry policy for each class
   - job ids that can't run twice
   - heartbeats
   - work checkpointed while it runs
   - a history that can be queried

   Each generation rebuilt some of these, and the next rewrite dropped them. This is not a lack of effort: 40k lines of pump and about 75 pump commits since Oct 26. These are hard, solved problems, and they are not this project's product.

2. **Too many LLM handoffs, with judgment used as error handling.**
   - Every need crosses about 9 roles, and each handoff is a strict text envelope.
   - Each step can fail in about five ways, so a clean run is unlikely. When a run fails, recovery goes to a blind LLM judge, which locks the need or restarts it from the plan. The restart discards the work.
   - A plain machine fact (429, timeout, conflict) should never reach a judge.

3. **The landing math does not close.**
   - A check that keeps up was too narrow (changed crates only), so regressions reached main.
   - A check that was wide enough was run serially, on unbisected batches, on loaded machines, so it could not keep up.
   - About 30% of units arrived bad, and the pool was allowed to produce faster than the lander could merge. That is how a backlog of 219 branches builds up.
   - Parallel units were scheduled onto the same hot files, so conflicts were the largest single cause of failed landings.

4. **The verification oracle is slow, noisy and write-heavy.**
   - Slow: cold builds in fresh clones, a 15–60 min candidate proof, and a 2.5-hour release gate.
   - Noisy: the pin file and wording diffs, plus checks that skip dependent crates. A red result can't be trusted, so automation can't act on it. People then merge on hope instead.
   - Write-heavy: the same cold builds are the SSD writes.

5. **Control and memory live in LLM sessions and prose.**
   - The supervisor compacts, stalls, misses stuck states and costs a lot.
   - Rulings are text that gets lost or contradicted.
   - Each supervisor layer was added to watch the one below. No layer made that one reliable.

### Contributing factors (not causes on their own)

- Cheap models as hands: high format-failure and timeout rates.
- The fabric: DNS, Tailscale, ssh limits, disk-full events.
- A backlog that grows itself: auto-filed follow-ups, and audit findings filed as needs.
- The pump building and fixing itself.
- One heavy worker carrying most of the load.

## What the merge model shows

Source: [`merge_queue_model.py`](merge_queue_model.py). It is a seven-day discrete-event simulation, averaged over 5 seeds. Its inputs are measured:

- **Arrival:** the pool delivered about 7 units/h on average and 40/h at peak (950 units on Nov 7). Radix `main` took about 115–183 merges per day.
- **Gate times** (measured on studio, warm tree):
  - one-unit changed-crate check: about 3 min
  - full workspace build and test: about 9.4 min (24,177 tests)
  - old candidate proof: 15–60 min
- **Share of bad units:** about 30% unchecked. 3–5% assumed for units already checked against current main on their worker.

The model is conservative: it runs one top-level batch at a time and does no speculative pipelining.

| Scenario (2 gate runners) | In/h | Landed/h | Median wait | Queue after 7 days |
|---|---|---|---|---|
| One at a time, 30-min proof, 10% bad, noisy oracle | 15 | 3.3 | 60 h | 1,872 |
| Batches of 20 merged without the gate (Nov 14) | 15 | 15.2 | 0 | 0, but **259 bad units on main** |
| Batch + bisect, 30-min proof | 15 | 4.8 | 47 h | 1,637 |
| Batch + bisect, **10-min full workspace gate**, 5% bad | 15 | 14.3 | 0.3 h | 6 |
| Same, 25/h | 25 | 22.7 | 5.6 h | 203 (slowly growing) |
| Same, 40/h, 3% bad, batch 24 | 40 | 37.1 | 7 h | 304 |
| Unchecked units (30% bad), 8-min gate | 15 | 8.1 | 18 h | 584 |

What it means:

- **A full workspace gate of about 10 minutes, with batch-and-bisect on the two Studios, lands about 20–25 checked units an hour.** That is roughly 500 a day, 3–4 times what radix `main` has actually taken. Nothing red reaches main.
- **Capacity was never the limit. The strategy was.** Each past strategy had one of three faults: a gate too narrow to protect main, an unbisected batch, or a serial proof run on a loaded machine.
- **Units must be checked before they queue.** At 30% bad, even a fast gate backs up, because bisection eats the runners.
- **Dispatch must be throttled to landing capacity (backpressure).** If the pool produces faster than the lander can merge, the surplus becomes the branch backlog. In the model, 40/h overruns the queue unless units are 3% bad and batches are large.
- **Gate runs need their own capacity.** They must not share a machine with eight agent slots, or the load produces false reds.
- **The corpus and the 2.5-hour release gate move after the merge.** They run every N merges or nightly, bisect automatically, and file a repair need.

## Disk wear

Live SMART readings, 2026-11-18. The cluster has been idle since Nov 14.

| Disk | Written | Wear | Rate |
|---|---|---|---|
| studio Lanes, Samsung 9100 PRO 2 TB | 165.6 TB in 667 h | 8% | 5.9 TB/day lifetime. 10–14 TB/day at the Nov 11–12 peak. 4.4 TB/day after the warm-tree restructure, with 8 slots |
| studio-02 Lanes, same model, new | 2.2 TB | 0% | 171 GB/h under the same load |
| **hub `/mnt/vivi`, 850 EVO 500 GB** | 8.27 TB since boot (4.8 days) | unknown (needs sudo) | **76 GB/h right now, while idle.** I measured it myself over 60 s. That is about 1.8 TB/day. The drive is rated about 150 TBW *(from memory, unverified)*, and **nothing monitors it** |

**Where the studio writes come from: about 90% is Rust debug artifacts in 21 persistent per-slot `target/` directories.**
- About 60% of those writes is `incremental/` and about 30% is `deps/`.
- Each busy slot writes about 30 GB/h, and the drive's rate scales almost linearly with busy slots.
- A landing check writes about 12 GB, and an agent job about 11–12 GB all-in.
- `sccache` is installed but not wired in (no `RUSTC_WRAPPER`).
- The Nov 11–13 changes cut cluster writes about 40%, from about 14.6 to about 8.5 TB/day. That is not an order of magnitude.

**The Cranelift pivot helps less than hoped *(inferred)*.** It removes the emitted-Rust corpus leg, which writes about 30 GiB per pass. It does not remove rebuilds of the 67-crate compiler, which is the 90% share.

Levers, largest first *(each is inferred until measured on one slot for a day)*:

1. **Agents iterate with `cargo check` plus targeted tests.** Full test builds happen only in the gate tree and the worker's pre-queue check.
2. **Turn incremental off in agent slots and wire `sccache` for dependencies.** This needs a new ruling, because ruling S9 says incremental is never off. Incremental is about 60% of the writes, and `sccache` cannot cache incremental builds.
3. **Use fewer busy slots per drive.** With backpressure from the lander, 8 slots per Studio is more than the lander can land.
4. **Treat drives as consumables, with a budget.** At 4.4 TB/day, a 2 TB drive rated about 1,200 TBW lasts about 8 months. It is worth deciding what write rate is acceptable, not only cutting it.

**Hub `/mnt/vivi` is the most urgent item in this document.** It holds the vivi server, pump data and lanes, and it writes about 1.8 TB/day with the cluster idle.

- **Who writes:** the cgroup `io.stat` for `vivi-server.service` shows 1,220 GB written to this drive in the 26 h since the service started, about 47 GB/h. That cgroup holds the server and both pump children.
- **What they are doing:** the faberlang pump ticks every ~7.5 s with `flow 0`. Each idle tick takes ~1.7 s, and one tick a minute spends ~18 s in collect.
- **Likely mechanism** *(code read; not yet confirmed on the live database)*:
  - `PumpStore::save` (`crates/vivi-core/src/pump/store.rs`) skips a write only when the serialized `State` is byte-for-byte unchanged.
  - Otherwise it runs `DELETE` on every table and reinserts the whole state. That is every unit since Oct 21 with its brief, every need and event, and the `meta['jobs']` blob, which nothing ever trims.
  - So any field that changes on an idle tick, a timestamp for example, rewrites the whole database. WAL mode then writes it a second time.
- **Confirmed 2026-11-18 (who, not why).** Pausing the faberlang pump changed nothing: about 77 GB/h. A paused pump keeps ticking; pause only stops dispatch.
  - Stopping it (`vivi pump stop`) dropped the drive to **57 MB/h** within a minute.
  - The vivi pump, paused but still ticking, writes almost nothing.
  - The faberlang pump is now **stopped**, so its cadence checks do not run either. `vivi pump start --project /mnt/vivi/projects/faberlang` brings it back, and it will write again.
- **Still to confirm (why):** which field changes on an idle tick, and how big faberlang's `pump.db` is. Both need the `vivi` user's file access.
- **Fix, if the pump stays:** write rows that changed, not the whole state, and trim the jobs record.
- Nothing monitors this drive: `orch/disk-writes` watches only `/vivi`.
- On Nov 12, Claude on hub also noted the hosted-project indexer rescanning 18k files every 5 s. That is mostly reads, and it is a secondary suspect.

## Two directions

### A. Fix the pump in place

This means the Oct 26 order: G1 classify and back off, G4 typed job record, G3 CTO brief from the record, G2 keep the branch and resume in place, G5 contract fixes.

- **For:** no new infrastructure, and the code is known.
- **Against:**
  - It is attempt seven on the same primitives.
  - Three weeks after that review, all five gaps were still open.
  - It does nothing for root causes 3 and 4. Those are where the throughput and the SSD writes actually go.

### B. Replace the runtime; keep Vivi as the record (recommended)

- **Temporal**, self-hosted, does dispatch, retries, timeouts, heartbeats, per-host queues and history.
  - The Rust SDK is GA (temporalio-sdk 1.2.0, 2026-11-09). The licence is MIT. It runs on Postgres with SQL visibility and no Elasticsearch.
  - Fallback: Hatchet (MIT, Postgres only, no official Rust SDK).
- **Vivi** stays the system of record for needs, wants, roles, charters and status, over its HTTP/MCP server.
- **What retires:** the pump's flow, collect, dispatch, requeue and lock machinery, as a clean break.
- **What is kept:** the pool's slot and worktree management, `vivi-submit` reply validation, and the console. Those parts work, and the new system's activities call them as a library.

| Root cause | How B addresses it |
|---|---|
| 1. Home-built runtime | Temporal supplies the primitives, so none of them is written here |
| 2. Handoffs and judgment | A shorter pipeline (below). Machine failures never reach a model |
| 3. Landing math | A lander with batch-and-bisect, a fast gate, and units checked before they queue |
| 4. Oracle | A ≤10-minute gate that covers dependent crates. The corpus and full suite run after the merge. The pin file is retired, per the Nov 6 ruling |
| 5. Control in prose | State and policy live in workflow code and history. An LLM sees only parked needs, with typed causes |

Switching the runtime alone would fix root cause 1 and little else. **Root causes 3 and 4 are the biggest levers, and they don't depend on which runtime is chosen.** So they go first.

### The pipeline under B

```
NeedWorkflow (id = need-<handle>; a second start is refused)
  plan      LLM, queue llm-light (mini): writes slices with acceptance
            checks into vivi; each slice ≤ ~30 min of agent work
  per slice (several in parallel, each its own child workflow):
    implement  agent on queue agent-<host>, warm persistent worktree;
               heartbeats, and pushes work in progress every few minutes,
               so a timeout or a lost host resumes from the pushed commit
    check      machine: build + changed and dependent crate tests against
               current main, in the same warm tree. Red goes back to
               implement with the failure text, on the same branch,
               at most N times
    review     LLM on green only; may ask for one change round
    land       signal to the Lander workflow; wait for merged or ejected
  close     machine summary into vivi; need done
```

The **Lander** is one long-lived workflow. It batches queued units, runs the fast gate on the gate runners, bisects a failure, merges the passing units onto `main`, and ejects failures back to their slice with the cause.

Two scheduling rules live in workflow code, not in prose:

- **Backpressure.** No slice starts while the Lander's queue is longer than about an hour of landing capacity. A slice that has not started has no branch, so it cannot become backlog.
- **Hot-file mutex.** The plan declares each slice's write scope. Two slices whose scopes overlap on the same hot paths, such as `radix-mir-clif` or `corpus/reference`, do not build at the same time. Conflicts were the largest single cause of failed landings.

A second **MainWatch** workflow runs the full suite and corpus every N merges or nightly. On a red result it bisects the range and files a repair need.

| Failure class | Policy |
|---|---|
| Provider (429, 5xx, quota) | Retry after the Retry-After delay. It does not use an attempt. The provider cools down for the whole pool |
| Infra (host lost, heartbeat missed, ssh or DNS) | Retry on another host's queue, at most 3 times, resuming from the pushed commit |
| Format | Is checked at hand-in by `vivi-submit` (already shipped in v24). When the agent can't produce a valid reply, the activity fails as infra |
| Work (red check, review change request) | Loops back to implement with the failure text, at most N times |
| Out of attempts | The need is **parked** in vivi with a typed cause and its history. Nothing restarts at plan and nothing is discarded |

These roles are dropped as automatic steps:

- admitter
- separate slicer
- judge
- rung raises
- research by default
- the CTO as an automatic decider

Parked needs go to the operator or the mind, which decide with full information.

### Roles: worth reconsidering (operator, 2026-11-18; not in scope here)

Vivi's roles (planner, hand, mind, head-cso, …) date from when a role was a mailbox that an agent polled for work assigned to it. The recent pattern assigns work straight to an LLM job, and under Temporal the workflow step decides what runs: a charter or prompt, a model, and a queue. A role adds little to that. It also creates friction: Phase 0 hold marks failed on 30 of 79 needs until the right "holder" role was named.

A direction to consider later: needs, wants and charters stay in Vivi, and step types plus charters replace roles as the unit of assignment. It is out of scope for the runtime work.

### Where things run

- **Temporal server + Postgres:** on hub. It is Linux, systemd, and the always-on box.
- **The mini:** the control role it has been missing. It runs the control worker (intake from vivi, Lander, MainWatch) and the light LLM roles (plan, review), which already ran there with 0.6% failures.
- **Studios:** agent and gate queues, one queue per host, so warm trees get affinity.
- The Temporal server could also go on the mini. Hub is the safer default.

## Plan, with a proof gate for each phase

**Phase 0: stop the bleeding (about a day).**
- Find and stop the hub `/mnt/vivi` writer. Pause the idle faberlang pump. Add the drive to write monitoring.
- Keep the pump flow off. It has been off since Nov 5.
- Triage the backlog:
  - the 219 unmerged radix unit branches: land, salvage or drop each
  - turn auto-filed follow-ups into wants
  - fix `hub.tailc3379f.ts.net` not resolving on hub
- Gate: the hub drive is under 50 GB/day while idle, the triage list is empty, and anyone can read the backlog count.

**Phase 1: make the oracle fast and honest, and cut writes. This is independent of the runtime.**
- **Merge gate:** the full workspace build and test on a dedicated, warm gate tree, about 9.4 min today. Changed crates alone are not enough: that check let 3 regressions onto main.
- **Gate capacity:** gate runs never share a machine with a full set of agent slots, because load produced false reds.
- **Batch and bisect** in the existing lander scripts (`land-unit`) as a stopgap. The model says this alone covers about 500 units/day.
- **Pre-queue check:** each unit rebases on current main, then builds and runs changed and dependent crate tests on its worker before it queues.
- **After the merge:** the corpus, the full suite and the release gate run every N merges or nightly, with automatic bisection and a repair need. Retire the pin file, per the Nov 6 ruling.
- **Disk:** on one slot for one day, try `cargo check` iteration, incremental off with `sccache` for dependencies (needs a new ruling on S9), and fewer busy slots. Keep what cuts GB per landed unit.
- **Gate (proof):** replay the last 50 landings. Targets:
  - gate time ≤10 min
  - false-red rate ≤2%
  - no regression reaching main
  - GB written per landed unit, measured before and after

**Phase 2: runtime spike (about 2–3 days).**
- Temporal on hub, running `NeedWorkflow` and the Lander with **stand-in activities** that replay the measured failure mix.
- The lab's d1–d9 and fail1 scenarios become workflow tests (Temporal's test server skips time).
- Pass criteria:
  - a 429 never spends an attempt
  - a timeout resumes from the pushed commit
  - a duplicate start is refused
  - a red check goes back to implement on the same branch
  - no slice runs twice at once
  - a Lander bisection ejects exactly the bad units
- Then one real need on one Studio.

**Phase 3: cut over Faber.**
- Run the new runtime for one week on real needs, with the pump stopped.
- Measure:
  - landed/h
  - median wait from arrival to landing
  - attempts per slice
  - agent-hours that produced nothing (baseline about 20%)
  - SSD bytes written per landed unit
- Gate: each metric beats the baseline in this document.

**Phase 4: retire the pump.**
- Delete the flow, collect, dispatch, requeue and lock code in one clean break.
- Vivi keeps the record, MCP, pool slots, `vivi-submit` and the console.

## Decisions for the operator

1. Direction B (Temporal + Lander + Vivi as the record) or A (fix the pump)? Phases 0 and 1 are the same either way.
2. Is green required to merge? The Nov 14 ruling said no. The model says the ~10-minute full workspace gate must be green to merge, and only the corpus and release suites move after the merge.
3. May incremental be turned off in agent slots (ruling S9) for a one-day write test?
4. What is the model floor for hands? glm-5.3-flash fails 3–8× more often than sol.
5. Should the Temporal server run on hub or on the mini?
