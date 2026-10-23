//! Replace 19.x pump-role charters (no v20 reply envelope) with the shipped
//! ones. A blank charter is handled by the role planners, not here.

use crate::mailspace::MailspaceConfig;
use crate::mailspace::shipped::pre_v20_charter;

use super::migrate::{CharterWrite, MigrateInputs, MigratePlan};
use super::pump_charter_roles;

/// For each pump role whose current charter is `pre_v20_charter`, plan the
/// shipped charter, unless the plan already writes that role.
pub(super) fn plan_stale_charters(
    config: &MailspaceConfig,
    inputs: &MigrateInputs,
    plan: &mut MigratePlan,
) {
    for (role, shipped) in pump_charter_roles(config) {
        let stale = inputs
            .charters
            .get(&role)
            .is_some_and(|text| pre_v20_charter(text));
        if !stale || plan.charters.iter().any(|write| write.role == role) {
            continue;
        }
        plan.charters.push(CharterWrite {
            origin: format!(
                "the shipped {role} charter (the current one has no v20 reply envelope)"
            ),
            role,
            text: shipped.to_string(),
        });
    }
}
