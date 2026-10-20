# Vivi 16.0.0

A major release for the pump: it is now a dispatcher only. Planning,
merging, gating, need audits, landing, promotion, cadence and cleanup all run
as durable jobs on role pools, and only the privileged pool runner publishes
refs. Each planned need gets its own branch, which lands on the integration
branch only after an Auditor accepts it. The pump's planner is a new role,
`pump-planner`. Branch names, pool slot locations and several config keys
change, so follow the upgrade steps below.

## Upgrading from 15.0.0

1. Drain, then stop, each pump. Let in-flight units finish and merge until
   `vivi pump status` shows nothing inflight, delivered or merging. Then run
   `vivi pump stop` for each project. Branch namespaces change:
   `unit/...` becomes `vivi/unit/...`, `pump/quarantine/...` becomes
   `vivi/quarantine/...`, and need branches are new (`vivi/need/<need>`).
   16 only recovers, publishes and cleans up branches in the new namespaces.
   Work left in flight from 15 is not reliably picked up; expect to
   `vivi pump requeue` or `vivi pump replan` it.
2. While 15 is still installed, destroy the slots of every remote pool
   (`fly`, `docker`, `ssh`, `runpod`):
   `vivi pool destroy --pool <name> --all --yes`. 16 keys pool identity on
   the mailspace id, not the name slug, so it no longer recognises slots 15
   made. It creates new ones, and the old machines keep running (and on Fly
   or RunPod, keep billing) until you remove them.
3. `vivi update`, then restart the pump service with the command
   `vivi update` prints (launchctl on macOS, systemd on Linux). Restart the
   other vivi units it names too.
4. `vivi config migrate` (dry run). Read the plan, especially every
   `!! BEHAVIOUR CHANGE` line and every `review:` line. Then
   `vivi config migrate --write`. It keeps a timestamped
   `mailspace.toml.bak.<stamp>` beside the config, and a
   `charters/<role>.md.bak.<stamp>` for any charter it replaces.
5. Act on the `review:` lines. Typical ones: give a new `host` pool a source
   and seed it; give a new role a charter; remove old local slot
   directories; review charters that migrate kept (a pump role's charter is
   now its job's system prompt).
6. `vivi config validate` must show no errors and `vivi doctor` no `[fail]`
   lines (warnings are advisory). The pump and the
   pump service refuse to start on a validate error.
7. `vivi pump start`.

Delete old `unit/*` and `pump/quarantine/*` branches on the remote by hand
once you are sure nothing needs them. 16 never touches them.

## Config changes, 15 → 16

| Key or surface | What changed | If you do nothing | Migrate |
| --- | --- | --- | --- |
| `pump-planner` role | New. The pump sends whole-need, revision and split planning to the active role named `pump-planner` (kind `planner`) on its pool. A role named `planner` (for example a Tugboat planner seat) is no longer used by the pump and is left alone. | Validate errors; the pump will not start. | Yes. Creates it on the hand role's pool and writes a charter. |
| `pump-planner` charter | The job's system prompt is the role's charter above the pool rules. | Migrate and new mailspaces write the shipped default. | Yes. Copies the `[pump.planner] charter` file's text into the role; falls back to the shipped default. A charter with text is never replaced. |
| `[pump.planner] run`, `charter` | Retired. No local planner command runs. | Ignored. | Removes both. |
| Merge role | Must be kind `merge` with a pool. Merge jobs need push access to the remote. | A missing merge role, or one without a pool, is a validate error. A merge role still of kind `hand` fails validate, and the pool runner refuses its publication to `next`. | Yes. Sets `kind = "merge"`; adds a pool (see `[pump.merge] lanes`); adds the role if absent. |
| Merge charter | Merge jobs use the merge role's charter as the system prompt. | A custom charter keeps working. | Leaves it; a non-shipped charter gets a `review:` line. |
| Hand role (`[pump] to`) | Must have kind `hand`: only a hand-kind role may publish its `vivi/unit/<attempt>` branch. | Every unit fails with `not authorized to publish`. Validate and doctor report it. | Sets a missing kind to `hand`; any other kind gets a `review:` line and is left alone. |
| Auditor role | New requirement for need landing. An active role named `auditor`, or of kind `auditor`, with a pool runs the read-only need-acceptance audit. | Validate and doctor error. Without one, needs never land. | Gives an existing auditor the hand role's pool, or adds a read-only `auditor` there. A blank charter gets the shipped auditor charter; a custom one is kept with a `review:` line. |
| `[pump.gate] every_merges`, `every_seconds` | Ignored. A gate now runs after every merge batch that lands, plus when there is no baseline or `FORCE-GATE` is set. | **Behaviour change:** more gates than before if you had set a count or a timer. | Removes both, marked `!! BEHAVIOUR CHANGE`. |
| `[pump.merge] lanes` | Merge concurrency is the merge pool's `slots`. | Ignored; validate warns when it is not the default. | Removes it. If the merge role had no pool, adds a local pool (`host`, else `pump-host` or `local-host`) with `slots` taken from `lanes`. The new pool starts cold: give it `[pools.<name>.local] worktree_of` or `repo` and seed it. |
| `[pump.merge] remote_wait`, `REMOTE-MERGE`, `merge-jobs/` | Not used. `remote_limit` is now only the minimum merge-job budget (the larger of it and `[pump.dispatch] budget`). | Ignored. | No. |
| `[pump] priority_file` | Already ignored in 15; priority lives in `pump.db`. | Ignored. | Removes it. Use `vivi pump priority`. |
| `[pump.dispatch] lanes`, `[pump.dispatch.kinds.*] pools`, `default_kind` | Unused once the hand role has a pool. Kind `max` and `skip_proof` still apply. | Validate warns. | No. Delete them yourself. |
| `[pump.gate] pools` | Now fallback candidates after the gate role's pool and its fallback chain. | Same pools, lower precedence. | No. |
| Local pool slot root | Default moved from `~/vivi-pool/<mailspace>/<pool>/slot-N` to `<project>/.vivi/pool/<pool>/slot-N`. A shared root is keyed on the mailspace id. Local roots may now be relative to the project. | 15 slots are not reused. 16 seeds fresh slots on first dispatch. | For a mailspace without an id, sets `root = "~/vivi-pool"` so the old slots stay usable. With an id, prints a `review:` line: remove the old directories once the pools are idle, and run `git worktree prune` in the source repository for `worktree_of` pools. |
| Remote pool slots | Pool identity is the mailspace id, not the name slug. | 15 slots are orphaned (see upgrade step 2). `vivi pool doctor` warns on slots with an old mark that it still sees. | No. |
| `ssh` pool `root` | Still `~/...` or absolute only; a relative root is rejected. Unchanged from 15, now stated explicitly because local roots may be relative. | — | No. |
| `[pump.planner] split_timeout` | Deprecated alias of `split_hard_cap`, as in 15. | Read as the alias, with a warning. | Renames it. |
| New optional keys | `[pump.need] repair_rounds` (1); `[pump.planner] planners_max` (3); `[pump.proof] allow` (empty); `[pump.merge] checks_for` (none) and `repair_budget` (`proof_timeout` + 300 s); `[pump.gate] hold_on_red` (false), `hold_priority_floor` (0), `results_idle_seconds` (0, off); `[pools.<name>] fallback` (none), `build_baseline_gib` (none); `[pools.<name>.ramdisk]` (off); `[memo] hidden_tags`, `retain`. | Defaults keep 15 behaviour. | No. |
| `[pump.ramdisk]` | Parsed and validated (`lane_gib`, `margin_gib`, `env`) but not used by the live path, since the in-process merge lanes are gone. Use `[pools.<name>.ramdisk]`. | No effect. | No. |
| New mailspaces | `vivi mailspace init` and `vivi sync join` add `operator`, `merge` and `pump-planner` with shipped charters, and bind them to a local pool when one exists. | — | — |

## Breaking

- The pump no longer merges, plans, gates or pushes in process. Every piece
  of work is a durable job addressed to a role and run on that role's pool.
  A source test fails if `src/pump` grows an in-process merger, planner or
  `git push`.
- Only the privileged pool runner publishes refs. A merge-kind role may
  publish to the integration branch. A release-kind role may publish main,
  and only to an exact expected commit. A hand-kind role may publish only
  `refs/heads/vivi/unit/<attempt>`.
- Unit merges go to the need's own branch, not to `next`. A need lands on
  `next` only after an Auditor accepts it.
- The gate runs after every landed merge batch. `every_merges` and
  `every_seconds` are ignored.
- Branch namespaces are `vivi/unit/`, `vivi/quarantine/` and `vivi/need/`.
- The pump's planner is the `pump-planner` role. The `planner` role is not
  used by the pump.
- Pool identity keys on the mailspace id. Local slots default to
  `<project>/.vivi/pool`.
- The pump and the pump service refuse to start without a usable merge role
  and `pump-planner` role.

## The pump as a dispatcher

- A job is saved in `pump.db` before a slot is chosen, with a stable id, so
  queueing is idempotent. A dispatched job belongs to the pool. Killing the
  pump does not lose or repeat it: on restart the pump finds its tasks by
  job marker and applies their receipts once.
- Job kinds: planner (board, revision, split), merge, need-merge,
  need-audit, gate, audit, promote, cadence, digest, investigation,
  reclaim and ref-delete.
- Routing follows the role's pool, then that pool's `fallback` chain. A job
  with no free slot waits. Fallback cycles fail validation.
- The board pass receives the live config with each request, so config
  edits apply on the next tick.
- Promotion goes to an active release role with a pool, else to the merge
  role. It is recorded only after main is checked to contain the gated tip.
- Post-merge slot cleanup runs as a reclaim job on local pools. Failed
  attempt branches are deleted through a ref-delete job.

## Need acceptance and landing

- The first dispatch of a need reserves `vivi/need/<need>` from the
  integration tip. Its unit merges are serialized and publish only to that
  branch. Separate needs progress in parallel.
- When every unit of the current plan revision is settled, the branch
  freezes and a read-only audit job goes to the Auditor. The audit covers the
  exact range and accounts for each requirement.
- Verdicts:
  - `clean_pass` accepts the range.
  - `residual` accepts it and files follow-up needs for each finding.
  - `block_range` adds repair units on the need branch and re-reviews only
    the repair diff. `[pump.need] repair_rounds` (default 1) caps the
    repairs; after that the finding is held and escalated.
  - `block_ship` holds the branch and escalates.
- Only a current `clean_pass` or `residual` receipt queues the merge role
  to land the branch. The pump checks the published ref's ancestry before it
  records the landing, then deletes the need branch through a separate
  cleanup job.
- Receipts are bound to the job, the need's thread hash, the revision and
  the exact commits. A stale receipt cannot land anything.

## Merge agent

- The merge brief leads with diagnose and repair. The worker resolves
  conflicts and fixes failing checks inside each source's write scope.
  Isolating one source is a last resort. The live merge path no longer
  creates quarantine records; existing ones are still repaired.
- `[pump.merge] checks_for` maps write-scope globs to extra checks. They run
  after the shared checks and before the unit proofs, only for overlapping
  units.
- A timed-out shared check is retried once. A timeout that does not recover
  leaves the batch undecided instead of blaming a unit.
- Merge receipts report shared-check timeouts, retries and recoveries.
  `vivi pump status` sums them.

## Planning and thread changes

- A reply on a need records a `replied` event on the parent. Each board pass
  hashes every planned need's thread and asks the planner for a revision when
  the hash moves.
- A reply on a closed need reopens it and plans a revision. A landed need
  reopens the same way.
- At startup, and at the start of every collect and dispatch pass, units of
  needs closed outside the pump are withdrawn. Merged and no-defect units
  stay as they are.
- `vivi pump replan <need>` supersedes a plan and queues a fresh planner run
  (`--force` while work is live).
- `[pump.planner] planners_max` (default 3) caps concurrent split planners.
  A rate-limited planner keeps its retry budget.
- Proofs may be repo-relative executables and may start with `NAME=value`
  pairs. `[pump.proof] allow` adds binaries to the allowlist. A proof that
  goes bad before dispatch parks the unit as `needs-replan` without charging
  an attempt.

## Gate

- A gate pins one integration tip after each landed batch. Batches landing
  mid-gate coalesce into one follow-up gate.
- Red names are owned by read-only audit work on the Auditor role. Each root
  cause in the audit report becomes one fix unit. Audits retry at most three
  times per failing set, with backoff.
- `[pump.gate] hold_on_red` writes `HOLD-MERGE` while owned reds are
  unfixed. Fix units, needs at or above `hold_priority_floor`, and units
  whose scope is clear of red crates still merge.
- A test target that dies keeps its owned names red (`TARGET-DIED`). A
  missing tool is an environment failure, not a new red.
- An external result wait applies only while the results directory shows a
  live heartbeat. `results_idle_seconds` stops waiting on a silent runner.
- The last green commit below a red tip can be promoted. `vivi pump status`
  and the stall alarm show the last green commit.

## Mind oversight

A mind-kind role with a cadence gets a read-only oversight wake. Its brief is
a compact pump-state snapshot. The worker returns `PUMP_OVERSIGHT_RESULT=`
JSON. Allowed actions: `hold-merge`, `clear-hold-merge`, `skip-unit`,
`unskip-unit`, `replan-need` and `note`. Anything else is refused and
recorded. Applying the result never blocks a tick.

## Cadence, memos and delegation

- Role cadences and digests run as pool jobs. The new `digest` role kind is
  read-only by default and saves digest memos.
- `vivi memo save --tag`; `vivi memo list|search` gain `--tag`,
  `--exclude-tag`, `--since`, `--limit` and `--all`. Digest memos are hidden
  by default. `[memo] hidden_tags` and `retain` configure this.
- `vivi need assign` and `vivi need withdraw` delegate needs to a local role
  or a synced member mailspace's role. `vivi boot` shows delegated needs.
- `vivi delivery send` and `vivi delivery close` hand a scoped Mind's
  integration branch to a merge role and record the merge commit.
- `vivi need done` and `vivi task done` take `--branch` and `--commit`.

## Pools

- Per-job RAM disks for local pools on macOS: `[pools.<name>.ramdisk]`
  (off by default; 8 GiB, 6 GiB margin). A job waits when memory is short.
  The disk is always torn down when the job ends. When the pump starts
  running, it sweeps orphaned `vivi-*` disks and their worktrees.
- `[pools.<name>] fallback` chains and `build_baseline_gib`.
- `vivi pool clean` touches only this pool's slots under a shared root.
- Provider failures restore the attempt with backoff and raise one
  provider-outage exception. A build-only proof killed by the lane timeout
  refunds the attempt.
- Fly: the API lease is held through claim, start and reset.

## New commands

- `vivi doctor` (`--json`, `--fix-hints`): one read-only verdict with
  ranked findings in install, mailspace, pump, sync, roles, pools, disk and
  proxy sections.
- `vivi monitor`: a read-only terminal view of running lanes across every
  registered mailspace (`--once`, `--json`).
- `vivi report records|throughput|durations|needs|failures` from synced
  attempt records.
- `vivi update` reports stale installed skill copies with the reinstall
  command, prints restart reminders for every installed vivi unit, and
  hints when its bin dir is off `PATH`.

## Docs

- `skills/vivi/references/pump-contract.md` and `pump-protocols.md` describe
  the dispatcher, routing, publication, acceptance and oversight rules.
- New references: `monitoring-and-reports.md` and the manual
  `bug-triage.md` playbook. The shipped `pump-planner` charter lives in
  `skills/vivi/references/charters/`.

## Known issues

- Each board pass reads the thread of every planned need with one
  `vivi need show --json` call per need. Large backlogs make the pass slow.
- `tests/pump_need_acceptance_cli.rs` takes about 150 s and can fail on its
  deadline on a loaded machine.
- A planner charter that migrate copies from a 15 `[pump.planner] charter`
  file may still tell the worker to write plan files. 16 asks for the plan
  between the `PUMP-PLAN-BEGIN` / `PUMP-PLAN-END` markers; migrate prints a
  `review:` line. Compare with the shipped
  `skills/vivi/references/charters/pump-planner.md`.

## Compatibility

- Mailspace storage schema: unchanged (9).
- Pump database schema: unchanged (6). 16 adds a `replan_requests` table
  when missing and keeps jobs and need state in the existing state store.
- Going back to 15.0.0 is a binary swap. 15 does not understand the new
  branch namespaces, need branches, durable jobs or `pump-planner` role, so
  drain the pump before you go back too.
- Upgrade every host that syncs a mailspace together. 15 peers do not act on
  need delegation.
