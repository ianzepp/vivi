//! The charters of the roles a configured pump runs, for the v20 envelope
//! check in `vivi config validate` and `vivi pump health`.

use crate::mailspace::MailspaceConfig;
use crate::mailspace::shipped::{
    ADMITTER_ROLE, HEAD_CTO_ROLE, REPAIRER_ROLE, RESEARCHER_ROLE, SLICER_ROLE, shipped_charter,
};

use super::model::from_mailspace;

/// The role the pump uses for each shipped pump role, paired with that
/// role's shipped charter. Roles resolve the way the pump finds them; a role
/// the project does not have is skipped, and each role appears once.
#[must_use]
pub fn pump_charter_roles(config: &MailspaceConfig) -> Vec<(String, &'static str)> {
    let routing = from_mailspace(config);
    let found = [
        ("pump-planner", routing.pump_planner()),
        ("merge", routing.merge()),
        ("auditor", routing.auditor()),
        (RESEARCHER_ROLE, routing.lookup(RESEARCHER_ROLE)),
        (ADMITTER_ROLE, routing.lookup(ADMITTER_ROLE)),
        (SLICER_ROLE, routing.lookup(SLICER_ROLE)),
        (REPAIRER_ROLE, routing.lookup(REPAIRER_ROLE)),
        (HEAD_CTO_ROLE, routing.lookup(HEAD_CTO_ROLE)),
    ];
    let mut out: Vec<(String, &'static str)> = Vec::new();
    for (shipped, role) in found {
        let (Some(role), Some(charter)) = (role, shipped_charter(shipped)) else {
            continue;
        };
        if !out.iter().any(|(name, _)| name == &role.name) {
            out.push((role.name.clone(), charter));
        }
    }
    out
}
