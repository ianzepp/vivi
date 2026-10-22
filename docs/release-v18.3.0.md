# Vivi 18.3.0

Minor release on 18.2.0. Three pieces toward creating a hub project in one
step: config inherited from a base mailspace, a pump that is "not ready"
instead of invalid, and a gate command you can set from the CLI or library.

## Inherited mailspace config

A `.vivi/mailspace.toml` above a project that says `base = true` is merged
under the project's own file whenever its config loads.

- Inherited: `pump`, `pools`, roles (`[[identities]]`, merged by name),
  `judgment`, `probes`, `absorb` and `memo`. Never inherited: `id`, `name`,
  `description`, `archive`, `sync`, mail and the store.
- Tables merge key by key and the deeper file wins; arrays replace whole.
- `unset = ["pools.studio.size_gib", "identities.reviewer"]` removes a value
  a base provided.
- Charters fall back to the nearest base's `charters/<role>.md`.
- Ancestors without `base = true` are ignored; an unreadable one ends the
  walk quietly.
- Writers (`vivi pool set`, `vivi role ...`) still edit only the project's
  own file. `vivi pump service add` and `vivi pump run` refuse a base.
- `vivi config show` and `vivi config validate` name the file each section
  came from.

## Not ready is not invalid

`vivi mailspace init` writes a starter `[pump]` table. A project that is
missing required items (pools for the pump roles, a gate command, a
branch, committer details) is now **not ready** rather than invalid:

- `vivi config validate` lists each missing item and exits **3**. Wrong
  values (bad numbers, pool names that point at nothing) still exit 1.
- `vivi pump run` and `once` hold: dispatch nothing, write the missing items
  to `status.txt`, and start for real once the items are filled. The service
  supervisor no longer crash-loops on such a project.
- `vivi pump health` reports `not ready: ...`; `vivi doctor` shows missing
  items as skipped findings with the fix.

## `vivi pump gate show|set`

`vivi pump gate set "<command>"` sets `[pump.gate] command` in the project's
own `mailspace.toml` with a format-preserving edit under the config lock.
`show` prints it, or `no gate command set` (a blank command counts as
unset). The library pair is `vivi::pump::gate_config::{set_command, command}`.
