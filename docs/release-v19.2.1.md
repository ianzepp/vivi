# Vivi 19.2.1

Patch release on 19.2.0: need-branch merges no longer swamp a worker.

## Need-merge cap

The pump merged delivered units into their need branches with one merge job
per (target, need) and no limit across needs. With many needs delivering at
once it started about twenty merge jobs together, each running the shared
checks (a full build and tests), and an 18-core worker went past load 100
and stopped answering SSH.

- `[pump.merge] need_parallel` (default 4; also per target under
  `[pump.targets.<id>.merge]`) caps queued or running need-merge jobs per
  target. Branch init, landing and the shared integration lane take no slot.
- Need groups waiting for a slot start in ranked order: fix units first, then
  need priority level, highest first, then the rest. Their units stay
  delivered and are retried on the next tick.
- When groups are deferred the pump logs
  `need-merge cap <target>: <active>/<cap> active, <n> waiting` once per tick.
- `vivi config validate` rejects `need_parallel = 0`.
