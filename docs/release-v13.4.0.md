# Vivi 13.4.0

A minor release that gives every pump hand attempt a durable record and
gives every retry the history it needs.

## Every attempt ends committed, pushed and recorded

Whatever an attempt's outcome, the pool runner commits what the hand left;
a failed agent's commit is titled `WIP (attempt failed): ...`. The pump
pushes local-worktree attempt branches as plain new refs and keeps them,
and posts one attempt record on the need thread: unit, attempt, model,
pool, outcome, branch, task, base, commits, files changed, the failing
command with an excerpt, and the hand's final message from its receipt.
A record reports a rejected push or an unreadable branch as such. Record
reads never write shared refs, and every call they make has a 30 s
timeout. A nonzero agent exit other than a timeout is a failed attempt.

## Retries carry the history

A hand's brief lists every earlier attempt in order, with its model, how it
ended, its branch and an excerpt, and the newest replies on the need
thread. The brief ends by asking the hand for a short final report, which
becomes part of its attempt record.

## `vivi need show --newest N`

`vivi need show` gains `--newest <N>`: the newest N messages of the whole
thread, printed oldest first, in text or `--json`.

## Fixes

- The pump's own replies on a need thread no longer read as amendments, so
  attempt records start no revision planners. Thread hashes carry a scheme
  prefix, and a hash from the older scheme is adopted, so upgrading starts
  no revisions either.
- Agent exit 137 (a timeout whose SIGTERM was ignored) splits the unit like
  124, and the timeout check no longer matches text a hand quoted.

## Compatibility

- Pump database schema: version 6 (`unit_events` gains `branch` and
  `task`). Older binaries refuse a version 6 database: stop a running
  pump with its own binary before installing this one.
- Mailspace schema: unchanged (8).
