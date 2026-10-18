# Vivi 13.1.4

A patch release for reliable, higher-throughput pump operation. This includes
the project pump supervisor, durable control state, shared-pool dispatch for
workspace targets, and recovery fixes for interrupted pump work.

## Pump supervision and durable state

The user-level pump service supervises registered project pumps and restarts a
project pump after it exits. Running, paused and stopped modes are stored in
`pump.db`, so control state survives service restarts. Schema migration takes
the project pump lock; a status read no longer migrates or changes the live
database. A state-save or control-read failure now stops the pump instead of
letting it continue with state that was not durably recorded.

## Recovery and dispatch

At startup the pump recovers pushed unit branches whose result was interrupted
before it could be saved, avoiding a duplicate worker assignment. Delivered and
merging work no longer blocks unrelated dispatch through the write-scope guard;
only active inflight work holds that overlap. Dispatch runs before the periodic
board pass, and the board cadence starts when a pass completes, so a slow pass
does not delay the first dispatch or immediately repeat.

Workspace targets can dispatch through their configured shared pools.

## Compatibility

- Mailspace schema: unchanged (8).
- Pump database schema: version 4; earlier supported pump databases migrate
  forward under the pump lock. Do not run an older binary against schema 4;
  older binaries refuse to open a newer pump database.
