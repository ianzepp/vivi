# Vivi 17.0.1

Patch release on 17.0.0. No configuration changes.

## Fix: the pump dispatches to worker pools

The pump finds room for a job by counting the free slots that
`vivi pool status --porcelain` lists. A pool whose slots exist only while a
job runs (`provider = "worker"`, and RunPod) lists no slots at rest, so the
pump counted it as full and never dispatched to it. A pump whose only pool
was a worker pool queued its jobs and then sat idle, logging nothing.

`vivi pool status --porcelain` now prints one more line for each such pool:

```text
capacity POOL FREE
```

`FREE` is the pool's `slots` less the pods it holds now. The pump adds it to
its free count. A worker that cannot hold a job right now still refuses it
with `no free slot`, and the pump then treats the pool as full for that
tick, as before.

Readers of `pool status --porcelain` that match lines by their first word
are unaffected; the new line starts with `capacity`.
