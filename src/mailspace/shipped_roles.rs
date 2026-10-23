//! Provisioning of the flow roles: researcher, admitter, slicer, repairer and head-cto.

use super::shipped::{
    ADMITTER_ROLE, HEAD_CTO_ROLE, REPAIRER_ROLE, RESEARCHER_ROLE, SLICER_ROLE, shipped_charter,
};
use super::{LocalIdentity, Mailspace, MailspaceConfig, RoleUpdate};
use crate::error::VivariumError;

const FLOW_ROLES: [&str; 3] = [RESEARCHER_ROLE, ADMITTER_ROLE, SLICER_ROLE];

impl Mailspace {
    /// Ensure the `researcher`, `admitter`, `slicer`, `repairer` and `head-cto`
    /// roles and their shipped charters exist.
    ///
    /// A new role gets kind equal to its name, `read_only: true`, and the pool
    /// of the pump's hand role, else the first local pool. The `repairer`
    /// differs: it is not read-only (it publishes `vivi/repair/<segment>`) and
    /// it gets model band `P0`, the top tier. A role that already exists is
    /// left as it is. The shipped charter is written only when the role has no
    /// charter text. Names and aliases match without regard to case.
    ///
    /// # Errors
    /// Returns an error if a flow role name is already another role's alias,
    /// or config or charter persistence fails.
    pub fn ensure_flow_roles(&mut self) -> Result<(), VivariumError> {
        for name in FLOW_ROLES {
            self.ensure_flow_role(name)?;
        }
        self.ensure_flow_role(HEAD_CTO_ROLE)?;
        self.ensure_repairer_role()
    }

    fn ensure_repairer_role(&mut self) -> Result<(), VivariumError> {
        let update = RoleUpdate {
            kind: Some(Some(REPAIRER_ROLE.to_string())),
            read_only: Some(Some(false)),
            model: Some(Some(String::from("P0"))),
            pool: flow_pool(&self.config).map(Some),
            ..RoleUpdate::default()
        };
        self.ensure_role_with(REPAIRER_ROLE, update)
    }

    fn ensure_flow_role(&mut self, name: &str) -> Result<(), VivariumError> {
        let update = RoleUpdate {
            kind: Some(Some(name.to_string())),
            read_only: Some(Some(true)),
            pool: flow_pool(&self.config).map(Some),
            ..RoleUpdate::default()
        };
        self.ensure_role_with(name, update)
    }

    fn ensure_role_with(&mut self, name: &str, update: RoleUpdate) -> Result<(), VivariumError> {
        let existing = find_role(&self.config, name)
            .map(|role| (role.name.clone(), role.name.eq_ignore_ascii_case(name)));
        let canonical = match existing {
            Some((owner, false)) => {
                return Err(VivariumError::Message(format!(
                    "cannot provision predefined role '{name}': that name is an alias for '{owner}'"
                )));
            }
            Some((owner, true)) => owner,
            None => {
                self.add_role(name, update)?;
                name.to_string()
            }
        };
        if self.read_charter(&canonical)?.trim().is_empty()
            && let Some(charter) = shipped_charter(name)
        {
            self.set_charter(&canonical, charter)?;
        }
        Ok(())
    }
}

/// The role a name or alias denotes, ignoring case.
fn find_role<'a>(config: &'a MailspaceConfig, name: &str) -> Option<&'a LocalIdentity> {
    config.identities.iter().find(|role| {
        role.name.eq_ignore_ascii_case(name)
            || role
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(name))
    })
}

/// The pool of the pump's hand role, else the first local pool.
fn flow_pool(config: &MailspaceConfig) -> Option<String> {
    let hand_pool = config
        .pump
        .as_ref()
        .and_then(|pump| find_role(config, &pump.to))
        .and_then(|role| role.pool.clone());
    hand_pool.or_else(|| {
        config
            .pools
            .iter()
            .find(|(_, pool)| pool.provider.eq_ignore_ascii_case("local"))
            .map(|(name, _)| name.clone())
    })
}

#[cfg(test)]
#[path = "shipped_roles_test.rs"]
mod tests;
