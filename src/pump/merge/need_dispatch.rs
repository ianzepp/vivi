//! Durable, per-need unit merge jobs and frozen-range progression.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::error::VivariumError;
use crate::pump::config::PumpTarget;
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::jobs::{self, JobSpec, PoolJob};
use crate::pump::need::{self, NeedBranch, NeedBranchStatus};
use crate::pump::state::{State, Status};

use super::pool_dispatch::{MERGE_BRIEF, MERGE_TIMEOUT_RULES};
use super::select::take_batch;

pub(super) use super::need_branch_init::apply_branch_init;
pub(super) use super::need_receipts::apply_need_merge;

const NEED_MERGE_KEY: &str = "need_merge";

/// Queue branch initialization and route eligible units by `(target, need)`.
/// Managed need units are removed from the shared integration queue even when
/// branch creation is waiting, so they can never fall through to `next`.
pub(super) fn dispatch_units(
    ctx: &Ctx,
    st: &mut State,
    role: &str,
    targets: &BTreeMap<String, PumpTarget>,
    batches: &BTreeMap<String, usize>,
    waiting: &mut BTreeMap<String, VecDeque<String>>,
) -> Result<Vec<String>, VivariumError> {
    let mut started = Vec::new();
    seed_completed_no_defect_needs(ctx, st, targets, &mut started);
    let grouped = take_managed_waiting(ctx, st, waiting);
    for ((target, need_id), units) in grouped {
        if !targets.contains_key(&target) {
            continue;
        }
        let size = batches.get(&target).copied().unwrap_or(1).max(1);
        let result =
            dispatch_need_group(ctx, st, role, &target, &need_id, units, size, &mut started);
        // One need that cannot dispatch (its remote cannot be read, its row
        // is bad) must not stop the others, or the pump.
        crate::pump::tick_steps::soft_step(
            ctx,
            &format!("dispatch-need {target}/{need_id}"),
            result,
        );
    }
    refresh_freezes(ctx, st)?;
    Ok(started)
}

fn dispatch_need_group(
    ctx: &Ctx,
    st: &mut State,
    role: &str,
    target: &str,
    need_id: &str,
    mut units: VecDeque<String>,
    size: usize,
    started: &mut Vec<String>,
) -> Result<(), VivariumError> {
    let Some(revision) = retain_current_need_units(st, need_id, &mut units) else {
        return Ok(());
    };
    let mut branch = need::get(st, target, need_id)?;
    if branch.as_ref().is_some_and(|record| {
        record.status != NeedBranchStatus::Landed && record.landing_job_id.is_some()
    }) {
        ctx.log(&format!(
            "need branch held by active landing {target}/{need_id}"
        ));
        return Ok(());
    }
    if branch.as_ref().is_some_and(|record| {
        matches!(
            record.status,
            NeedBranchStatus::Collision | NeedBranchStatus::Unverified
        )
    }) {
        ctx.log(&format!(
            "need branch held for explicit recovery {target}/{need_id}"
        ));
        return Ok(());
    }
    if let Some(record) = branch.as_ref()
        && revision > record.revision
        && record.status != NeedBranchStatus::Landed
    {
        need::advance_revision(st, target, need_id, revision)?;
        branch = need::get(st, target, need_id)?;
    }
    if !check_external_prerequisites(ctx, st, target, need_id, revision, branch.as_ref())? {
        return Ok(());
    }
    let before: BTreeSet<String> = st.jobs.keys().cloned().collect();
    let can_route = need::ensure_creation(ctx, st, target, need_id, revision)?;
    add_new_jobs(st, &before, started);
    if !can_route || has_unsettled_need_merge(st, target, need_id) {
        return Ok(());
    }
    let Some(branch) = need::get(st, target, need_id)? else {
        return Ok(());
    };
    if branch.status != NeedBranchStatus::Active || branch.revision != revision {
        return Ok(());
    }
    enqueue_need_batch(
        ctx, st, role, target, need_id, revision, &branch, &mut units, size, started,
    )
}

fn retain_current_need_units(
    st: &State,
    need_id: &str,
    units: &mut VecDeque<String>,
) -> Option<u64> {
    let plan = st.needs.get(need_id)?;
    if plan.amendment_pending {
        return None;
    }
    let revision = plan.revision;
    let current: BTreeSet<String> = plan.units.iter().cloned().collect();
    units.retain(|uid| {
        current.contains(uid) && st.units.get(uid).is_some_and(|u| u.revision == revision)
    });
    (!units.is_empty()).then_some(revision)
}

fn check_external_prerequisites(
    ctx: &Ctx,
    st: &mut State,
    target: &str,
    need_id: &str,
    revision: u64,
    branch: Option<&NeedBranch>,
) -> Result<bool, VivariumError> {
    let dependencies = need::external_prerequisites(st, target, need_id, revision)?;
    let base_dependencies = branch
        .filter(|record| record.status != NeedBranchStatus::Landed || revision <= record.revision)
        .map(|record| &record.base_prerequisites);
    let added: Vec<_> = dependencies
        .iter()
        .filter(|dependency| base_dependencies.is_some_and(|base| !base.contains(*dependency)))
        .cloned()
        .collect();
    if !added.is_empty() {
        request_dependency_replan(ctx, st, target, need_id, revision, &added);
        return Ok(false);
    }
    for dependency in dependencies {
        let Some(dependency_need) = dependency.need.as_deref() else {
            request_dependency_replan(ctx, st, target, need_id, revision, &[dependency]);
            return Ok(false);
        };
        if dependency.target.as_deref() != Some(target) {
            request_dependency_replan(ctx, st, target, need_id, revision, &[dependency]);
            return Ok(false);
        }
        if !crate::pump::ready::need_landed(st, dependency_need) {
            ctx.log(&format!(
                "need branch waits: {need_id} revision {revision} depends on unlanded {dependency_need}"
            ));
            return Ok(false);
        }
    }
    Ok(true)
}

fn request_dependency_replan(
    ctx: &Ctx,
    st: &mut State,
    target: &str,
    need_id: &str,
    revision: u64,
    dependencies: &[need::NeedBranchPrerequisite],
) {
    let detail = dependencies
        .iter()
        .map(|dependency| {
            format!(
                "{}:{}@r{}",
                dependency.need.as_deref().unwrap_or("unknown"),
                dependency.unit,
                dependency.revision
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let unit_ids = need::units_for(st, target, need_id);
    let mut requested = false;
    for uid in unit_ids {
        let affected = st.units.get(&uid).is_some_and(|unit| {
            unit.revision == revision
                && unit.after.iter().any(|after| {
                    dependencies
                        .iter()
                        .any(|dependency| dependency.unit == *after)
                })
                && matches!(unit.status, Status::Pending | Status::Delivered)
        });
        if affected {
            requested = true;
            crate::pump::collect::needs_replan(
                ctx,
                st,
                &uid,
                "external prerequisite changed after need branch base",
                &detail,
            );
        }
    }
    if !requested {
        crate::pump::amend::request(
            ctx,
            st,
            need_id,
            &format!(
                "external unit prerequisite is absent from target {target} need branch base: {detail}"
            ),
        );
    }
    ctx.exception("need-external-prerequisite-replan", need_id, &detail);
}

fn enqueue_need_batch(
    ctx: &Ctx,
    st: &mut State,
    role: &str,
    target: &str,
    need_id: &str,
    revision: u64,
    branch: &NeedBranch,
    units: &mut VecDeque<String>,
    size: usize,
    started: &mut Vec<String>,
) -> Result<(), VivariumError> {
    let selected = retain_branchable(ctx, st, need_id, take_batch(st, units, size));
    if selected.is_empty() {
        return Ok(());
    }
    let Some(target_ctx) = ctx.for_target(target) else {
        return Ok(());
    };
    let Some(spec) = need_merge_spec(ctx, st, &target_ctx, role, branch, revision, selected)?
    else {
        return Ok(());
    };
    let id = spec.id.clone();
    let units = spec.units.clone();
    jobs::enqueue(st, spec)?;
    for uid in units {
        if let Some(unit) = st.units.get_mut(&uid) {
            unit.status = Status::Merging;
        }
    }
    ctx.log(&format!(
        "need merge job queued {id} target={target} need={need_id} revision={revision}"
    ));
    started.push(id);
    Ok(())
}

/// Refresh current need-branch freeze records after plan or unit lifecycle changes.
pub(crate) fn refresh_freezes(ctx: &Ctx, st: &mut State) -> Result<(), VivariumError> {
    let records = need::all(st)?;
    for branch in records {
        if st.closed_needs.contains(&branch.need) {
            continue;
        }
        let Some((revision, amendment_pending)) = st
            .needs
            .get(&branch.need)
            .map(|plan| (plan.revision, plan.amendment_pending))
        else {
            continue;
        };
        if (branch.status != NeedBranchStatus::Landed && branch.landing_job_id.is_some())
            || matches!(
                branch.status,
                NeedBranchStatus::Collision | NeedBranchStatus::Unverified
            )
        {
            ctx.log(&format!(
                "need freeze held for branch recovery or landing {}/{}",
                branch.target, branch.need
            ));
            continue;
        }
        if revision > branch.revision && branch.status != NeedBranchStatus::Landed {
            need::advance_revision(st, &branch.target, &branch.need, revision)?;
        }
        if amendment_pending {
            continue;
        }
        let Some(current) = need::get(st, &branch.target, &branch.need)? else {
            continue;
        };
        if current.status != NeedBranchStatus::Active || current.revision == 0 {
            continue;
        }
        record_terminal_dispositions(ctx, st, &current)?;
        if has_unsettled_need_merge(st, &current.target, &current.need)
            || !need::current_units_settled(st, &current.target, &current.need, current.revision)?
        {
            continue;
        }
        let tip = current
            .branch_tip_sha
            .as_deref()
            .unwrap_or(current.base_sha.as_str());
        need::freeze(st, &current.target, &current.need, current.revision, tip)?;
        ctx.log(&format!(
            "need branch frozen {}/{} revision={} base={} tip={}",
            current.target,
            current.need,
            current.revision,
            short(&current.base_sha),
            short(tip)
        ));
    }
    Ok(())
}

/// Whether a pool merge job owns this need branch and should bypass next gates.
pub(crate) fn is_need_merge_job(job: &PoolJob) -> bool {
    job.kind == "merge" && job.payload.get(NEED_MERGE_KEY).and_then(Value::as_bool) == Some(true)
}

/// Whether this need has a pending, dispatched, or unapplied unit merge receipt.
pub(super) fn has_unsettled_need_merge(st: &State, target: &str, need_id: &str) -> bool {
    st.jobs.values().any(|job| {
        is_need_merge_job(job)
            && job.target.as_deref() == Some(target)
            && job.payload.get("need").and_then(Value::as_str) == Some(need_id)
            && (!job.status.is_terminal() || job.applied_at.is_none())
    })
}

/// Replace the ordinary target queue with only unowned/legacy units and group
/// managed units in first-seen priority order.
fn take_managed_waiting(
    ctx: &Ctx,
    st: &mut State,
    waiting: &mut BTreeMap<String, VecDeque<String>>,
) -> Vec<((String, String), VecDeque<String>)> {
    let mut grouped = Vec::new();
    for (target, queue) in waiting.iter_mut() {
        let mut legacy = VecDeque::new();
        let mut needs = Vec::<(String, VecDeque<String>)>::new();
        while let Some(uid) = queue.pop_front() {
            let Some(unit) = st.units.get(&uid) else {
                continue;
            };
            if unit.need.is_empty() {
                legacy.push_back(uid);
                continue;
            }
            if !st.needs.contains_key(&unit.need) {
                let need_id = unit.need.clone();
                if let Some(unit) = st.units.get_mut(&uid)
                    && unit.status != Status::NeedsReplan
                {
                    unit.status = Status::NeedsReplan;
                    unit.since = ctx.now();
                    ctx.exception(
                        "need-plan-missing",
                        &uid,
                        &format!("unit belongs to missing need plan {need_id}; held out of next"),
                    );
                }
                continue;
            }
            if let Some((_, units)) = needs.iter_mut().find(|(need_id, _)| need_id == &unit.need) {
                units.push_back(uid);
            } else {
                needs.push((unit.need.clone(), VecDeque::from([uid])));
            }
        }
        *queue = legacy;
        grouped.extend(
            needs
                .into_iter()
                .map(|(need_id, units)| ((target.clone(), need_id), units)),
        );
    }
    grouped
}

/// Seed a branch for a need whose current plan is already settled without merge work.
fn seed_completed_no_defect_needs(
    ctx: &Ctx,
    st: &mut State,
    targets: &BTreeMap<String, PumpTarget>,
    started: &mut Vec<String>,
) {
    let mut candidates = Vec::new();
    for (need_id, plan) in &st.needs {
        if plan.units.is_empty()
            || plan.revision == 0
            || plan.amendment_pending
            || st.closed_needs.contains(need_id)
        {
            continue;
        }
        for target in targets.keys() {
            let units: Vec<&crate::pump::state::Unit> = need::units_for(st, target, need_id)
                .iter()
                .filter_map(|uid| st.units.get(uid))
                .filter(|unit| unit.revision == plan.revision)
                .collect();
            if !units.is_empty()
                && units
                    .iter()
                    .all(|unit| matches!(unit.status, Status::Merged | Status::NoDefect))
            {
                candidates.push((target.clone(), need_id.clone(), plan.revision));
            }
        }
    }
    for (target, need_id, revision) in candidates {
        let result = seed_need(ctx, st, (&target, &need_id, revision), started);
        crate::pump::tick_steps::soft_step(ctx, &format!("seed-need {target}/{need_id}"), result);
    }
}

/// Seed the branch of one settled need, unless it is held or waiting.
fn seed_need(
    ctx: &Ctx,
    st: &mut State,
    (target, need_id, revision): (&str, &str, u64),
    started: &mut Vec<String>,
) -> Result<(), VivariumError> {
    let branch = need::get(st, target, need_id)?;
    if branch.as_ref().is_some_and(|record| {
        (record.status != NeedBranchStatus::Landed && record.landing_job_id.is_some())
            || matches!(
                record.status,
                NeedBranchStatus::Collision | NeedBranchStatus::Unverified
            )
    }) {
        return Ok(());
    }
    if !check_external_prerequisites(ctx, st, target, need_id, revision, branch.as_ref())? {
        return Ok(());
    }
    let before: BTreeSet<String> = st.jobs.keys().cloned().collect();
    need::ensure_creation(ctx, st, target, need_id, revision)?;
    add_new_jobs(st, &before, started);
    Ok(())
}

/// Add only newly enqueued durable branch-init jobs to the returned list.
fn add_new_jobs(st: &State, before: &BTreeSet<String>, started: &mut Vec<String>) {
    started.extend(st.jobs.keys().filter(|id| !before.contains(*id)).cloned());
}

/// Record the explicit revision-bound blocker for current Stuck/Replaced units.
fn record_terminal_dispositions(
    ctx: &Ctx,
    st: &mut State,
    branch: &NeedBranch,
) -> Result<(), VivariumError> {
    for uid in need::units_for(st, &branch.target, &branch.need) {
        let Some(unit) = st.units.get(&uid) else {
            continue;
        };
        if unit.revision != branch.revision {
            continue;
        }
        let (kind, detail) = match unit.status {
            Status::Stuck => (
                "stuck",
                unit.history.last().map_or_else(
                    || String::from("unit is stuck"),
                    |attempt| attempt.detail.clone(),
                ),
            ),
            Status::Replaced => ("replaced", unit.replaced_by.join(",")),
            _ => continue,
        };
        need::record_disposition(
            st,
            &branch.target,
            &branch.need,
            &uid,
            branch.revision,
            kind,
            &detail,
        )?;
        ctx.log(&format!(
            "need unit disposition recorded {}/{uid} {kind}",
            branch.need
        ));
    }
    Ok(())
}

/// Build the role assignment for one current-revision unit batch.
fn need_merge_spec(
    ctx: &Ctx,
    st: &State,
    target: &Ctx,
    role: &str,
    branch: &NeedBranch,
    revision: u64,
    units: Vec<String>,
) -> Result<Option<JobSpec>, VivariumError> {
    let id = next_need_merge_id(st, &branch.target, &branch.need)?;
    let need_ref = format!("refs/heads/{}", branch.branch);
    let starting_tip = branch
        .branch_tip_sha
        .as_deref()
        .unwrap_or(branch.base_sha.as_str());
    let runner_branch = need_merge_branch_name(&branch.target, &branch.need, &id);
    let Some(unit_sources) =
        need_unit_sources(ctx, st, &units, &branch.need, &branch.target, revision)?
    else {
        return Ok(None);
    };
    let payload = need_merge_payload(
        target,
        branch,
        revision,
        &need_ref,
        starting_tip,
        &runner_branch,
        unit_sources,
    );
    let subject = format!("merge {} units into their need branch", units.len());
    let body = need_merge_body(&payload);
    let mut spec = JobSpec::new(id.clone(), "merge", role, subject, body);
    spec.builtin_role = Some(String::from("merge"));
    spec.target = Some(branch.target.clone());
    spec.from_ref = Some(starting_tip.to_owned());
    spec.branch = Some(runner_branch);
    spec.publish_ref = Some(need_ref);
    spec.publish_contains = Some(starting_tip.to_owned());
    spec.budget = target
        .cfg
        .merge
        .remote_limit
        .max(target.cfg.dispatch.budget);
    spec.units = units.clone();
    spec.with = units
        .iter()
        .filter_map(|uid| st.units.get(uid).and_then(|unit| unit.branch.clone()))
        .collect();
    spec.payload = payload;
    Ok(Some(spec))
}

fn need_merge_body(payload: &Value) -> String {
    format!(
        "{MERGE_BRIEF}\n\n{MERGE_TIMEOUT_RULES}\n\nThis is an isolated need-branch merge. Merge only the listed unit sources onto `starting_tip_sha`, publish only to `need_ref`, and never publish these units to the shared integration ref. Merge each unit source with `git merge --no-ff --no-edit -m \"<plain summary of what is being integrated>\"` and commit the final tree with a plain message; the runner refuses to publish commits whose messages contain task, need, unit, attempt or job identifiers or branch or ref names. The immutable acceptance base is `frozen_base_sha`. The controller checks runner ancestry evidence for the starting tip and every merged source SHA captured in `unit_sources`.\n\nInput JSON:\n{payload}"
    )
}

fn need_merge_branch_name(target: &str, need_id: &str, job_id: &str) -> String {
    format!(
        "pump/merge/{}/need/{}/{}",
        safe(target),
        safe(need_id),
        job_id
    )
}

fn need_merge_payload(
    target: &Ctx,
    branch: &NeedBranch,
    revision: u64,
    need_ref: &str,
    starting_tip: &str,
    runner_branch: &str,
    unit_sources: Vec<Value>,
) -> Value {
    let merge = &target.cfg.merge;
    json!({
        "version": 1,
        "need_merge": true,
        "target": branch.target,
        "need": branch.need,
        "plan_revision": revision,
        "need_branch": branch.branch,
        "need_ref": need_ref,
        "frozen_base_sha": branch.base_sha,
        "starting_tip_sha": starting_tip,
        "integration_branch": target.cfg.branch,
        "integration_ref": format!("refs/heads/{}", target.cfg.branch),
        "integration_remote": target.cfg.remote,
        "runner_branch": runner_branch,
        "unit_sources": unit_sources,
        "role_sources": [],
        "format": merge.format,
        "shared_checks": merge.shared_checks,
        "checks_for": merge.checks_for,
        "generated": merge.generated,
    })
}

/// Shape unit sources in the dispatcher's preserved rank order.
fn need_unit_sources(
    ctx: &Ctx,
    st: &State,
    units: &[String],
    need_id: &str,
    target: &str,
    revision: u64,
) -> Result<Option<Vec<Value>>, VivariumError> {
    let Some(assignments) =
        need_unit_source_assignments(ctx, st, units, need_id, target, revision)?
    else {
        return Ok(None);
    };
    let mut sources = Vec::with_capacity(units.len());
    let local_refs: Vec<String> = assignments
        .iter()
        .filter(|source| source.local)
        .map(|source| source.reference.clone())
        .collect();
    let remote_refs: Vec<String> = assignments
        .iter()
        .filter(|source| !source.local)
        .map(|source| source.reference.clone())
        .collect();
    let Some(local_shas) = observed_local_ref_shas(ctx, target, &local_refs) else {
        ctx.log("need merge waits: one or more local unit refs lack a verified target SHA");
        return Ok(None);
    };
    let Some(remote_shas) = observed_ref_shas(ctx, target, &remote_refs) else {
        ctx.log("need merge waits: one or more unit source refs lack a verified remote SHA");
        return Ok(None);
    };
    for source in assignments {
        let source_shas = if source.local {
            &local_shas
        } else {
            &remote_shas
        };
        let Some(source_sha) = source_shas.get(&source.reference) else {
            return Ok(None);
        };
        sources.push(unit_source_payload(
            source.uid,
            source.unit,
            source.branch,
            &source.reference,
            source_sha,
        ));
    }
    Ok(Some(sources))
}

struct NeedUnitSource<'a> {
    uid: &'a str,
    unit: &'a crate::pump::state::Unit,
    branch: &'a str,
    reference: String,
    local: bool,
}

fn need_unit_source_assignments<'a>(
    ctx: &Ctx,
    st: &'a State,
    units: &'a [String],
    need_id: &str,
    target: &str,
    revision: u64,
) -> Result<Option<Vec<NeedUnitSource<'a>>>, VivariumError> {
    let mailspace = if ctx.cfg.targets.is_empty() {
        None
    } else {
        Some(crate::mailspace::Mailspace::discover(Some(&ctx.project))?)
    };
    let mut source_refs = BTreeSet::new();
    let mut assignments = Vec::with_capacity(units.len());
    for uid in units {
        let Some(unit) = st.units.get(uid).filter(|unit| {
            unit.need == need_id && unit.target_id() == target && unit.revision == revision
        }) else {
            return Err(VivariumError::Message(
                "a selected need unit changed ownership or revision before merge dispatch".into(),
            ));
        };
        let Some(branch) = unit.branch.as_deref() else {
            return Ok(None);
        };
        let reference = unit_source_ref(branch);
        if !source_refs.insert(reference.clone()) {
            return Err(VivariumError::Message(
                "two need units resolve to the same source branch".into(),
            ));
        }
        let pool = unit
            .pool
            .as_deref()
            .and_then(|name| mailspace.as_ref()?.config.pools.get(name));
        let Some(local) = unit_source_is_local(ctx, target, unit, pool) else {
            ctx.log(&format!(
                "need merge waits: local unit source for {uid} is not the target integration repository"
            ));
            return Ok(None);
        };
        assignments.push(NeedUnitSource {
            uid,
            unit,
            branch,
            reference,
            local,
        });
    }
    Ok(Some(assignments))
}

fn unit_source_ref(branch: &str) -> String {
    if branch.starts_with("refs/heads/") {
        branch.to_owned()
    } else {
        format!("refs/heads/{branch}")
    }
}

/// Whether a unit branch is owned by this target's shared local repository.
fn unit_source_is_local(
    ctx: &Ctx,
    target: &str,
    unit: &crate::pump::state::Unit,
    pool: Option<&crate::pool::PoolConfig>,
) -> Option<bool> {
    let Some(pool_name) = unit.pool.as_deref() else {
        return Some(false);
    };
    let target_ctx = ctx.for_target(target)?;
    if !ctx.cfg.targets.is_empty() {
        let pool = pool?;
        return Some(pool_uses_target_repo(pool));
    }
    match ctx.pool_sources.get(pool_name) {
        Some(source) => same_repo_path(source, &target_ctx.repo).then_some(true),
        None => Some(false),
    }
}

fn pool_uses_target_repo(pool: &crate::pool::PoolConfig) -> bool {
    pool.provider == "local" && (pool.worktree_source().is_some() || pool.github.is_none())
}

fn same_repo_path(source: &Path, target: &Path) -> bool {
    if source == target {
        return true;
    }
    match (source.canonicalize(), target.canonicalize()) {
        (Ok(source), Ok(target)) => source == target,
        _ => false,
    }
}

/// Resolve exact local refs only in the configured target integration repo.
fn observed_local_ref_shas(
    ctx: &Ctx,
    target: &str,
    references: &[String],
) -> Option<BTreeMap<String, String>> {
    if references.is_empty() {
        return Some(BTreeMap::new());
    }
    let target_ctx = ctx.for_target(target)?;
    let query = references
        .iter()
        .map(|reference| shell_quote(reference))
        .collect::<Vec<_>>()
        .join(" ");
    let format = shell_quote("%(objectname)%09%(refname)%09%(objecttype)");
    let output = target_ctx.git(&format!("for-each-ref --format={format} {query}"), 30);
    if output.code != 0 {
        return None;
    }
    let requested: BTreeSet<&str> = references.iter().map(String::as_str).collect();
    let mut observed = BTreeMap::new();
    for line in output.text.lines() {
        let mut fields = line.split_whitespace();
        let (Some(sha), Some(reference), Some(object_type)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if requested.contains(reference)
            && (object_type != "commit"
                || !need::is_full_sha(sha)
                || observed
                    .insert(reference.to_owned(), sha.to_owned())
                    .is_some())
        {
            return None;
        }
    }
    (observed.len() == requested.len()).then_some(observed)
}

fn unit_source_payload(
    uid: &str,
    unit: &crate::pump::state::Unit,
    branch: &str,
    source_ref: &str,
    source_sha: &str,
) -> Value {
    json!({
        "id": uid,
        "need": unit.need,
        "target": unit.target_id(),
        "plan_revision": unit.revision,
        "branch": branch,
        "source_ref": source_ref,
        "source_sha": source_sha,
        "title": unit.title,
        "proof": unit.proof,
        "write_scope": unit.write_scope,
        "kind": unit.tools,
    })
}

/// Query one exact remote ref without changing local Git refs or worktrees.
pub(super) fn observed_ref_sha(ctx: &Ctx, target: &str, reference: &str) -> Option<String> {
    observed_ref_shas(ctx, target, &[reference.to_owned()])?.remove(reference)
}

/// Query a set of exact remote refs in one read-only Git call.
fn observed_ref_shas(
    ctx: &Ctx,
    target: &str,
    references: &[String],
) -> Option<BTreeMap<String, String>> {
    if references.is_empty() {
        return Some(BTreeMap::new());
    }
    let target_ctx = ctx.for_target(target)?;
    let query = references
        .iter()
        .map(|reference| shell_quote(reference))
        .collect::<Vec<_>>()
        .join(" ");
    let output = target_ctx.git(
        &format!("ls-remote {} {query}", shell_quote(&target_ctx.cfg.remote)),
        30,
    );
    if output.code != 0 {
        return None;
    }
    let requested: BTreeSet<&str> = references.iter().map(String::as_str).collect();
    let mut observed = BTreeMap::new();
    for line in output.text.lines() {
        let mut fields = line.split_whitespace();
        let (Some(sha), Some(refname)) = (fields.next(), fields.next()) else {
            continue;
        };
        if requested.contains(refname) {
            if !need::is_full_sha(sha)
                || observed
                    .insert(refname.to_owned(), sha.to_owned())
                    .is_some()
            {
                return None;
            }
        }
    }
    (observed.len() == requested.len()).then_some(observed)
}

/// Extract a JSON string field.
pub(super) fn string_field(value: &Value, field: &str) -> Option<String> {
    value.get(field)?.as_str().map(str::to_owned)
}

/// Stable merge-job IDs are isolated by target and need, then incremented.
fn next_need_merge_id(st: &State, target: &str, need_id: &str) -> Result<String, VivariumError> {
    let prefix = format!("merge-need-{}-{}-", safe(target), short_hash(need_id));
    let mut next = 0u32;
    for id in st.jobs.keys() {
        let Some(suffix) = id.strip_prefix(&prefix) else {
            continue;
        };
        let sequence = suffix.parse::<u32>().map_err(|error| {
            VivariumError::Config(format!("invalid need merge job sequence {id}: {error}"))
        })?;
        next = next.max(sequence);
    }
    let next = next.checked_add(1).ok_or_else(|| {
        VivariumError::Config(format!(
            "need merge sequence exhausted for {target}/{need_id}"
        ))
    })?;
    Ok(format!("{prefix}{next}"))
}

/// Keep the helper's identifiers independent of punctuation in mailspace handles.
fn short_hash(value: &str) -> String {
    hex::encode(&Sha256::digest(value.as_bytes())[..6])
}

fn safe(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn retain_branchable(ctx: &Ctx, st: &mut State, need_id: &str, units: Vec<String>) -> Vec<String> {
    units
        .into_iter()
        .filter(|uid| {
            let missing = st.units.get(uid).is_some_and(|unit| unit.branch.is_none());
            if missing {
                if let Some(unit) = st.units.get_mut(uid) {
                    unit.status = Status::Held;
                }
                ctx.exception(
                    "need-merge-branch-missing",
                    uid,
                    &format!("current unit for need {need_id} has no source branch"),
                );
            }
            !missing
        })
        .collect()
}

fn short(value: &str) -> String {
    value.chars().take(10).collect()
}

#[cfg(test)]
#[path = "need_dispatch_test.rs"]
mod tests;
