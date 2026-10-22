# Vivi 19.0.3

Patch release on 19.0.2.

## Need merges: a valid receipt is accepted, and a bad one stops looping

The merge brief always showed a `job` entry in the `PUMP_MERGE_RESULT=` shape,
so a need-branch merge worker with no role sources added one naming its own
job. Every such receipt was rejected as "receipt did not match its unit
assignment", even when the merge was clean and every check passed, and each
rejection queued another full merge job with no limit.

- The brief shows a `job` entry only for each listed role source, never for
  the running job itself.
- A `job` entry naming the running job is ignored; any other unexpected
  source still fails validation.
- The `need-merge-unverified` exception now names the first validation
  failure (wrong target, missing unit, unexpected job source, ...).
- After three invalid receipts in a row for one need revision, the pump raises
  one `need-merge-receipt-invalid` exception naming the jobs and stops
  queuing merges for that need until a new plan revision (`vivi pump replan`).
  A valid receipt, including a valid `undecided` one, clears the count.
- "need unit disposition recorded" is logged when a disposition changes, not
  twice per tick.

## Worker pools: destroy works without a ledger entry

A worker slot whose ledger entry was gone (left by the create race fixed in
19.0.2) was never torn down: `destroy` printed `destroyed` and changed nothing,
so the 19.0.2 reconcile pass reported success every round while the slot kept
holding capacity. Teardown now falls back to the slot's own path, disk image
and directory names, detaches whatever is mounted at the slot path, and fails,
naming what remains, instead of reporting `destroyed`.
