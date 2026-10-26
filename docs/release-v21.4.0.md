# Vivi 21.4.0

Minor release on 21.3.2. Pairs with vivi-server 2.4.0.

## New: `vivi mind`, a hub-wide pump supervisor

The mind is a long-running service that watches every registered project's
pump and wakes a model session when something needs a decision. It ships
here as a first release. Its authority stages are observe, unstick and
file-fixes; upgrading and releasing itself are refused. Filing a fix need is
not wired to the server yet.

- It polls each watched project about once a minute and wakes on a new
  signal: a degraded or stalled pump, a locked need, a held step, a red main,
  a repair alarm, an envelope failure, a stale pending job, an idle running
  step, or repeated dispatch failures. A heartbeat wake comes due at 1.1 times
  its cadence. Wakes are capped by
  `budgets.max_wakes_per_hour` (default 6), and each need has its own action
  budget.
- The wake log says why a wake began and shows readable local times. An
  attach probes every project first, so an operator's prompt sees fresh status.
- It acts through the server's MCP tools, the way any MCP client does, and
  runs no `vivi` command and writes no file in a project. A `[mcp]` table in
  `mind.toml` names the server and the key file. `pump_control` pauses and
  resumes a pump, `pump_action` requeues, replans, answers and unlocks,
  `pump_list` and `pump_log` read status, and `send` mails the operator. The
  server decides what the key may do. The model sees each tool's answer on a
  follow-up turn in the same wake, up to `follow_ups` turns (default 3,
  maximum 8). Without `[mcp]` the mind plans and logs but does nothing.
- Authentication is a plain bearer key for now. Signed sessions are a later
  phase.
- `mind.toml.example` documents every setting.

## Changed: the pump lands on `vivi/main`

The pump's integration branch is `vivi/main`, and a project's local `main`
stays a mirror of upstream. `vivi project create` and the lane `attach` now
default a target's `main` to `vivi/main`. Each pump run creates a `vivi/…`
main on the remote from the remote's default branch when it is missing, and
only under the `vivi/` prefix; any other missing main still fails the clone.
The commit guard allows pushes to `vivi/main`, not to other `vivi/` refs.

Existing projects keep the `main` they have.

## Fix: the repair need always has a recipient

`pump.reply_to` fell back to an empty string, so the repair need's
`need send --to` failed on every tick and the pump looped. It now falls back
to `operator`.

## Fixes: pools, config and proxy

- `config validate` runs each pool table's own checks.
- A missing provider setting is reported as not ready, not as an error, so
  `pool add` for a provider that is not set up yet is no longer rolled back.
- `pool bands set` writes the first band, and `pool set` takes a whole
  harness.
- `pool destroy` accepts `--porcelain`.
- The no-model error names the one write that sets pool and model.
- `head-cto` defaults to read-only, matching the role vivi ships.
- The commit-message guard sees the need again, and the pump's run suffix stays
  out of commits.
- `proxy check` fails a provider whose key command or credentials file is
  unusable.
- `sync join` succeeds when joined again with the same key and mailspace.
