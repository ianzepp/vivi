# Campaign: Pump as a Cluster Scheduler

**Status**: planned — design admitted 2026-10-22; no phase dispatched.

## Summary

Turn the pump from "one supervisor process per registered project, each placing
its own work on the workers" into one multi-project pump that runs every
registered project in a single loop and owns placement for the whole host, the
way a batch scheduler's controller does. The design borrows Slurm's model and
names on purpose, so each decision can be checked against a long-proven system
instead of being invented: partitions (lane sizes), trackable resources (disk,
memory, build tokens), licenses (provider concurrency), QOS and priority (job
classes), fair-share across accounts (projects), and backfill.

Vivi is already project-agnostic: every command takes `--project` or walks up
to a mailspace root, and the pump's tick is a function of one project root.
Only the hosting is per-project today. This campaign changes the hosting and
adds the scheduling phase; the per-project pump logic (planning, receipts, need
branches, merges, gates, closeout) stays as it is.

## Why now (observed 2026-10-22 on hub)

- `vivi pump service run` supervises one long-lived `pump run --service-child`
  process per registry entry (`src/pump/service.rs`, `children:
  BTreeMap<PathBuf, Child>`). Measured: the faberlang child holds ~109 MB RSS
  and ~3% of a core while busy; every child wakes each 20 s tick even with
  nothing to do. One hundred registered, mostly idle projects means ~100
  processes and ~5–10 GB for no work.
- Each child places its own work: it claims pool slots directly and counts
  per-model caps only for itself. Across projects there is no fairness (a
  large backlog can take every lane), no class priority (hands can fill the
  pool while delivered work waits for a merge lane), and no shared provider
  limits (ten busy projects each allow themselves the full Codex cap).
- Pools inherited from a base mailspace are copied per project: slot names
  and claims are keyed by the using project's own id (`space_key(name, id)`,
  `src/pool/slot.rs`), so "32 lanes" in an ancestor means 32 per project, not
  32 for the host. Only the worker's own `worker.conf` budgets bound the
  total.
- Placement by push over SSH spreads capacity bookkeeping over the
  dispatchers; the 2026-10-22 slot race and leaked slot (fixed in 19.0.2 and
  19.0.3) are that class of bug.
- Model choice for hands ignores bands (a caps rotation picks the model) and
  reasoning is not applied by the studio harness, so the P-band intent does
  not reach most work (model audit, 2026-10-22).

## Concept map (Slurm → vivi)

| Slurm | Vivi after this campaign |
|---|---|
| `slurmctld` (one controller, owns queue and placement) | the multi-project pump loop (`vivi pump run` over the registry), run by the pump service |
| `sbatch` / job submission | a project's tick files ready work (units, merges, gates, planner/audit jobs) as scheduling requests |
| Account / association | project (mailspace) |
| Partition | lane class: a pool shape (worker host, boundary kind, size) defined in the base mailspace, e.g. `small` 8 GiB, `medium` 32 GiB, `large` 128 GiB |
| Partition `MaxNodes` / node count | host-wide lane count of the partition (shared by every project that inherits it) |
| TRES (cpu, mem, gres) | per-job resource request: lane disk, memory reservation, build tokens |
| Node `RealMemory`, `Gres` | worker capacity reported by the worker (budgets in `worker.conf`, live free memory/disk) |
| **Licenses** (`Licenses=foo:4`) | provider concurrency: `codex:2`, `zai:6`, `opencode-go:4`, … counted host-wide, replacing per-pool caps |
| QOS + priority weights | job class weights: merge and gate > need audit > hand > planner > cadence |
| Fair-share (decayed usage per account) | per-project decayed usage lowers that project's priority |
| Backfill | a small job runs in a gap when it cannot delay a higher-priority reservation |
| `squeue`, `sinfo`, `sprio` | `vivi pump queue`, `vivi pump partitions`, `vivi pump queue --why` |
| Pending reason (`Resources`, `Priority`, `Licenses`, `PartitionNodeLimit`) | a stated pending reason on every waiting request (closes want 7237eaad: a job with no pool waits forever, silently) |

## Settled decisions (operator, 2026-10-22)

1. **One pump, many projects.** `vivi pump run --project X` keeps running one
   project. `vivi pump run` without `--project` (or with `--manifest <file>`)
   iterates the registry and, per round, checks cheaply whether each project
   needs a tick; idle projects cost a stat and a small query, not a process.
   The pump service hosts this loop instead of supervising per-project
   children.
2. **Placement belongs to the loop, not to projects.** Projects file requests;
   only the loop's scheduling phase grants lanes and licenses.
3. **Pools live in the ancestor (base) mailspace as host defaults.** A
   partition defined in the base is one shared capacity for the host; projects
   use it, they do not get private copies of its lanes.
4. **Lane sizes are a catalog.** Most projects use the small lanes (8 GiB); a
   large project (radix) defaults to medium (32 GiB) and sends gate/release
   work to large (128 GiB). A project chooses defaults per role; a unit may
   ask for a larger class.
5. **Model choice is by band.** P0–P5 resolve to a model + reasoning pair for
   every job kind, hands included; reasoning is actually applied.
6. **Borrow, don't invent.** Where this document and Slurm's semantics agree,
   follow Slurm's; a deviation needs a stated reason in the phase spec.

## Scope (admitted items)

| Id | Item | Status |
|---|---|---|
| S1 | Multi-project loop: `pump run` over the registry/manifest; per-project need-a-tick check; per-project tick isolation (panic catch, time budget, backoff); project lock respected (a standalone pump owns its project, the loop skips it with a stated reason); bounded parallel ticks with a single-threaded grant phase | pending |
| S2 | Pump service hosts the loop (no per-project children); hot reload of the registry and per-project config between rounds; upgrade path from the child model (`pump adopt`-style one-time step if state needs it) | pending |
| S3 | Scheduling queue: durable request table (project, job id, class, partition, TRES, licenses, priority inputs, submit time); grant/lease rows with heartbeat and expiry; a standalone `--project` pump files into the same queue (one grant path) | pending |
| S4 | Scheduling pass: priority = class weight + need priority + age + fair-share (decayed per-project usage); partition capacity; license counters; backfill for small jobs; pending reason per request | pending |
| S5 | Partitions: defined in the base mailspace; shared lane namespace keyed by the defining base, not the using project; per-role default partition per project; per-unit partition request from the planner (size class in the plan) | pending |
| S6 | Licenses: host-wide provider concurrency counters replace per-pool `caps`; a model maps to the licenses it consumes; licenses configured in the base | pending |
| S7 | Worker resources (TRES): worker reports capacity (disk budget, free memory, build tokens); `worker.conf` gains a build-token count that the driver enforces across all jobs on the host; a job's request is checked against both the partition and the worker | pending |
| S8 | Band-based model selection for every job kind (hands included, replacing the caps rotation); reasoning applied end to end (`{reasoning}` in harness runs; recorded in dispatch/collect notes) | pending |
| S9 | Operator views: `vivi pump queue [--why]`, `vivi pump partitions`, `vivi pump licenses`; MCP read tools for the same; exceptions for requests that can never schedule (no partition, unknown license) | pending |
| S10 | Hub cutover: base mailspace on hub with the partition catalog and licenses; faberlang moved onto it; per-project children retired; measured before/after (memory, wakeups, queue wait) | pending |

## Phases

| Phase | Items | Done when | Status |
|---|---|---|---|
| 1. Loop | S1, S2 | The service runs every registered project from one process; an idle project costs no process; behaviour of a single busy project is unchanged (existing pump tests green; a two-project test shows independent progress and a panicking project not stalling the other) | pending |
| 2. Queue and pass | S3, S4, S9 (queue views) | Every dispatch goes through a grant; class priority and fair-share are observable in `vivi pump queue --why`; a starving-project test and a merge-before-hand test pass | pending |
| 3. Partitions and licenses | S5, S6, S9 (partitions, licenses) | A base-defined partition is one host-wide capacity across two projects; provider licenses cap concurrent jobs host-wide; the per-pool caps path is removed | pending |
| 4. Worker resources | S7 | A build-token limit on studio holds across projects; a job too large for any worker pends with reason `Resources` | pending |
| 5. Bands everywhere | S8 | Hands, merges, gates, planners and audits all resolve model + reasoning from bands; dispatch notes record both | pending |
| 6. Cutover | S10 | Hub runs the loop with the base catalog; faberlang (and any second project) run on it; measurements recorded | pending |

Phases 1 and 5 have no dependency on each other and can run in parallel.
Phase 3 depends on 2; 4 depends on 3; 6 depends on all.

## Open questions (operator)

1. **Push or pull to workers.** Keep the current push model (hub dispatches
   over SSH into a granted lane) for this campaign, or add a pull agent on
   each worker (it reports resources and takes leased jobs, CI-runner style)?
   Pull removes capacity bookkeeping from hub entirely but is a larger change;
   recommendation: keep push in this campaign, treat the agent as a follow-on.
2. **Preemption.** Slurm can preempt lower-QOS jobs. Recommendation: no
   preemption (jobs are short and receipts make cancellation costly); priority
   and backfill only.
3. **Fair-share half-life** and class weights: start values (e.g. 24 h
   half-life; merge 1000, gate 900, audit 800, hand 500, planner 400,
   cadence 100) or something else.
4. **Adopt instead of build.** Nomad (macOS `raw_exec`, priorities, node
   pools; no fair-share or licenses without its paid tier) was considered and
   not chosen; Slurm and HTCondor are heavy to operate on macOS workers.
   Confirm building inside vivi.

## Related

- Wants 064cb378 (fixed: slot race), 49c8d294 (fixed: destroy without ledger),
  7237eaad (silent pending role jobs; closed by S9), 67c2cb1a (reclaim noise).
- Model audit 2026-10-22 (hand routing ignores bands; reasoning not applied;
  OpenCode Go DeepSeek V4.1 Flash not wired).
- `docs/worker-driver-protocol.md` (worker provider), skills/vivi "Inherited
  config (base mailspaces)".
