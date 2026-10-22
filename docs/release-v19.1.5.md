# Vivi 19.1.5

Patch release on 19.1.4: gate fixes no longer stall.

## Pump-created needs under pump records

When a gate went red, the pump sent a gate-fix need (from its own identity to
the operator) but never opened a pump record for it, and `vivi pump send` /
`replan` refused it because it was not addressed to the pump identity. Its
revision plan could then be lost, and the need sat with `amendment_pending`
and nothing in flight, so the gate could not go green.

- The pump opens a gate-fix need's record when it sends the need.
  `vivi pump send --need` and `vivi pump adopt` accept any need the pump
  identity itself sent, whoever it is addressed to; other third-party needs
  are still refused. The board does not plan these needs itself.
- A plan file that cannot be applied (unparseable JSON, no `need`, a stale
  revision) is logged and raised as `bad-plan` / `revision-stale`; nothing is
  skipped silently. A plan wrapped in a code fence is unwrapped.
- A requested revision that goes unanswered is re-asked on the board pass
  with backoff (60 s doubling, three tries), then one `revision-failed`
  exception names `vivi pump replan <need>`.
- A board pass no longer copies a stale `amendment_pending` back over a
  revision that landed while it ran.

After upgrading, run `vivi pump adopt` once to open records for gate-fix needs
created by older versions.
