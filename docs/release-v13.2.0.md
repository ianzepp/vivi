# Vivi 13.2.0

A minor release that keeps the pump's merge line moving when checks go red.
Fix units now reach the integration branch first, merging runs in parallel
lanes, and a batch that fails a shared check is set aside and repaired instead
of being merged one unit at a time.

## Priority and pull-model merging

`vivi pump priority set|clear|list` stores need priority levels in `pump.db`;
the old `priority.txt` is imported once at startup and renamed
`priority.txt.imported`. Units rank fix units first, then by need level, then
plan order. Merge lanes now take the top-ranked batch at the moment they are
free, so priority holds under a backlog; `merging` means a unit is inside a
lane, and units waiting for a lane stay `delivered`.

## Parallel merge lanes

`[pump.merge] lanes` (default 1) runs that many independent merge lanes per
target. Lane 0 uses the target's integration checkout; further lanes work in
their own clones under `<home>/merge-lanes/<target>/<n>`, created on first
use. Each lane publishes on its own: it merges a moved remote tip and pushes
without re-running checks, leaving cross-lane breaks to the gate. A lane that
loses three batches in a row to lane-level failures backs off for ten minutes
and raises `merge-lane-down`.

## Quarantine

A batch of several units that fails a shared check is pushed to
`pump/quarantine/<id>`, its units become `quarantined`, and one repair unit is
dispatched from that branch at the top of the queue with the failing command
and output. When the repair merges, every quarantined unit lands with it. A
repair that goes stuck or finds no defect releases the units to merge one per
batch. The failing check's output tail is now logged.

## Promotion, gates and alarms

Main is promoted when the gate's failing set is a subset of main's own
(never redder than main), instead of requiring every owned failure fixed.
Promotion fetches the integration branch first, so tips pushed by any lane
promote. A merged fix unit forces the next gate; the forced-gate flag is per
target. New `[pump.alarm]` exceptions flag a fix unit waiting too long, no
promotion for hours, and stalled merging; `status.txt` gains three stall
lines.

## Dispatch

A unit kind names several pools: `pools = ["a", "b"]` under
`[pump.dispatch.kinds.<name>]`, and a unit goes to the listed pool with the
most free slots.

## Compatibility

- `[pump.dispatch.kinds.*] pool = "x"` no longer parses; write
  `pools = ["x"]`.
- `[pump] priority_file` is accepted and ignored.
- Pump database schema: version 5 (adds `need_priority`; `promotions` gains
  `failing_json`). Older binaries refuse a version 5 database.
- Mailspace schema: unchanged (8).
- Remote merge batch ids are longer (`b<secs>-<target>-<lane>-<seq>`); a remote
  merger must treat the id as an opaque stem.
