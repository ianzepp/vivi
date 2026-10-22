# Vivi 19.0.2

Patch release on 19.0.1.

## Worker pools: one slot per dispatch, and no leaked slots

Two dispatches in the same pump tick could both pick slot 1 on a `worker`
pool. The driver checked whether the slot existed before it took its lock,
so both passed; the second create then failed on the same disk image name,
and its teardown removed the first dispatch's boundary. The leftover slot
stayed listed as `unseeded`, counted against `slots`, and nothing reclaimed
it, so a two-slot pool ran one job at a time.

- `create` now takes the driver lock first and refuses an index whose slot
  directory or ledger entry exists (`slot exists`); the dispatcher moves on to
  the next free index and never tears down a slot it did not create.
- A create, seed or claim failure destroys the slot it made. If that teardown
  fails, the dispatch error says so and names `vivi pool destroy`.
- `vivi pool reconcile` (which the pump runs every tick that has work in
  flight) destroys a stopped worker slot that no open dispatch explains, once
  it is older than the setup grace (seed limit plus 60 seconds), reporting it
  as `orphan-stopped`. Other providers are unchanged.

## Pump gate: BUILD-FAILED is one state

A gate receipt that quoted the build error inside the FAILING markers stored
the error text as a second failing name next to `BUILD-FAILED`. A block that
holds `BUILD-FAILED` now reads as that single build-failed state, with the
rest kept only as receipt detail. A block holding a line that is not a
failing-name shape is unreadable (the existing `gate-unreadable` path and
forced retry), never a list of prose names. The gate brief now says the error
goes after FAILING-END. A pump that already stores a prose name in its
baseline keeps it until the next gate replaces the red set.
