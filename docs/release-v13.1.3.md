# Vivi 13.1.3

A patch release for pump restart recovery. The mailspace schema stays at 8 and
the pump database schema stays at 2; nothing to migrate.

## Interrupted merge recovery

The pump controller now recovers persisted `merging` units to `delivered` on
startup and saves that repair before starting the merge worker. This restores
the in-memory queue after a controller restart without changing the general
SQLite store loader.

When a recovered unit's branch is already contained in the integration branch,
the pump restores its merge receipt from that integration tip. It skips the
formatter, proof and shared checks, scope attribution, and push, so a crash
after a successful push does not create a duplicate merge effect. An ancestry
check error remains retryable.

## Compatibility

- Mailspace schema: unchanged (8). Pump database schema: unchanged (2).
- The merge overlap guard is unchanged.
