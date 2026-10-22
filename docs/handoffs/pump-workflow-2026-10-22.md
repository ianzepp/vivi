# Handoff: how the vivi pump processes work (as of vivi 19.2.3, 2026-10-22)

Purpose: give a fresh session a distilled, code-grounded picture of the pump so
it can discuss restructuring the pump's workflow (a defined process, ideally a
distilled Tugboat/Speedboat + factory/delivery model) instead of patching one
defect at a time. Written by the session that ran the faberlang hub pump
ramp-up on 2026-10-22 and shipped vivi 19.0.2 → 19.2.3 that day.

Trust levels below: **[code]** = read in source this session; **[live]** =
seen in the hub pump's log/status; **[inferred]** = my reading, not verified.
Per workspace rule, docs are stale by default: the repo wins over this file.

---

## 1. Topology

- **Hub** (`hub`, Arch Linux) runs three system services as user `vivi`:
  `vivi-pump.service` (`vivi pump service run`, one child pump per registered
  project), `vivi-server.service` (hosts project mailspaces, MCP at
  `/v1/projects/<id>/mcp`), `vivi-proxy.service` (LLM front door: codex, grok,
  opencode-go, zai, deepseek, qwen; kimi dead). Registry:
  `/mnt/vivi/.config/vivi/pump-service.toml`. **[live]**
- **Project root** for faberlang: `/mnt/vivi/projects/faberlang/.vivi/`
  (`mailspace.toml`, mail store, `pump.db` schema 8, plans/, needs/, log.txt,
  exceptions.txt). Hub never builds. **[live]**
- **Lanes**: bare repos `/mnt/vivi/lanes/<owner>/<repo>.git`, served by the
  restricted `git` user; workers fetch/push there. **[live]**
- **Worker pools** (provider `worker`, driven over SSH by
  `src/pool/worker-macos.sh`; per-dispatch APFS or dir slots; runner
  `src/pool/job-run.sh`): **[live]**
  - `studio` (Mac Studio, 18 cores): 16 slots; hands (luna + deepseek, caps
    8+8), merge and gate jobs. Cargo `jobs = 4` per build (set after an
    overload at 16).
  - `mini-plan` (mini-m1): planners, opencode-go/deepseek-v4.1-flash xhigh, cap 8.
  - `mini-audit` (mini-m1): auditors, codex/gpt-6.1-sol medium, cap 4.
  - Model choice per job: role band P0–P5 → `[pools.<p>.bands]` → least-loaded
    model under its cap (`src/routing/model.rs pick_lane_with`). **[code]**

## 2. Inputs and identity

- Work enters as a **need** in the mailspace. Since vivi 19 the pump only works
  needs that have an open **pump record** (`vivi pump send --need <h>`; MCP
  `pump_send`). A need with no record is invisible to the pump. **[code]**
- Needs must be addressed to the pump identity (`[pump] to`, faberlang: `mind`),
  except needs the pump itself sent (gate fixes), accepted since 19.1.5. **[code]**
- Pump identities: `[pump] from` (default `operator`) sends pump letters;
  `reply_from` (default `mind`) posts pump replies. **[code]**
- **Priority**: `vivi pump priority set <need> <level>` (higher first; stored in
  pump.db). The blockers pass auto-sets level 1 on a stuck need that blocks many
  units. **[code]**
- **Targets**: `[pump.targets.<id>]` (path, branch, main, promote, merge
  overrides). Naming any target drops the implicit `default`; faberlang has
  `default` (radix), `gradus`, `inferentia`. Units name a target; `after`
  dependencies may not cross targets. **[code]**

## 3. The tick (what one pass does, roughly in order)

`src/pump/cycle.rs` / `run.rs` drive a tick; steps are "soft" (one failing
step logs and the tick continues: `tick_steps::soft_step`). **[code]**

1. **Collect** (`collect*.rs`, `pool/collect.rs`): read finished pool jobs'
   receipts (`@@vivi-pool receipt`, now up to 256 KiB), route each by job kind:
   hand delivery, planner result, merge receipt (`PUMP_MERGE_RESULT=`), gate
   result, audit report, need audit, landing/cleanup receipts.
2. **Board** (`board.rs`, `boardpass.rs`, `plan.rs`, `amend.rs`): for each
   need with an open record: launch a planner if unplanned; ingest plan files
   (`plans/<need>.json`, revisions `<need>.r<N>.json`); post replies; detect
   thread changes → revision; re-plan parked needs that got a reply; re-ask
   invalid plans; re-plan needs whose `waits_on` prerequisites landed.
   Board work runs partly in a detached worker and is merged back.
3. **Dispatch** (`dispatch.rs`, `jobs/dispatcher.rs`, `ready.rs`): pick ready
   units in ranked order, offer them to pools, send the task letter + brief.
4. **Merge** (`merge/*`): delivered units merge into their **need branch**
   `vivi/need/<id>` (one job per (target, need); capped by
   `[pump.merge] need_parallel`, default 4, since 19.2.1). Merge jobs run the
   shared checks + the unit's proof on a worker.
5. **Need audit** (`need_audit*`): when a need's units are merged, an auditor
   checks acceptance against the need (`verdict=residual|block_range|…`);
   residuals become follow-up needs.
6. **Landing** (`need_landing/*`): a frozen, audited need branch is published
   to the target's integration branch (`refs/heads/<target.branch>`), then
   cleanup removes the need branch.
7. **Gate** (`gate*.rs`): after merge batches, the gate runs on the
   integration tip (worker job). Red → **gatefix** (`gatefix.rs`): an auditor
   establishes failing tests, the pump sends a **gate-fix need** (pump-created,
   own record), which is planned/merged/landed like any need. Reds have an
   owner (`red owned by <need>`).
8. **Promote**: a clean gate on a target with `promote` advances `main`
   (`next → main`). faberlang: last promote 91eafb0e3e, ~24 h ago (gate red).
9. **Closeout** (`closeout.rs`): a need becomes `done` in the mailspace only on
   explicit `vivi pump closeout --close <need>`, and only when fully landed.
   Nothing does this automatically. **[code]**
10. **Cadence** (`cadence/*`): role jobs on timers (canary, lint, test,
    head-ceo/cto/cmo/cso/cxo). faberlang's are stale since 10-21 and the CEO/CTO
    heads are skipped by an open-wants cap. **[live]**

## 4. Planner contract (the core of "how work is shaped")

- Planner job input: need text + thread + the charter/rules
  (`rules.md` = pump-planner charter + project rules + pool rules), via `pi -p
  --append-system-prompt rules.md @brief.md`. Since 19.2.0 planners get every
  target checkout (`$WORK/targets/<key>/repo`). **[code]/[live]**
- Output: JSON between `PUMP-PLAN-BEGIN/END`: `verdict` (`units`, `hold`,
  `design`, `operator`, plus pump-side `invalid`), `reason`, `units[]` (`id`,
  `brief`, `target`, `write_scope`, `proof`, `after`, `tools`), `withdraw`.
- Parked verdicts (`operator`, `design`, `hold`) post on the need thread and
  wait; any non-pump reply newer than the plan re-plans (19.2.3). Their reason
  keeps 16k chars (19.2.2; units plans keep 400). **[code]**
- Ingest checks (19.2.0, `plan_check.rs`): logged bad-plan reasons;
  configurable `[[pump.plan.proof_rules]]` (faberlang: `corpus-suites/**` needs
  `faber corpus … --target`); vacuous proofs rejected; unlanded prerequisites →
  hold `waits_on`; merged units never re-dispatched; cross-need scope overlap
  warned and never co-dispatched. Rejection → re-ask with findings (2 re-asks,
  then hold). **[code]**
- Revisions (`amend.rs`, `revision_input.rs`): a thread change on a planned
  need asks the planner for `r<N+1>` with the full prior plan, per-unit state
  and amendable flag, and thread messages since the last plan. **[code]**

## 5. Readiness and ranking

- A unit dispatches when (`ready::is_ready`): pending, out of retry backoff,
  its need not held / not waiting on unlanded needs, no reserved-path overlap,
  `after` satisfied, no busy overlapping unit. **[code]**
- Rank (`ready::ranked`, `jobs/dispatcher dispatch_order`, merge cap ranking):
  fix units first, then need priority (highest first), then the rest, stable.
  **Known defect (fix in flight, 19.2.4):** "fix unit" = id starts with
  `fix_prefix` (`pumpfix`), but since 19.1.5 gate-fix needs use ordinary handle
  ids, so gate fixes rank like ordinary work. **[code]/[live]**

## 6. State: where it lives

- **pump.db** (sqlite): units, needs records, jobs, priorities, board state.
- **Files** under the project `.vivi/`: plans (+ superseded/, rejected/),
  needs/<need>.json (planner inputs, seen sets), log.txt, exceptions.txt,
  needs-ready.txt (legacy).
- **Mailspace**: needs, pump records, task letters per job (the job's thread
  carries dispatch note + the worker's receipt reply), event letters (marks,
  binds, dependency edits), replies.
- **Git**: lane repos (unit branches `vivi/unit/<uid>-a<n>`, need branches,
  integration branch, main).
- So one unit's truth is split across pump.db, plan files, mail threads and git
  refs, reconciled every tick. **[code]/[inferred]**

## 7. Where policy lives (relevant to "process vs patches")

| Kind of rule | Where it lives today |
|---|---|
| Unit/need state transitions, ordering, caps, retries, holds | Rust, hard-coded per module (plan.rs, ready.rs, merge/*, gatefix.rs, amend.rs, blockers.rs…) |
| Numbers and switches | `mailspace.toml` `[pump.*]` (dispatch parallel, merge lanes / need_parallel / shared_checks / checks_for, gate, targets, plan proof_rules, budgets) |
| How a plan / brief / audit should look | Role charters + planner `rules.md` (prompt text), plus parsers that enforce markers |
| Who does what | Roles (kind, pool, band) in `mailspace.toml`, via `vivi role` |

There is no explicit, declarative workflow (state machine / process spec) the
pump interprets; the process is the sum of modules and their tick order.
Tugboat/Speedboat/factory/delivery (skills under `~/work/ianzepp/skills/`, and
the embedded copies via `vivi skills show tugboat|speedboat`) are not encoded
in the pump; only fragments echo them (planner/auditor/merge roles, holds,
gates). **[inferred]**

## 8. Evidence for "whack-a-mole": defects fixed on 2026-10-22

Each fix patched one transition the implicit process got wrong:

| Release | Defect | Process gap it reveals |
|---|---|---|
| 19.0.2–19.0.3 | slot leaks; need-merge receipt loop; BUILD-FAILED parse | job lifecycle and receipt contract not uniform |
| 19.1.0 | `vivi pump recover`; MCP client | no first-class "unverified branch" state handling |
| 19.1.3 | all hands on one model | model routing policy implicit |
| 19.1.4 | tick > 1 min | serial dispatch/reconcile |
| 19.1.5 | gate-fix needs had no pump record; silent plan drops; lost revisions | pump-created work not a first-class input |
| 19.2.0 | revision answers dropped; revision input truncated; planners saw one repo; no plan validation | plan contract enforced only by prompt |
| 19.2.1 | ~20 concurrent merges overloaded studio | no resource model across job kinds |
| 19.2.2 | design reason cut to 400 chars | parked-verdict path an afterthought |
| 19.2.3 | only the operator could answer a parked plan | role identity hard-coded in transitions |
| 19.2.4 (in flight) | non-default-target gate audits never collected; gate-fix units not ranked first | job kind × target matrix not uniform; identity by naming convention |

Other standing gaps: "done" ≠ "on main" (closeout is manual; landing is the
integration branch); 21 h with no promote while the gate is red; stale cadence
jobs; open-wants cap skipping the heads that consume wants.

## 9. Pointers

- Code: `~/work/ianzepp/vivi/src/pump/` (module map above), `src/pool/`,
  `src/routing/`. Contract doc (stale-by-default):
  `skills/vivi/references/pump-contract.md`. Releases: `docs/release-v19.*.md`.
- Campaign draft for multi-project scheduling:
  `docs/pump-cluster-scheduler-campaign.md` (Slurm concept map).
- Hub ops: `hub` skill, hub `~/AGENTS.md`. Read-only pump inspection that works
  without a password: `ssh hub 'cd / && sudo -n -u vivi vivi pump status|log …
  --project /mnt/vivi/projects/faberlang'`; any `vivi pump *` subcommand is
  allowed; `sudo -n vivi-install vivi|vivi-server X.Y.Z` upgrades.
- Process skills to distill from: `~/work/ianzepp/skills/{tugboat,speedboat,
  factory,delivery,campaign}/`.
- Today's evaluations: planner review (DeepSeek v4.1 Flash ≈ GLM-5.3, more
  reliable; proofs weakest), scoping-loop test (need 1d35423b: design verdict →
  approval → 3 units), Flint local model (fallback only).

## 10. Asking the source session

The session that wrote this keeps operating the hub pump (19.2.4 in flight:
gate-audit collection + gate-fix ranking). Send it questions with SendMessage to
**"Faberlang development process ramp-up"** (ref `eda1eb`; ListAgents shows it). It
will answer from code and the live pump, and flag when an answer is inferred.
