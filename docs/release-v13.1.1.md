# Vivi 13.1.1

A patch release for the pump. The mailspace schema stays at 8 and the
pump database schema stays at 2; nothing to migrate.

## A dependent need's units wait for their dependency

`--depends-on` already made a prerequisite edge in the backlog graph, and
`vivi step` declined blocked nodes, but the pump's dispatch only checked
unit-level `after` edges. Units of a blocked need went out anyway, found
nothing to do because the dependency's code did not exist yet, and burned
every attempt to `stuck`.

Readiness now consults the graph. Each pass reads
`vivi graph blocked backlog --kind need --json` once, and a unit waits while
its need has a prerequisite that has not landed. A prerequisite counts as
landed when its Vivi node is done, or when the pump has already merged every
unit of that need — so a delivered dependency that nobody has closed yet does
not stall its dependents. The first tick of a hold appears in `log.txt` as

```
hold <uid>: need <need> waits on dependency <dep> not landed
```

The release is silent, and the same rule keeps a `delivered` unit out of the
merge feed, so a dependent cannot merge before its dependency. A failed graph
read logs `dependency gate unreadable` and holds nothing, so a broken read
cannot stall dispatch.

The motivating case was need `39ed90f8` (TS and Go target-native imports),
filed against the Python end-to-end proof. Its units dispatched and stuck
before the graph showed the need blocked by `7224bb29`. With this release the
units hold instead.

## New read: `vivi graph blocked`

```
vivi graph blocked [GRAPH] [--kind task|need|want|decision|stub|parked] [--json]
```

Lists every blocked node with the prerequisites it is still waiting on. The
JSON rows are `{source_id, kind, label, blocked_by}`. The graph defaults to
`backlog`. This is a read-only addition; `graph ready` and its JSON shape are
unchanged.

## Compatibility

- Mailspace schema: unchanged (8). Pump database schema: unchanged (2).
  `held_on` on a unit is runtime log-dedup state and resets when the pump
  restarts, which only repeats the hold line once.
- Existing `stuck` units are not re-dispatched automatically. The gate
  prevents new premature burns; a unit already parked needs a person.
- The pump runs one extra `vivi` read per pass while units are pending or
  delivered (dispatch and each merge feed). On the faberlang mailspace the
  filtered read takes about 0.25s.
