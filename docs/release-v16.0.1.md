# Vivi 16.0.1

Patch on 16.0.0.

A rehearsal of upgrading a real 15 pump to 16.0.0 found four defects. One
stopped planning altogether, two left roles stalled with nothing in the logs,
and one let a single bad tick kill the pump.

## Planner jobs were never dispatched

The pump queued board, revision and split planner jobs without a start ref.
`vivi pool dispatch` refuses a job with no `--from` ("no from"), so each
planner job stayed pending for ever and no need was ever planned. A planner
job now starts at the target's dispatch ref (`[pump.dispatch] from`, else the
target's integration branch), as unit jobs do. A new test drives the queued
job's own `pool dispatch` line through the real job planner.

## Roles that cannot run now fail validate and doctor

`vivi pool dispatch` takes a model for a pump job from the role's band and
the pool's `[pools.<name>.bands]` table. A role with a band and a pool with
no bands table ("pool X has no model for band P2"), a role with neither band
nor model ("no model"), or a pool with no `[pools.<name>.harness]` left its
jobs pending with no warning.

`vivi config validate` and `vivi doctor` now check the `pump-planner`,
`merge`, `auditor` and `gate` roles (when present and active): the role's
pool must have a harness, and the model must resolve on that pool exactly as
dispatch resolves it. Each failure names the role and pool, quotes the
dispatch error and gives the fix: add `[pools.<pool>.bands]` entries and give
the role a band they serve. The hand role is not checked: the pump picks each
unit's model itself, from the pool's caps.

## `vivi config migrate` fills the merge pool

When migrate adds the pump-host pool for the merge role (`host`, else
`pump-host` or `local-host`), it used to write only `provider` and `slots`:
no source and no harness. It now copies, from the hand role's pool when that
is a local pool: the provider, `worktree_of` or `repo`, `build_dir`,
`image`, the `harness` table (its `run`, `models` and `env` names), the
`committer` and the `bands` table. `slots` still comes from
`[pump.merge] lanes`. The `review:` line about warm slots stays: the pool
still starts cold and needs seeding. A hand pool on another provider is not
copied and keeps the old "give it a source" advice.

Roles migrate adds (`pump-planner`, `auditor`, `gate`, `merge`) now start at
the hand role's band when it has one, so the migrated file passes the new
check. A hand role with no band leaves them without one.

## A tick error no longer kills the pump

A failed step in a tick (one failed `git ls-remote`, one bad need row)
returned an error out of the run loop, so the pump exited and the service
restarted it into the same failure. Now the need dispatch and collect steps,
the job dispatch step, and each need inside them, log the failure, raise a
`tick-step-failed` operator exception (in the pump home's `exceptions.txt`)
naming the step, and let the tick continue. A failed save of the pump state,
an unreadable control state and an invalid config still stop the pump. A
persistent failure is raised once, then again only when its error text
changes or ten minutes have passed; every failure is still logged.

## Upgrading from 16.0.0

1. `vivi update`, then restart the pump service with the command it prints.
2. Re-run `vivi config validate` and `vivi doctor`. The new model and harness
   checks may now report roles that 16.0.0 silently stalled. Fix each one:
   add `[pools.<pool>.bands]` entries (`vivi pool bands set <pool> P2 <model>
   <reasoning>`) and give the role a band they serve (`vivi role set <role>
   --model P2`), or add the pool's `[pools.<pool>.harness]`. The pump and the
   pump service refuse to start on a validate error.
3. If you ran `vivi config migrate --write` on 16.0.0, its `host` pool has no
   source and no harness. Copy them from the hand role's pool by hand, or
   delete the `[pools.host]` table and the merge role's `pool` line and run
   `vivi config migrate --write` again.
