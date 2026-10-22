# Vivi 19.0.0

Major release on 18.x. The pump's intake is now explicit: a need is planned
when it has an open **pump record**, and every operator verb that acts on a
dispatch acts on that record. This replaces the approval mark, the date cutoff
and the pump.db request queues. It is a clean break: nothing keeps the old
behavior alive, and an old config that still sets a retired key is refused.

## What changed

### Pump records are the pump's intake

A **pump record** is a mailspace item (kind `pump`, a sibling of task, need and
want). One dispatch of one need is one record: it names the need in
`X-Vivi-Need`, is addressed to the pump's identity (`[pump] reply_from`) and
covers the dispatch end to end. A second dispatch of the same need is a new
record. At most one record per need is open; `vivi pump send` and
`vivi pump reopen` check and act in one transaction, so two concurrent writers
cannot both open one.

- `vivi pump send --need <need>` opens a record. It is refused when the need
  is not addressed to `[pump] reply_from` (or held under another mailspace's
  address), so a record can never sit open where the board would not plan it.
- `vivi pump list`, `show`, `done`, `reopen` and `history` read and close
  records. A record is the pump's own: no backlog node, never dispatched by
  `vivi step`, not carried by `vivi sync`.
- The board plans exactly the open needs that have an open record. A mark, a
  configured sender or a date never makes a need plannable.
- The pump replies its progress (the plan, each attempt record and attempt
  status event, `implemented`, `merged`, `unverified`) on the record, not on the
  need thread. A plan verdict that waits for an operator ruling (`operator`,
  `design`, `hold`) stays on the need thread. `vivi need attempts` and the close
  timing line read attempts from the need's records too.
- The pump closes the record with an outcome: `landed` (the need closed with
  every unit satisfied), `gave-up` (planner failed three times, a need-audit
  hold, or stuck units with nothing else able to advance), `withdrawn`, or
  `superseded` (an operator replan; a fresh record opens).

### Requeue, withdraw and replan are actions on the record

`vivi pump requeue`, `withdraw` and `replan` no longer write rows into
pump.db. Each posts one reply on the need's open pump record whose headers
carry the action:

| Header | Value |
| --- | --- |
| `X-Vivi-Pump-Action` | `requeue`, `withdraw` or `replan` |
| `X-Vivi-Pump-Record` | the record the action was posted on |
| `X-Vivi-Need` | the need that record covers |
| `X-Vivi-Pump-Unit` | requeue: the unit |
| `X-Vivi-Pump-Attempts` | requeue: set the attempt counter to N |
| `X-Vivi-Pump-Fresh` / `X-Vivi-Pump-Refund` | requeue: `1` resets the counter / refunds transient failures |
| `X-Vivi-Pump-Force` | replan: `1` replans past live work or a running planner |

The body is the reason. The pump applies each action at its next tick, **exactly
once**: the applied letter ids are saved with the state in one transaction, and
a withdraw or replan's debt to close its record is part of the saved mark and is
paid with checks at every step, so a tick that dies at any point resumes without
losing or repeating anything.

- **requeue** puts the named unit back in line; the record stays open.
- **withdraw** withdraws the need's unfinished units, then closes the record
  `withdrawn`.
- **replan** supersedes the plan, clears the need's parked planner state, closes
  the record `superseded` and opens a fresh one.
- A need with **no open record** is refused and nothing is posted; the error says
  to run `vivi pump send --need <need>`. An action on a closed record is never
  applied.
- The library entry point is `Mailspace::pump_post_action`. It prints nothing,
  so a hosted server can post the same letters.

### Retrying a given-up need

Sending a new record for a need whose last record closed `gave-up` is the
retry. The pump notices a record it has not run the need under, clears the
planner state that parked it and requeues its stuck units with a fresh attempt
budget, so the need is planned again or resumed instead of closing `gave-up` at
once. A need-audit hold is not cleared: it waits for an operator ruling.

### `vivi pump adopt`

New command, and the upgrade step (below). It opens a record for every need the
pump has planned or is planning and has not finished that has no open record.
Idempotent; `--dry-run` lists what it would open; a handle that is not a
mailspace need (the `pumpfix-*` repair needs of Vivi 15) is reported as skipped.

### Schema 8

`pump.db` moves to schema version 8. The `requeue_requests`,
`withdraw_requests` and `replan_requests` tables are dropped when the store
opens. A request the old pump had not applied is discarded with a log line:
re-issue it against the need's open pump record. The saved state gains
`board.actions` (applied operator actions) and `board.records` (the record each
need last ran under) and loses `board.replan`; an older state.json still loads.
A pump on 18.x cannot open a schema 8 store ("newer than this binary"), so
upgrade the pump and its CLI together.

## Removed

- `[pump.board] mark` and `since` are errors in `vivi config validate`, and
  `vivi config migrate` removes them. (`[pump.board] from` stays: its first entry
  is the default `reply_to` for the pump's replies. It is no longer an intake
  filter; set `reply_to` explicitly if you want a different address.)
- The pump.db request tables and their store API (`add_*_request`,
  `pending_*_requests`, `mark_*_applied`), `requeue::apply_pending`,
  `withdraw::apply_pending`, `replan::apply_pending`, and the board's replan set
  (`st.board.replan`, the `replan` argument of `launch_planners`).
- `replan` of a need the pump holds no plan for used to log "unknown" and do
  nothing; it now clears the need's parked planner state and supersedes the
  record (a revival path).
- Withdraw and replan need an open record, so the `pumpfix-*` needs of Vivi 15,
  which have no mail need, can no longer be withdrawn or replanned through the
  CLI.

## Fixed

- The board pass owns the planner bookkeeping (`planner_fails`,
  `planner_retry_at`, `planner_pids`, `planner_failed`) but its results were not
  folded back into the live state, so a planner that kept failing never reached
  the three-failure park. They merge back now.

## Upgrading

1. Remove `mark` and `since` from `[pump.board]` (or run `vivi config migrate`).
   Review `from`: it only names the default reply address now.
2. Stop the pump, upgrade the binary, start the pump. It migrates `pump.db` to
   schema 8 on first open. Re-issue any requeue, withdraw or replan the old pump
   had queued and not applied: they are discarded.
3. Run `vivi pump adopt` (try `--dry-run` first). Without it, work that was in
   flight has no record and its replies are dropped until one is opened.
4. From here on, dispatch a need with `vivi pump send --need <need>`; retry a
   given-up one with another `pump send`; act on a dispatch with
   `vivi pump requeue|withdraw|replan`.
