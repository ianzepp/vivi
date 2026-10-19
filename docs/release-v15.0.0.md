# Vivi 15.0.0

A major release for the pump: role-to-pool routing, self-healing dispatch
and merge, and a role-gated pump/pool snapshot in `vivi boot`.

## Breaking

- `[pump.planner] split_timeout` is renamed `split_hard_cap` (default
  5400 s). The old key is still read as an alias; it is reported as a
  deprecation warning with the other legacy keys, `vivi config migrate`
  rewrites it, and `split_hard_cap` wins when both are set.
- `steward`, `head` and `gate` role kinds are read-only by default.
- A `Depends-On` reference to an item not held in this mailspace now blocks
  readiness, surfaced as the new `awaiting_dependency` exception.
- The pump refuses to start on roster or routing validation errors: an
  identity that names no roster role, a role that names an unknown pool, or
  a gate role that is not read-only. Legacy `[pump.dispatch]` `lanes`/`kinds`,
  `merge.lanes` and `gate.pools` still work, with deprecation warnings.
- Local pools require an `origin` remote and reset each slot onto a fresh
  `origin/<main>`.
- `mailspace init` and `sync join` add an `operator` role.
- Proxy `/health` is ok when any provider is healthy; per-provider bodies
  gain a `status` field.
- Mailspace storage schema 8 → 9 migrates itself on first open. Opening the
  mailspace with 14.1.1 afterwards loses deferral marks.
- The new `promoted` sync event is not understood by 14.x peers.

### Upgrade notes

- Run `vivi config validate` before restarting a pump; fix any roster or
  routing error it names, then `vivi config migrate` to move legacy
  `[pump.dispatch]`, `merge.lanes`, `gate.pools` and `split_timeout` keys.
- Give every local pool repository an `origin` remote.
- Upgrade every host that syncs a mailspace together; do not go back to
  14.1.1 on a mailspace once 15.0.0 has opened it.
- Restart the pump service and the proxy unit after `vivi update`.

## Features

- Role → pool routing: `vivi config show|validate|migrate`,
  `vivi pool list|show|add|set|unset|remove|bands`,
  `vivi role set --pool/--models/--read-only` and `vivi role pool`.
- `vivi pool clean` and `vivi pool reset`.
- `vivi pump requeue`, plus transient-failure retry with backoff
  (`transient_max`, `transient_backoff`).
- A host-failure classifier with per-pool pause and probe-based resume.
- `[[pump.merge.generated]]`: regenerate generated files on drift instead
  of failing the merge on their conflicts.
- Blocker detection (`blocking_hold_units`, `blocking_replan_minutes`), top
  blockers in `status.txt`, and a stuck-root requeue.
- Proof-command validation and a `needs-replan` status.
- Split-planner retry and a re-ask on a bad plan.
- The board pass runs on its own thread with concurrent dispatch.
- Verdict/repo/tip headers on close and report letters, and a `promoted`
  sync event.
- `vivi need attempts --json` and timing summaries.
- `status.txt` is refreshed every tick; `vivi pump status` shows writer
  state and snapshot age.
- `vivi boot` prints a role-gated pump/pool snapshot from local sources
  (in-flight units, merge queue, held/stuck work, merge lanes, last promote,
  pools), with `--role`, `--pump full|own|none` and `--json` (PR #24).
- `vivi proxy check` probes the Codex stream.
- Backlog nodes and edges are created at ingest.

## Fixes

- The Codex proxy strips sampling parameters; `response.completed` parsing
  is fixed.
- Fly create recovers from a slow start and adopts unseeded slots.
- systemd/launchd install restarts the unit and reports its PID.
- Stale `index.lock` cleanup, lane-reset verification and merge-failure
  classification.
- Quarantine dissolves on repair timeout and bisects failed shared checks.
- Gate fixes get backing needs.
- The board plans prerequisites first.
- `vivi pool clean` compares canonical paths (macOS `/var` vs
  `/private/var`).
- `vivi need attempts --json` reports the handle that was asked for.

## Refactors and docs

- Test fixtures give local pool repositories an `origin` remote and count
  the `operator` role.
- Dead-code cleanup in mailspace item events.
- README, `skills/vivi/SKILL.md` and the tugboat/speedboat skills cover
  routing, pools and the boot pump snapshot.

## Compatibility

- Mailspace schema: 8 → 9 (automatic, one way).
- Pump database schema: unchanged (6).
