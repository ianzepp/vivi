//! Durable post-merge pool cleanup jobs.

use std::collections::BTreeSet;

use serde_json::json;

use crate::pool::clean::{ReclaimRequest, ReclaimUnit};
use crate::pump::host::Ctx;
use crate::pump::jobs::{JobSpec, JobStatus};
use crate::pump::state::{State, Status, Unit};

/// Queue cleanup for a merged unit. The worker receives a portable snapshot;
/// it never depends on the pump process or its `pump.db`.
pub fn enqueue_merged(ctx: &Ctx, st: &mut State, uid: &str, unit: &Unit) {
    if ctx.mailspace.is_empty() {
        return;
    }
    let source_project = match ctx.project.canonicalize() {
        Ok(path) => path,
        Err(error) => {
            ctx.exception(
                "reclaim-unavailable",
                uid,
                &format!("project root: {error}"),
            );
            return;
        }
    };
    let local_pools = local_pool_candidates(ctx, unit);
    let cleanup_pools = cleanup_pools(ctx, unit);
    let merging_targets = st
        .units
        .iter()
        .filter(|(other, value)| other.as_str() != uid && value.status == Status::Merging)
        .map(|(_, value)| value.target_id().to_string())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let request = ReclaimRequest {
        version: 1,
        source_project,
        mailspace_id: ctx.mailspace.clone(),
        unit: ReclaimUnit::from(unit),
        pools: cleanup_pools,
        merging_targets,
    };
    let request_value = match serde_json::to_value(&request) {
        Ok(value) => value,
        Err(error) => {
            ctx.exception("reclaim-unavailable", uid, &format!("snapshot: {error}"));
            return;
        }
    };
    let role = reclaim_role(ctx);
    let id = format!("reclaim:{}:{uid}", unit.target_id());
    let body = worker_brief(&request_value);
    let mut spec = JobSpec::new(
        id,
        "reclaim",
        role,
        format!("Reclaim merged work for {uid}"),
        body,
    );
    spec.pool_candidates.clone_from(&local_pools);
    spec.local_only = true;
    spec.target = Some(unit.target_id().to_string());
    spec.branch.clone_from(&unit.branch);
    spec.units.push(uid.to_string());
    spec.payload = json!({
        "request": request_value,
        "unavailable_reported": local_pools.is_empty(),
    });
    if local_pools.is_empty() {
        ctx.exception(
            "reclaim-unavailable",
            uid,
            "no local pool can access the mailspace cleanup resources; request remains pending",
        );
    }
    if let Err(error) = crate::pump::jobs::enqueue(st, spec) {
        ctx.exception("reclaim-enqueue", uid, &error.to_string());
        return;
    }
    ctx.log(&format!("reclaim queued {uid}"));
}

/// Refresh local-only candidates for pending reclaim jobs after a config reload.
pub fn refresh_pending(ctx: &Ctx, st: &mut State) {
    let merging_targets = active_merge_targets(st);
    let pending: Vec<String> = st
        .jobs
        .values()
        .filter(|job| job.kind == "reclaim" && job.status == JobStatus::Pending)
        .map(|job| job.id.clone())
        .collect();
    for id in pending {
        let Some(job) = st.jobs.get_mut(&id) else {
            continue;
        };
        let Some(mut request) = job
            .payload
            .get("request")
            .cloned()
            .and_then(|value| serde_json::from_value::<ReclaimRequest>(value).ok())
        else {
            continue;
        };
        let Ok(unit) =
            serde_json::from_value::<Unit>(serde_json::to_value(&request.unit).unwrap_or_default())
        else {
            continue;
        };
        let candidates = local_pool_candidates(ctx, &unit);
        request.pools = cleanup_pools(ctx, &unit);
        request.merging_targets.clone_from(&merging_targets);
        let Ok(request_value) = serde_json::to_value(&request) else {
            continue;
        };
        let reported = job
            .payload
            .get("unavailable_reported")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        job.pool_candidates.clone_from(&candidates);
        job.body = worker_brief(&request_value);
        job.payload["request"] = request_value;
        let Some(uid) = job.units.first().map(String::as_str) else {
            continue;
        };
        if candidates.is_empty() && !reported {
            ctx.exception(
                "reclaim-unavailable",
                uid,
                "no local pool can access the mailspace cleanup resources; request remains pending",
            );
            job.payload["unavailable_reported"] = json!(true);
        } else if !candidates.is_empty() && reported {
            ctx.resolve(
                "reclaim-unavailable",
                uid,
                "a local cleanup pool is available; reclaim will be dispatched",
            );
            job.payload["unavailable_reported"] = json!(false);
        }
    }
}

/// Apply terminal cleanup receipts once; no cleanup work runs in the pump.
pub fn collect_completed(ctx: &Ctx, st: &mut State) {
    let completed: Vec<String> = st
        .jobs
        .values()
        .filter(|job| {
            job.kind == "reclaim"
                && job.applied_at.is_none()
                && !matches!(job.status, JobStatus::Pending | JobStatus::Dispatched)
        })
        .map(|job| job.id.clone())
        .collect();
    for id in completed {
        let Some(outcome) = crate::pump::jobs::completion(st, &id).cloned() else {
            continue;
        };
        if outcome.state == "finished" {
            ctx.log(&format!("reclaim completed {id}: {}", outcome.detail));
        } else {
            ctx.exception(
                "reclaim-failed",
                &id,
                &format!("{}: {}", outcome.state, outcome.detail),
            );
        }
        crate::pump::jobs::mark_applied(st, &id, ctx.now());
    }
}

fn reclaim_role(ctx: &Ctx) -> String {
    ctx.routing
        .lookup("reclaim")
        .or_else(|| ctx.routing.hand(&ctx.cfg.to))
        .map_or_else(|| ctx.cfg.to.clone(), |role| role.name.clone())
}

/// Local providers are the only pools that can reach the shared slot roots.
fn local_pool_candidates(ctx: &Ctx, unit: &Unit) -> Vec<String> {
    let mut candidates = Vec::new();
    if let Some(role_pool) = ctx
        .routing
        .lookup("reclaim")
        .or_else(|| ctx.routing.hand(&ctx.cfg.to))
        .and_then(|role| role.pool.as_deref())
        && ctx
            .routing
            .pools
            .get(role_pool)
            .is_some_and(|pool| pool.provider == "local")
    {
        candidates.push(role_pool.to_string());
    }
    if let Some(pool) = unit.pool.as_deref()
        && ctx
            .routing
            .pools
            .get(pool)
            .is_some_and(|config| config.provider == "local")
        && !candidates.iter().any(|candidate| candidate == pool)
    {
        candidates.push(pool.to_string());
    }
    for pool in ctx
        .routing
        .pools
        .iter()
        .filter(|(_, config)| config.provider == "local")
        .map(|(name, _)| name)
    {
        if !candidates.iter().any(|candidate| candidate == pool) {
            candidates.push(pool.clone());
        }
    }
    candidates
}

fn cleanup_pools(ctx: &Ctx, unit: &Unit) -> Vec<String> {
    let local: BTreeSet<&str> = ctx
        .routing
        .pools
        .iter()
        .filter(|(_, pool)| pool.provider == "local")
        .map(|(name, _)| name.as_str())
        .collect();
    let mut selected = Vec::new();
    if let Some(pool) = unit.pool.as_deref() {
        if local.contains(pool) {
            selected.push(pool.to_string());
        }
    } else {
        selected.extend(local.iter().map(|pool| (*pool).to_string()));
    }
    if let Some(pool) = ctx.routing.merge().and_then(|role| role.pool.as_deref())
        && local.contains(pool)
        && !selected.iter().any(|selected| selected == pool)
    {
        selected.push(pool.to_string());
    }
    selected
}

fn active_merge_targets(st: &State) -> Vec<String> {
    st.units
        .values()
        .filter(|unit| unit.status == Status::Merging)
        .map(|unit| unit.target_id().to_string())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn worker_brief(request: &serde_json::Value) -> String {
    format!(
        "Perform post-merge cleanup for this mailspace. Write the JSON request below to `reclaim-request.json` in your current directory, then run `vivi pool clean --after-merged-request reclaim-request.json --porcelain`. Return the command output and exit status. Do not use a pump database or modify source files.\n\nRequest:\n```json\n{}\n```",
        serde_json::to_string_pretty(request).unwrap_or_else(|_| String::from("{}"))
    )
}

#[cfg(test)]
#[path = "reclaim_test.rs"]
mod tests;
