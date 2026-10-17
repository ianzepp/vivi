# Vivi 13.0.0

A major release for `vivi pump`.

The pump's live state moves from `state.json` into `pump.db` (SQLite). The
pump now picks up amendments to a planned need, closes a finished need on
request, and shows its state to the commands that already read a need. The
mailspace schema is unchanged; `pump.db` is a separate file in the pump home.

## `pump.db`

`<home>/pump.db` is the pump's memory: units, attempts, needs, plan
revisions, merges, gates, exceptions and the board bookkeeping. It is opened
with WAL, one writer, and a schema version (`PRAGMA user_version = 2`).
Unknown top-level and per-unit keys are preserved.

The first start with a predecessor's `state.json` imports it in one
transaction and renames the file to `state.json.imported`. The import is
one-way. `vivi pump export --json <path>` writes the live database in the old
`state.json` shape, which is the rollback path.

## Amendments

A plan is a snapshot. When the need thread gains a message after the plan,
the board pass notices the newer event, hashes the thread, and asks the
planner for `plans/<need>.r<N>.json`. The revision keeps every unit id:

- a merged or no-defect unit is immutable;
- an inflight, delivered, merging or splitting unit finishes, and the
  addendum becomes a new unit;
- a pending unit is amended;
- a unit absent from the revision is kept; `withdraw` names the pending
  units to remove;
- a changed merged or in-flight unit raises `revision-conflict`.

Every accepted revision is recorded in `plan_revisions`. The old plan file
stays on disk. A need with a pending amendment is not closed by `closeout`
until the revision lands.

## Closeout

The pump still never closes a need on its tick. `vivi pump closeout` lists
every need whose units are all satisfied while its Vivi copy is still open,
with the unit count, integration tip and completion time. `vivi pump
closeout --close <handle>` moves it as the item's holder (or `--for`) and
accepts it as the author. `--json` gives the same rows for a script.

## Pump state is visible

On the pump host, `pump.db` is read-only to the ordinary commands:

- `need show` appends a `pump` block: verdict, revision, unit counts,
  completion, registry state, the last unit event and any pending amendment;
- `need list --pump` appends `[pump r<N> M/T merged, open|complete|amending]`;
- `board --pump` adds a `pump (local)` block;
- `boot` adds a `pump: local execution state` section with unit counts,
  complete-but-open, amendments, stuck and exception counts.

A missing database means the pump state is not local. An unreadable or
older-schema database is named, never fatal. `vivi trace` does not show pump
state.

## Cross-host visibility

The `planned`, `revised-N` and `merged` replies on a need thread carry the
revision and unit state. Letters sync, so a reader on another host sees the
plan revision and completion state in the thread without `pump.db`. No
`mailspace_item_metadata` projection is written: that table is local to one
`mail.sqlite` and would not travel.

## Upgrading

1. Stop the running pump with `STOP` and wait for it to exit.
2. Back up the pump home (at least `state.json`, `plans/` and the pool
   settings).
3. Install 13.0.0 and restart the pump.

The first start imports `state.json` into `pump.db` and leaves
`state.json.imported` for rollback. See `docs/pump-db-migration.md` for the
full procedure.

## Compatibility

- Mailspace schema: unchanged.
- Pump database schema: version 2. Readers refuse an unknown version rather
  than guessing.
- `state.json`: import and export only; it is no longer written on each tick.
- Rollback to 12.x: stop the pump, run `vivi pump export --json <path>`, and
  start the 12.x pump on that file or on `state.json.imported`.
