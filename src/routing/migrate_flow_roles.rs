//! `vivi config migrate`: the flow roles (researcher, admitter, slicer) and
//! their shipped charters.

use crate::mailspace::shipped::{
    ADMITTER_CHARTER, ADMITTER_ROLE, RESEARCHER_CHARTER, RESEARCHER_ROLE, SLICER_CHARTER,
    SLICER_ROLE,
};
use crate::mailspace::{LocalIdentity, MailspaceConfig};
use crate::pump::config::PumpConfig;

use super::migrate::{Assignment, CharterWrite, MigrateInputs, MigratePlan, NewRole, hand_pool};

const FLOW_ROLES: [(&str, &str); 3] = [
    (RESEARCHER_ROLE, RESEARCHER_CHARTER),
    (ADMITTER_ROLE, ADMITTER_CHARTER),
    (SLICER_ROLE, SLICER_CHARTER),
];

/// Plan the `researcher`, `admitter` and `slicer` roles: a missing role is
/// added read-only on the hand role's pool, a role with no pool gets that
/// pool, and a blank charter gets the shipped text. The band is the
/// operator's to set, so none is copied here.
pub(super) fn plan_flow_roles(
    config: &MailspaceConfig,
    pump: &PumpConfig,
    inputs: &MigrateInputs,
    plan: &mut MigratePlan,
) {
    let pool = hand_pool(config, pump, plan);
    for (name, shipped) in FLOW_ROLES {
        let existing = find_role(config, name);
        if existing.is_some_and(|role| role.status != "active") {
            continue;
        }
        let Some(pool) = pool.clone().or_else(|| existing?.pool.clone()) else {
            plan.review.push(format!(
                "{name} needs a pool: run vivi role pool {name} <pool> (add the role first with \
                 `vivi role add {name} --kind {name} --read-only true` if it is missing)"
            ));
            continue;
        };
        match existing {
            Some(role) if role.pool.is_some() => {}
            Some(role) => plan.assignments.push(Assignment {
                role: role.name.clone(),
                pool,
                reason: "flow roles run on the pool of the hand role".into(),
            }),
            None => plan.new_roles.push(NewRole {
                name: name.into(),
                kind: name.into(),
                pool,
                read_only: Some(true),
                model: None,
            }),
        }
        let blank = inputs
            .charters
            .get(name)
            .is_none_or(|text| text.trim().is_empty());
        if blank {
            plan.charters.push(CharterWrite {
                role: name.into(),
                origin: format!("the shipped {name} charter"),
                text: shipped.into(),
            });
        }
    }
}

fn find_role<'a>(config: &'a MailspaceConfig, name: &str) -> Option<&'a LocalIdentity> {
    config
        .identities
        .iter()
        .find(|role| role.name.eq_ignore_ascii_case(name))
}

#[cfg(test)]
#[path = "migrate_flow_roles_test.rs"]
mod tests;
