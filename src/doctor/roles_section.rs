//! The role roster section of `vivi doctor`.
//!
//! Role bindings are probed with the quick process check. Findings contain
//! only role names and classified states; process names and arguments are
//! deliberately excluded.

use chrono::{DateTime, Utc};

use super::Finding;
use crate::doctor::Project;
use crate::duration::parse_duration_secs;
use crate::mailspace::{LocalIdentity, RoleView};
use crate::role_schedule::ScheduleState;
use crate::role_status::{self, ProcessState};
use crate::storage::Storage;

const SECTION: &str = "roles";
const ROSTER_FIX: &str = "add project roles with vivi role add <name> --project <root>";
const BINDING_FIX: &str =
    "refresh the role binding with vivi role set <name> --pid <pid> --project <root>";
const CADENCE_FIX: &str =
    "set a cadence for head roles with vivi role set <name> --cadence <duration> --project <root>";
const MERGE_POOL_FIX: &str = "vivi role pool merge <pool> --project <root>";

/// Report roster, bound-process, and cadence findings for this project.
#[must_use]
pub fn findings(project: &Project) -> Vec<Finding> {
    let views = project
        .config
        .identities
        .iter()
        .map(|identity| role_view(project, identity))
        .collect::<Vec<_>>();
    let mut findings = findings_from(&views);
    if project.config.pump.is_some() {
        let pools: Vec<&str> = project.config.pools.keys().map(String::as_str).collect();
        findings.push(merge_pool_finding(&views, &pools));
    }
    if views
        .iter()
        .any(|view| kind_is(view, "head") && view.cadence.is_some())
    {
        match project.open_storage_read_only() {
            Ok(storage) => {
                replace_head_cadence(
                    &mut findings,
                    head_cadence_finding_with_store(
                        &views,
                        &storage,
                        Some(&project.config.name),
                        Utc::now(),
                    ),
                );
            }
            Err(_) => replace_head_cadence(
                &mut findings,
                Finding::skipped(
                    SECTION,
                    "head cadence",
                    "mail store unavailable for cadence signals",
                ),
            ),
        }
    }
    findings
}

/// Build role findings from the configured roster.
///
/// This function is pure apart from the process liveness probes, which are
/// quick, local observations and never expose command lines.
#[must_use]
pub fn findings_from(views: &[RoleView]) -> Vec<Finding> {
    vec![
        roster_finding(views),
        bindings_finding(views),
        head_cadence_finding(views),
        non_head_cadence_finding(views),
    ]
}

fn roster_finding(views: &[RoleView]) -> Finding {
    if views.is_empty() {
        Finding::warn(SECTION, "roster", "role roster is empty", ROSTER_FIX)
    } else {
        Finding::ok(
            SECTION,
            "roster",
            &format!("{} role(s) configured", views.len()),
        )
    }
}

fn bindings_finding(views: &[RoleView]) -> Finding {
    let mut dead = Vec::new();
    let mut zombie = Vec::new();
    for view in views.iter().filter(|view| view.pid.is_some()) {
        let process =
            role_status::probe_quick(view.pid, view.host.as_deref(), view.harness.as_deref());
        match process.state {
            ProcessState::Dead => dead.push(view.name.as_str()),
            ProcessState::Zombie => zombie.push(view.name.as_str()),
            ProcessState::NotSet
            | ProcessState::Remote
            | ProcessState::Alive
            | ProcessState::Subagent
            | ProcessState::Unknown => {}
        }
    }
    if dead.is_empty() && zombie.is_empty() {
        let bound = views.iter().filter(|view| view.pid.is_some()).count();
        return Finding::ok(
            SECTION,
            "bindings",
            &format!("{bound} bound process(es) have no dead or zombie binding"),
        );
    }
    let mut details = Vec::new();
    if !dead.is_empty() {
        details.push(format!("dead: {}", dead.join(", ")));
    }
    if !zombie.is_empty() {
        details.push(format!("zombie: {}", zombie.join(", ")));
    }
    Finding::warn(
        SECTION,
        "bindings",
        &format!("stale process bindings: {}", details.join("; ")),
        BINDING_FIX,
    )
}

fn head_cadence_finding(views: &[RoleView]) -> Finding {
    let heads: Vec<_> = views.iter().filter(|view| kind_is(view, "head")).collect();
    if heads.is_empty() {
        return Finding::skipped(SECTION, "head cadence", "no head roles configured");
    }
    let missing: Vec<_> = heads
        .iter()
        .filter(|view| {
            view.cadence
                .as_deref()
                .is_none_or(|cadence| cadence.trim().is_empty())
        })
        .map(|view| view.name.as_str())
        .collect();
    let invalid: Vec<_> = heads
        .iter()
        .filter_map(|view| {
            let cadence = view.cadence.as_deref()?.trim();
            (!cadence.is_empty() && parse_duration_secs(cadence).is_err())
                .then_some(view.name.as_str())
        })
        .collect();
    if !missing.is_empty() || !invalid.is_empty() {
        let mut details = Vec::new();
        if !missing.is_empty() {
            details.push(format!("without cadence: {}", missing.join(", ")));
        }
        if !invalid.is_empty() {
            details.push(format!("invalid cadence: {}", invalid.join(", ")));
        }
        return Finding::warn(
            SECTION,
            "head cadence",
            &format!("head role(s) {}", details.join("; ")),
            CADENCE_FIX,
        );
    }
    Finding::ok(
        SECTION,
        "head cadence",
        &format!("all {} head role(s) declare cadence", heads.len()),
    )
}

fn head_cadence_finding_with_store(
    views: &[RoleView],
    storage: &Storage,
    legacy_domain: Option<&str>,
    now: DateTime<Utc>,
) -> Finding {
    let base = head_cadence_finding(views);
    if base.status != crate::doctor::Status::Ok {
        return base;
    }
    let heads: Vec<_> = views.iter().filter(|view| kind_is(view, "head")).collect();
    let mut never = Vec::new();
    let mut due = Vec::new();
    let mut overdue = Vec::new();
    for view in heads {
        let Some(cadence_seconds) =
            view.cadence
                .as_deref()
                .and_then(|cadence| match parse_duration_secs(cadence) {
                    Ok(seconds) => Some(seconds),
                    Err(_) => None,
                })
        else {
            continue;
        };
        match latest_signal(storage, view, legacy_domain) {
            Ok(None) => never.push(view.name.as_str()),
            Ok(Some(at)) => {
                let age = now.signed_duration_since(at).num_seconds().max(0) as u64;
                let state = cadence_state(age, cadence_seconds);
                match state {
                    ScheduleState::Due => due.push(view.name.as_str()),
                    ScheduleState::Overdue => overdue.push(view.name.as_str()),
                    ScheduleState::None | ScheduleState::Never | ScheduleState::Ok => {}
                }
            }
            Err(_) => {
                return Finding::skipped(SECTION, "head cadence", "cadence signal query failed");
            }
        }
    }
    let mut issues = Vec::new();
    if !never.is_empty() {
        issues.push(format!("no outbound signal: {}", never.join(", ")));
    }
    if !due.is_empty() {
        issues.push(format!("due: {}", due.join(", ")));
    }
    if !overdue.is_empty() {
        issues.push(format!("overdue: {}", overdue.join(", ")));
    }
    if issues.is_empty() {
        base
    } else if !overdue.is_empty() {
        Finding::fail(SECTION, "head cadence", &issues.join("; "), CADENCE_FIX)
    } else {
        Finding::warn(SECTION, "head cadence", &issues.join("; "), CADENCE_FIX)
    }
}

fn latest_signal(
    storage: &Storage,
    view: &RoleView,
    legacy_domain: Option<&str>,
) -> Result<Option<DateTime<Utc>>, String> {
    let mut names = vec![view.name.as_str()];
    names.extend(view.aliases.iter().map(String::as_str));
    let domain = view
        .address
        .rsplit_once('@')
        .map_or("", |(_, domain)| domain);
    let mut addresses: Vec<String> = names
        .iter()
        .map(|name| format!("{name}@{domain}"))
        .collect();
    if let Some(legacy_domain) = legacy_domain {
        addresses.extend(names.iter().map(|name| format!("{name}@{legacy_domain}")));
    }
    let message = storage
        .latest_message_from_addresses(&addresses)
        .map_err(|error| error.to_string())?;
    message
        .map(|message| {
            DateTime::parse_from_rfc3339(&message.date)
                .map(|date| date.with_timezone(&Utc))
                .map_err(|error| error.to_string())
        })
        .transpose()
}

fn cadence_state(age: u64, cadence: u64) -> ScheduleState {
    if age >= cadence.saturating_mul(2) {
        ScheduleState::Overdue
    } else if age >= cadence.saturating_add(cadence / 10) {
        ScheduleState::Due
    } else {
        ScheduleState::Ok
    }
}

fn replace_head_cadence(findings: &mut [Finding], replacement: Finding) {
    if let Some(finding) = findings
        .iter_mut()
        .find(|finding| finding.name == "head cadence")
    {
        *finding = replacement;
    }
}

fn role_view(project: &Project, identity: &LocalIdentity) -> RoleView {
    let sidecar = match std::fs::read_to_string(project.dir.join("mailspace.id")) {
        Ok(sidecar) => Some(sidecar),
        Err(_) => None,
    };
    let prefix = project
        .config
        .id
        .as_deref()
        .and_then(|id| id.get(..8))
        .or_else(|| sidecar.as_deref().and_then(|id| id.trim().get(..8)));
    let domain = prefix.map_or_else(
        || format!("{}.local", project.config.name),
        |prefix| format!("{prefix}.local"),
    );
    let charter = std::fs::read_to_string(
        project
            .dir
            .join("charters")
            .join(format!("{}.md", identity.name)),
    )
    .unwrap_or_default();
    RoleView {
        name: identity.name.clone(),
        address: format!("{}@{domain}", identity.name),
        aliases: identity.aliases.clone(),
        kind: identity.kind.clone(),
        labels: identity.labels.clone(),
        status: identity.status.clone(),
        harness: identity.harness.clone(),
        provider: identity.provider.clone(),
        model: identity.model.clone(),
        thinking: identity.thinking.clone(),
        pid: identity.pid,
        host: identity.host.clone(),
        cadence: identity.cadence.clone(),
        pool: identity.pool.clone(),
        read_only: identity.read_only,
        models: identity.models.clone(),
        has_charter: !charter.is_empty(),
        charter,
    }
}

fn non_head_cadence_finding(views: &[RoleView]) -> Finding {
    let unexpected: Vec<_> = views
        .iter()
        .filter(|view| view.cadence.is_some())
        .filter(|view| !kind_is(view, "head"))
        .map(|view| view.name.as_str())
        .collect();
    if unexpected.is_empty() {
        Finding::ok(
            SECTION,
            "non-head cadence",
            "no cadence on non-auto-run roles",
        )
    } else {
        Finding::warn(
            SECTION,
            "non-head cadence",
            &format!(
                "cadence set on non-auto-run role(s): {}",
                unexpected.join(", ")
            ),
            "clear the cadence with vivi role set <name> --clear-cadence --project <root>",
        )
    }
}

fn kind_is(view: &RoleView, expected: &str) -> bool {
    view.kind
        .as_deref()
        .is_some_and(|kind| kind.trim().eq_ignore_ascii_case(expected))
}

/// Pump-configured mailspaces need an active merge role bound to a real pool.
pub(super) fn merge_pool_finding(views: &[RoleView], pool_names: &[&str]) -> Finding {
    const NAME: &str = "merge pool";
    let Some(merge) = views
        .iter()
        .find(|view| view.name.eq_ignore_ascii_case("merge") || kind_is(view, "merge"))
    else {
        return Finding::fail(
            SECTION,
            NAME,
            "no merge role is configured for durable merge jobs",
            MERGE_POOL_FIX,
        );
    };
    match merge.pool.as_deref() {
        Some(pool) if pool_names.iter().any(|name| *name == pool) => Finding::ok(
            SECTION,
            NAME,
            &format!("merge role {} uses pool {pool}", merge.name),
        ),
        Some(pool) => Finding::fail(
            SECTION,
            NAME,
            &format!("merge role {} names unknown pool {pool}", merge.name),
            MERGE_POOL_FIX,
        ),
        None => Finding::fail(
            SECTION,
            NAME,
            &format!(
                "merge role {} has no pool; durable merge jobs cannot dispatch",
                merge.name
            ),
            MERGE_POOL_FIX,
        ),
    }
}

#[cfg(test)]
#[path = "roles_section_test.rs"]
mod tests;
