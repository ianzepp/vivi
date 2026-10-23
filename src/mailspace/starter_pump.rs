//! The starter `[pump]` table a new mailspace is created with.

use std::path::Path;

use super::config_file::{parse_config, read_raw};
use super::{MAILSPACE_CONFIG, MAILSPACE_DIR, Mailspace, config_edit, project_root};
use crate::error::VivariumError;

/// What `vivi mailspace init` appends to a new `mailspace.toml`. Every value
/// the pump needs and cannot guess is left empty; until each is filled in the
/// project is not ready and the pump holds (`vivi config validate` lists
/// what is left). Every other pump setting keeps its default.
pub const STARTER_PUMP: &str = r#"
# Starter pump configuration. The pump holds ("not ready") and dispatches
# nothing until the items below are filled in and the roles it uses each have
# a pool (`vivi pool --help`, then `vivi role pool <role> <pool>`).
# `vivi config validate` lists what is still missing.
[pump]
# Git author and committer of every commit the pump makes.
committer_name = ""
committer_email = ""
"#;

impl Mailspace {
    /// Whether a mailspace config already exists at `project` (or the
    /// current directory).
    ///
    /// # Errors
    /// Returns an error if the current directory cannot be read.
    pub fn config_exists(project: Option<&Path>) -> Result<bool, VivariumError> {
        Ok(project_root(project)?
            .join(MAILSPACE_DIR)
            .join(MAILSPACE_CONFIG)
            .is_file())
    }

    /// Create the mailspace at `project` the way `vivi mailspace init` does:
    /// the config and store, the operator, merge, planner and flow (researcher, admitter, slicer) roles, and, for a
    /// mailspace that did not exist yet, the [`STARTER_PUMP`] table. Returns
    /// the mailspace and whether this call created it.
    ///
    /// # Errors
    /// Returns an error if the mailspace cannot be created or a role or the
    /// starter table cannot be written.
    pub fn init_with_defaults(project: Option<&Path>) -> Result<(Self, bool), VivariumError> {
        let existed = Self::config_exists(project)?;
        let mut mailspace = Self::init(project)?;
        mailspace.ensure_operator_role()?;
        mailspace.ensure_merge_role()?;
        mailspace.ensure_pump_planner_role()?;
        mailspace.ensure_flow_roles()?;
        if !existed {
            mailspace.ensure_starter_pump()?;
        }
        Ok((mailspace, !existed))
    }

    /// Append the [`STARTER_PUMP`] table when the config has no `[pump]`
    /// table. Idempotent; the rest of the file keeps its text and comments.
    ///
    /// # Errors
    /// Returns an error if the config cannot be read, parsed or written.
    pub fn ensure_starter_pump(&mut self) -> Result<(), VivariumError> {
        if self.config.pump.is_some() {
            return Ok(());
        }
        let path = self.config_path();
        config_edit::apply(&path, |doc| {
            if doc.contains_key("pump") {
                return Ok(());
            }
            let text = format!("{doc}{STARTER_PUMP}");
            *doc = text.parse().map_err(|error| {
                VivariumError::Config(format!("cannot add the starter [pump] table: {error}"))
            })?;
            Ok(())
        })?;
        self.config.pump = parse_config(&path, &read_raw(&path)?)?.pump;
        Ok(())
    }
}

#[cfg(test)]
#[path = "starter_pump_test.rs"]
mod tests;
