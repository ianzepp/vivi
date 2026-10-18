# Pump workspace repository targets

Status: implemented and validated on `feat/pump-service`.

## Interpreted Unit

Make one workspace mailspace coordinate work across several Git repositories.
Keep pool configuration at the mailspace level and let each executable unit
select exactly one repository target. Preserve one need and one workspace graph
when a need spans targets.

## Normalized Spec

- The workspace root owns one Vivi mailspace and one pump service registration.
- `[pump.targets.<id>]` names a repository root relative to the mailspace and
  its optional GitHub identity/source, integration branch, main/promotion
  policy, merge checks, and gate. The target's integration checkout lives
  under that target's pump-home directory.
- `[pools.<name>]` remains workspace-scoped. Provider, slot capacity, model
  capability, and machine setup are shared across all repository targets.
- Need authors can describe repository paths naturally. The planner receives
  the target catalog and assigns one target to each plan unit. A need may yield
  units for multiple targets. Each executable unit stores one immutable target
  ID and a write scope relative to that repository.
- An omitted or unknown planner target parks the need as `hold`; a target
  removed after planning parks its unit as `held` and resumes when restored.
  Runtime target paths must exist and be Git repositories. A target cannot
  change underneath an attempt or retry.
- Pump work state, need graph, and lifecycle remain workspace-scoped. Merge,
  gate, baseline, branch, checkout, and promotion state are target-scoped.
- Pool claims and capacity remain workspace/pool-scoped. Each job carries its
  target. A slot can serve different targets in succession using isolated,
  target-keyed checkouts under the same slot root. Repository identity,
  checkout path, and target-specific refs come from the job, not a
  repository-specific pool.
- A multi-target need completes only after all its units complete. Cross-target
  `after` dependencies are held because unpublished changes cannot be handed
  from another checkout.

## Initial Baseline (Before This Change)

- `PumpConfig` currently defines one integration checkout, remote, branch,
  main branch, merge config, and gate config in `src/pump/config.rs`.
- `Ctx` stores one repository path. Pump board planning iterates workspace
  needs, but planning and execution share that process-wide repository context.
- `PoolConfig.repo` is currently a GitHub `owner/name`. Pool `JobPlan.remote`
  derives from it. Pool seeding creates a fixed repository checkout, remote
  jobs use `$WORK/repo`, and local worktree slots attach to one `worktree_of`
  repository. Dispatch records do not store repository identity.
- The pump service already registers workspace mailspace roots and launches one
  pump process per registered root. That remains the outer supervision unit.
- The accepted control-state change is already committed as
  `c764e18 pump: store control modes in pump.db`; the service foundation is
  `16e28a1 pump: add user-level project supervisor`.

Implementation now carries the target through planner input, plan validation,
unit state, pump.db, shared-pool dispatch records, isolated slot checkouts,
local worktree cleanup, collection, merge queues, and per-target gate state.
Legacy configuration and existing unit state map to `default`. Named local
pool jobs use the pump-owned target integration clone as their worktree source,
so the merge thread can consume their local branch. Non-GitHub remote target
dispatch is explicitly rejected because remote-pool credentials are scoped to
GitHub repositories.

## Stage Graph

```text
shared target/job contract
        ↓
workspace target config + pump.db migration
        ↓
planner target assignment + pre-dispatch validation
        ↓
target-aware shared-pool checkout and receipts
        ↓
per-target merge/gate state + workspace need completion
```

## Implementation Work

1. **Target contract and migration** — add named pump targets; move repo
   identity out of pool capacity config; migrate a legacy single-repository
   workspace to one target when its source is unambiguous. Require an explicit
   target map for a multi-repository workspace whose old source does not
   identify a workspace-relative repo.
2. **Planner and pump state** — expose target IDs and workspace-relative paths
   to the planner; persist one target ID per unit; convert write scopes to
   target-relative paths; hold invalid or ambiguous plans before task send.
3. **Shared pool jobs** — pass the resolved target through dispatch, credentials,
   ref checks, checkout preparation, and result receipts. Keep one pool
   registry and slot limit. Isolate target checkouts under each slot and verify
   the same slot can execute target A, target B, then target A without state
   leakage. Move repository-specific checkout/build settings to targets only
   where live callers require them.
4. **Per-target integration** — partition merge queues, integration branches,
   gate state, baselines, and promotion by target. Aggregate unit and need
   completion in the workspace store and report partial target outcomes.

## Checkpoints And Gates

- Checkpoint 1: one legacy single-target workspace opens as a `default` target;
  one multi-target workspace loads two target definitions with one shared pool.
- Checkpoint 2: a need naming paths from two repositories plans target-specific
  units; missing/ambiguous mappings produce a visible hold and no dispatch.
- Checkpoint 3: one shared pool slot executes A → B → A; target-specific
  checkouts, credentials, refs, branches, and receipts remain isolated.
- Checkpoint 4: A and B merge and gate independently; a failed B gate leaves
  the parent need open while completed A work remains recorded.
- Final validation covers schema migration/restart, target mismatch, retry
  affinity, same-relative-path isolation, and service startup from one
  workspace registration.

Focused evidence covers one need assigned across targets, missing-target and
cross-target-dependency holds, same-path scopes across targets, target
assignment and retry immutability, unit target persistence, legacy state
loading, per-target gate-state isolation and branch selection, target clone
initialization, local branch collection and merge, shared-pool identity,
worktree cleanup, and remote/local A → B → A checkout isolation. The service
continues to register each workspace mailspace once; its pump initializes all
configured targets.

## Validation

Validation passed: `cargo fmt --check`, `cargo test --test hygiene`, and the
full `cargo test` suite. The full run reported 1,795 library tests passing; the
repository's two performance tests and external Docker/SSH tests were ignored
by their existing annotations. No version bump or release tag is included.

## Companion Skill Plan

Updated `skills/vivi/SKILL.md`, `pump-contract.md`, `pump-protocols.md`, and the
pump database migration note for workspace-relative targets, shared pools,
the unit target field, schema migration, and hold behavior.

## Known Boundaries

- Legacy single-repository configuration remains implicit `default`; a
  multi-repository workspace needs explicit target paths because a prior pool
  `repo` value cannot safely identify a workspace-relative directory.
- Remote providers currently support GitHub targets only. Local named targets
  work through target-specific Git worktrees.
- Target build caches persist under each slot's target-keyed directory instead
  of using the legacy single-repository tmpfs/build-pack path.
