# Vivi 24.0.0

Major release on 23.0.2. Role replies are checked by the server while the
agent that wrote them is still running; a slot is kept until its work is
published; pool jobs keep their temporary files inside their own slot; roles
can be command checks that the pump runs on a cadence with no model; and the
pump's subsystems switch on and off one at a time, so an outside orchestrator
can drive needs while the pump runs only cadence and board work. `vivi pool
doctor --test` works on local pools again. Read **Breaking changes** before
upgrading a host.

## Breaking changes

- **A slot is freed only when its work is published.** Collection used to
  free a slot whatever the job's outcome, and a per-dispatch slot is destroyed
  when freed, so a job whose publish was refused, failed or never ran lost its
  commits. A slot is now freed only when the outcome says the work was
  published, was read-only, or never started. Otherwise the slot keeps its
  claim and its files (an overdue job is still stopped) until someone recovers
  the work and frees it with `vivi pool destroy`. A pool can therefore run
  short of free slots where 23 would have recycled them.
- **An accepted hand-in is the job's receipt.** When an agent hands its reply
  in with `vivi-submit` and the server accepts it, that reply is the receipt,
  in place of the agent's last printed message. A stopped slot's receipt is
  read the same way (`job/reply.md` before `agent.out`). With nothing handed
  in, the printed output is the receipt, as before.
- **Check durations may be decimal.** A `[[check]]` `duration` in a reply
  envelope is now any finite non-negative number (`1.5`), where 23 accepted
  whole numbers only. Priorities, run numbers and timestamps stay integers.
  Upgrade every binary that reads a project's mail together.
- **Roles carry `command` and `target`.** `[[identities]]` entries gain both
  fields, set with `--command` and `--target` on `vivi role add` and `vivi
  role set` (`role set` also takes `--clear-command` and `--clear-target`) or
  the matching MCP role operations.
  Upgrade every binary that reads a mailspace before writing them.
- **`[pump.subsystems]` refuses unknown keys.** The table is new; only
  `flow`, `cadence`, `board` and `integration` are accepted.

## Pool

- **Replies are validated at hand-in.** Every slot gets `vivi-submit` on its
  `PATH`. It posts a reply to the server's `POST /v1/reply/check/<kind>`, which
  runs the pump's own parser for that role and answers `accepted: answer
  <word>` or `rejected: <reason>`. The endpoint stores nothing and needs no
  key. A malformed reply is refused while its agent can still fix it. The
  server passes its address to its pump children as `VIVI_REPLY_CHECK_URL`,
  and role step, slice and correction briefs end with the exact hand-in
  command.
- **Temporary files stay in the slot.** A job's `TMPDIR` is a directory inside
  its slot, on the worker's job disk, and it is passed into the agent's
  environment. It is emptied when the next job starts. The agent's `PATH`
  starts with a `mktemp` wrapper, because macOS `/usr/bin/mktemp` ignores
  `TMPDIR`: test leftovers had twice filled a worker's system volume and
  stopped every job on it.
- **`vivi pool doctor --test` works on local pools** (#29). The live test
  builds its slots under `<pool>-doctor`, which the local provider refused
  ("slot spec does not belong to this local pool"). A local pool now serves
  its `-doctor` test pool, as the ssh and worker providers already did, and
  still refuses any other pool.

## Pump

- **Subsystem switches.** `[pump.subsystems]` has one switch each for `flow`,
  `cadence`, `board` and `integration`, all on by default, so a config
  without the table runs as before. A switched-off part is skipped on every
  tick while collection and job dispatch still run.
- **Command cadence roles.** A role with a `command`, a `cadence` and
  optionally a `target` is a command check. The pump runs the command as a
  read-only pool job with no model from the target's main. It skips a run
  when main has not moved since the last green, and it retries environment
  failures with backoff from 60 seconds up to 32 minutes. A non-zero exit of
  the command on a recorded commit is red. Not built yet: filing a need for a
  red, and the recovery report after it. A red is staged and holds the
  role's one run until that lands.
- **Reslicing.** Split, blocked-scope, follow-up and rework slicers start
  from one resolved, pinned base and name that snapshot in their briefs. A
  rework can route without a work-bearing need branch. Briefs lead with the
  slicing instruction and end with the exact reply format.
- **Envelope diagnoses.** Refusals for hand, correction, merge and
  post-landing repair replies share one evidence-aware diagnosis, and
  reslice alarms include missing-envelope and discarded-change facts.
- **Lenient envelope layout.** `[[check]]` and `[[follow_up]]` tables written
  below the closing `+++` fence, or in a second fence block, are moved into
  the front matter when they parse. A check row may carry free-text detail.

## Fixes

- Need-rung library selection works again after the workspace split.
- The cleanup rejection hook creates its hooks directory before installing.

## Testing

`cargo test` at the repository root runs only the root package since the
crate split. The full gate is `cargo test --workspace`.
