# Vivi 19.1.4

Patch release on 19.1.3: a pump tick no longer pays for its remote calls one
after another.

## The problem

On a hub pump with three SSH worker pools, a tick took 60 to 75 seconds
although `tick_secs` was 20. A tick spent 2 to 26 seconds collecting
(`vivi pool reconcile` visited each pool in turn, and every pool lists its
slots and polls its dispatches over SSH) and 9 to 25 seconds dispatching (each
new job built its lane inside the tick: claim a slot, create the image or
directory, deliver the job, fetch, seed, start, each an SSH call). A finished
job waited a whole tick to be collected, and starting several jobs cost
another tick.

## Parallel dispatch

New `[pump.dispatch] parallel` (default 6, the same as the default `per_tick`, at least 1) bounds how many pool
dispatches a tick runs at once.

- Unit starts already ran on one thread per planned unit, with no bound. They
  now share the bound; at the defaults (`per_tick = 6`, `parallel = 6`) they
  start together as before.
- Durable role jobs (hand, audit, gate, planner, merge, cadence) dispatched one
  at a time. They now go in groups of `parallel`. Within a group each job's
  pool is chosen first, in job order, counting the slots earlier jobs of the
  group will take; the task sends and `vivi pool dispatch` calls then run
  concurrently; the outcomes are applied in job order. A job whose pool
  refuses it moves on to its next candidate pool in a later round of the same
  group, as a serial run would.
- Selection, caps, least-loaded model choice, `per_tick`, free-slot counting
  and every state write stay on the tick thread. Only the remote calls
  overlap, and a dispatch that failed is handled exactly as before (the task
  is closed as superseded for a unit; the job stays pending with its task for a
  durable job).
- `parallel = 1` runs the dispatches one after another on the tick thread, in
  the old order.
- The slot claim is atomic in every provider (local and ssh hard-link, docker
  container name, fly lease, worker driver lock, runpod dispatch lock), so
  concurrent dispatches to one pool never take the same slot. A worker host
  creates slots under one lock and waits up to 30 seconds for it; a very high
  `parallel` against a single worker host can fail a dispatch with "the worker
  lock is busy", which the next tick retries.
- The `dispatch took Ns` log line is unchanged.

## Parallel reconcile

`vivi pool reconcile` without `--pool`, over more than one pool, now
reconciles each pool on its own thread (each thread opens its own provider)
and merges the reports: round lines in the order the dispatches were opened,
then each pool's slot cleanups in pool order, then the idle note. Dispatches
of pools no longer configured are closed as lost, as before. A pool that
fails reports its error after the others have finished. A single pool, or
`--pool`, runs as before. The pump's `collect took Ns` log line is unchanged.

## Upgrade

No configuration change is needed. Lower `[pump.dispatch] parallel` to 1 to
restore strictly serial dispatches.

On the macOS worker driver, slot creation (the APFS image or directory) holds
the driver lock, so creates on one worker host still run one at a time; only
delivery, fetch, seed and start overlap. A dispatch that waits more than 30 s
for that lock fails and is retried next tick.
