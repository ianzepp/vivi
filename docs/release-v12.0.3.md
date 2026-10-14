# Vivi 12.0.3

A patch release for `vivi pump`. The mailspace schema stays at 8; nothing to
migrate.

## One merger at a time across a pump restart

A batch offered to a remote merger can outlive the pump process that offered
it, for example when the pump is restarted while the merger is still working.
The new process put those units back in the queue and offered a fresh batch.
The merger was still busy, so it did not claim the batch in time, and the pump
merged it locally. Both mergers then pushed the integration branch, and every
unit in the local batch failed with `push-failed`.

Before offering a batch, the pump now waits for any earlier batch that is
claimed but unanswered and younger than `merge.remote_limit`. It logs which
batch it is waiting for. A claim older than `remote_limit` is ignored, so a
merger that died does not hold the pump forever.

## Upgrading

Install 12.0.3 and restart the pump. The fix applies from the first offer the
new process makes, including the case where the restart happens while a batch
is being merged.
