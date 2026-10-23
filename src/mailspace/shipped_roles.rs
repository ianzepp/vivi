//! Provisioning of the flow roles: researcher, admitter, slicer and head-cto.

use super::shipped::{ADMITTER_ROLE, HEAD_CTO_ROLE, RESEARCHER_ROLE, SLICER_ROLE, shipped_charter};
use super::{Mailspace, MailspaceConfig, RoleUpdate};
use crate::error::VivariumError;

const FLOW_ROLES: [&str; 3] = [RESEARCHER_ROLE, ADMITTER_ROLE, SLICER_ROLE];

impl Mailspace {
    /// Ensure the `researcher`, `admitter`, `slicer` and `head-cto` roles and
    /// their shipped charters exist.
    ///
    /// A new role gets kind equal to its name, `read_only: true`, and the pool
    /// of the pump's hand role, else the first local pool. A role that already
    /// exists is left as it is. The shipped charter is written only when the
    /// role has no charter text.
    ///
    /// # Errors
    /// Returns an error if a flow role name is already another role's alias,
    /// or config or charter persistence fails.
    pub fn ensure_flow_roles(&mut self) -> Result<(), VivariumError> {
        for name in FLOW_ROLES {
            self.ensure_flow_role(name)?;
        }
        self.ensure_flow_role(HEAD_CTO_ROLE)?;
        Ok(())
    }

    fn ensure_flow_role(&mut self, name: &str) -> Result<(), VivariumError> {
        let existing = self
            .config
            .identities
            .iter()
            .find(|role| role.name == name || role.aliases.iter().any(|alias| alias == name));
        match existing {
            Some(role) if role.name != name => {
                return Err(VivariumError::Message(format!(
                    "cannot provision predefined role '{name}': that name is an alias for '{}'",
                    role.name
                )));
            }
            Some(_) => {}
            None => {
                let update = RoleUpdate {
                    kind: Some(Some(name.to_string())),
                    read_only: Some(Some(true)),
                    pool: flow_pool(&self.config).map(Some),
                    ..RoleUpdate::default()
                };
                self.add_role(name, update)?;
            }
        }
        if self.read_charter(name)?.trim().is_empty()
            && let Some(charter) = shipped_charter(name)
        {
            self.set_charter(name, charter)?;
        }
        Ok(())
    }
}

/// The pool of the pump's hand role, else the first local pool.
fn flow_pool(config: &MailspaceConfig) -> Option<String> {
    let hand_pool = config.pump.as_ref().and_then(|pump| {
        config
            .identities
            .iter()
            .find(|role| role.name.eq_ignore_ascii_case(&pump.to))
            .and_then(|role| role.pool.clone())
    });
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
