# Vivi 21.3.0

Minor release on 21.2.0, from rehearsing new projects on hub's pump. A new
project needs fewer manual steps, and a land can be proved without reading
the repository on the host. Pairs with vivi-server 2.3.0. The letter and
pump formats are unchanged.

## Jobs work on the project's own repository

A job without a target (the planner, admitter and slicer steps) now runs on
the project's primary target: the explicit `default` target, else the only
target. Before, it took its repository from the pool's `remote_url` or
`repo`, so every pool a project used needed a per-project repository, and a
pool inherited from a host base refused those jobs ("repo is required to
dispatch"). Projects with no explicit targets, or several and no `default`,
are unchanged.

## A land names its commit

A need that lands now records the branch and commit it landed on its done
event, as close metadata and in the note: `landed by the pump flow: main at
<sha>`.

## Fewer false alarms on a new project

- A mailspace with no `[sync]` table (a hosted project is the server's own
  mailspace) no longer runs `vivi sync pull` on every board pass.
- A main tip whose tree is empty (a new project's initial commit) is skipped
  and recorded instead of running a main rung that can only fail with
  `flow-env-failure`. The next commit with files runs at once.
- A job held after repeated pool refusals is released when a config reload
  changes the config, and its `job-dispatch-refused` alarm is resolved. Before,
  it stayed held until an operator requeued it, even after the cause was fixed.

## Import an existing repository

`vivi project create --source <addr>` and `vivi project lane add --source
<addr>` fetch every branch and tag of an existing repository into the new
lane on this host, so a real repository is onboarded without a push from
outside. The branch must exist in the source.

## Known issue

`pool::local_git::tests::the_lock_holds_across_processes` can fail under
heavy parallel load (a forked sibling briefly shares the lock file); it
passes when run alone.
