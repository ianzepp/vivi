# Vivi 22.1.0

Minor release on 22.0.0. The hub can now run unreleased `vivi/main` builds,
with the mind deciding when a build goes back; pump dispatch takes a priority
in one call; thinking steps get configurable time budgets; and the CTO sees
more of why it was called.

## Edge builds and the mind's rollback

- Pushes to `vivi/main` trigger a Linux binary build and publication into a
  GitHub prerelease tagged `edge-<12-hex commit>`
  (`.github/workflows/edge.yml`; the newest 20 are kept). A newer push cancels
  an in-progress edge run. Prereleases are invisible to `vivi update` and
  `install.sh`.
- The mind gains an `[edge]` section in `mind.toml` (optional `status`, default
  `/var/lib/vivi-edge/status`) and a `rollback-edge` action at the `unstick`
  stage. It reads the host installer's status file and wakes at checkpoints
  5 and 20 minutes after an edge switch to judge the pumps. In acting mode
  (with `[mcp]` configured), it asks for a rollback by writing the active
  build's commit into `<mind home>/edge-rollback`. A request naming any other
  build is ignored by the host side. Without `[edge]` nothing changes.
- The host side (installer, follow timer, rollback path unit, and the mind and
  server pinned to the released binary) is host setup, not part of this binary.

## Dispatch and priority

- `vivi need send --dispatch [--priority N]` files a need and opens its pump
  record in one step; the hosted `send` tool takes `dispatch: true` and
  `priority`, and a new operator-only `pump_priority` tool sets, clears and
  lists per-need priority. Partial failures keep the filed letter and say what
  is left to do.

## Step budgets

- Research, plan, admit, slice, audit and judge steps take per-kind time
  budgets from configuration, with validated bounds; rework uses the slicer
  budget. Envelope correction has its own configurable budget. The flow CTO
  job continues to use the shared dispatch budget.
- Eligible thinking-step budget kills are classified before the envelope and
  get one durable rerun at a raised budget. Envelope-correction jobs and
  post-landing repair audits do not get that raised-budget rerun. Repair
  attempts take a configurable duration (`repair_attempt_secs`). Live reload
  affects new jobs only.

## Fixes

- CTO brief: carries each failed slice's attempt record (bounded) and says
  whether a need-branch row is recorded; a CTO `rework` answer on a need with
  no recorded branch row is refused before anything is routed.
- New lanes enable `uploadpack.allowReachableSHA1InWant` to serve reachable
  commits, so a job pinned to a main that has since moved still fetches.
- Runner commits keep their configured identity when the job inherits
  `GIT_AUTHOR_*` / `GIT_COMMITTER_*`.
- Prune job ids are scoped to the newest landing run, so a reopened need that
  lands a second time no longer collides with its first prune.
- Filing a repair need never fails the main rung with "unknown need": cached
  handle indexes refresh after another process commits to the mailspace, and a
  second tick cannot file the same repair twice.

## Upgrading a host

Install the release as usual. The edge channel is opt-in: it needs the host
setup and an `[edge]` section in the mind's `mind.toml`.
