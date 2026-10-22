# Vivi 18.2.0

Minor release on 18.1.2. It adds a read-only pump health verdict.

## `vivi pump health` and `vivi::pump::health`

A monitor can judge a pump without running its commands or touching its
lock. The verdict is **ok**, **degraded**, **stalled** or **unknown**, with
one reason line per finding.

- **stalled**: control is running, and either the writer is alive with no
  tick inside `--stale-after` (default thirty ticks, at least 600 s), or the
  writer is gone and `status.txt` is older than that.
- **degraded**: control is paused or stopped, the writer is missing but the
  status is fresh, units are stuck, quarantined or need a replan, needs are
  planner-parked, the baseline is red, or `stall-*` exceptions fall in the
  window.
- **unknown**: no `[pump]` table, no `pump.db`, or a schema this binary does
  not read.

`--json` adds unit counts, recent merges, blocked units and parked needs
with reasons, complete-but-open needs and recent exceptions. Pass a previous
`as_of` as `--since` to get `changed_since`.

The read never writes, never migrates and never takes the pump lock:

- `pump.db` is opened read-only and read inside one read transaction
  (`PumpStore::load_snapshot`), so a read never mixes two saves.
  `need show` and `boot` now read the same way.
- The writer is found from `/proc/locks` on Linux. Elsewhere it is inferred
  from tick freshness, and the answer says which.

vivi-server 1.6.0 serves the same answer over HTTP for hosted projects.
