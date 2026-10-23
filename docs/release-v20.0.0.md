# Vivi 20.0.0

Major release on 19.x. The pump now runs **flows**: each need moves through a
written recipe of steps instead of the fixed unit-dispatch, gate, promote and
merge-batch pipeline. That old pipeline is deleted, not kept beside the new
one. This is a clean break.

## Upgrade note

Do not point a live 19.x pump at this release yet. The hub cut-over
(schema 9 migration, draining old pipeline state, `vivi pump adopt --flows`)
and the matching vivi-server release are not done. Old pipeline state in an
existing `pump.db` is left as is and no code owns it. vivi-server still builds
against v19.2.0 and does not compile against this release, because
`vivi::pump::gate_config` and `ProjectCreate.gate` are gone.

## Flows

- Four flows ship in `flows/`: `build`, `research`, `release` and `repair`.
- A build run plans the need, admits the plan, slices it, sends each slice to
  a hand, and merges each slice onto the need's branch (`vivi/need/<need>`).
  It then verifies the need: an auditor review and the need rung run side by
  side. Last, it merges the need into the target's main and runs the main
  rung.
- A red main files a repair need. The repair flow runs at CTO level and never
  escalates. A repair that cannot land locks with one alarm and waits for an
  operator.
- `vivi pump answer` answers a flow step by hand. `vivi pump unlock` restarts
  a locked run.
- New settings: `[pump.rungs]` (set with `vivi pump rungs`),
  `[pump.flow] max_live_runs` and `[pump.slice] minutes`.
- Every pump tick reads its state from `pump.db`, so a change made between
  ticks, or across a stop and start, is never undone by a stale copy.
  Loading state no longer rewrites it.
- Environment failures hold the run for the operator, with one alarm. They do
  not burn an attempt. Three step errors in a row hold the run too.

## Removed

- Commands: `vivi pump gate` and `vivi pump recover`, and `--gate` on
  `vivi project create`.
- The old pipeline: unit dispatch and readiness ranking, the gate, gate-fix
  and red-gate audits, promotion, merge batches, need landing and the old
  need-branch state, and the planner bookkeeping.
- Mind oversight now allows only `replan-need` and `note`. The `hold-merge`,
  `clear-hold-merge`, `skip-unit` and `unskip-unit` ops are gone.
- Config keys:
  - `[pump.gate]` and each target's `gate`;
  - `[pump] branch` and `promote`, and each target's `branch` and `promote`;
  - `[pump.merge]` `batch`, `need_parallel`, `shared_checks`, `checks_for`,
    `proof_timeout`, `remote_wait` and `lanes`;
  - `[pump.need]`.

  An old config still loads, and retired keys are ignored.
  `vivi config validate` names each retired key it finds, and
  `vivi config migrate` removes them. Migrate carries an old
  `[pump.merge] lanes` value into the new merge pool's slots, and names the
  change when an existing pool has fewer slots.

## Other changes

- Cadence jobs always run read-only. A role with a cadence that is set
  writable gets one readiness warning.
- A merge job may publish only to each target's configured main branch,
  `vivi/need/<x>`, `vivi/need-prior/<x>` and `pump/merge-main/<x>`.
- `vivi config show` prints a six-field flow line. The branch and promote
  fields are gone.
- `pump.db` schema upgrades now stop on a failed column migration instead of
  marking the database upgraded with columns missing.
- Several failures that were silently dropped now print a warning: freeing a
  local slot, recording seed build baselines, the cleanup before a dispatch,
  and removing a half-written sync key.
- The unknown-provider error in pool config now lists every provider.
