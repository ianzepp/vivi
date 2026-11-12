//! Trusted post-agent command for one repository target.

use serde::{Deserialize, Serialize};

/// A fixed shell command, copied from dispatcher configuration into the runner.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JobCheck {
    pub command: String,
    /// Independent of the harness budget; the runner reserves this time too.
    #[serde(default = "default_timeout")]
    pub timeout_s: u64,
}

const fn default_timeout() -> u64 {
    600
}

impl JobCheck {
    /// Validate before dispatch, including callers that bypass pump validation.
    ///
    /// # Errors
    /// Returns an error for an empty command, NUL, or an out-of-range timeout.
    pub fn validate(&self) -> Result<(), vivi_error::VivariumError> {
        if self.command.trim().is_empty() || self.command.contains('\0') {
            return Err(vivi_error::VivariumError::Config(
                "check.command must be nonempty and contain no NUL".into(),
            ));
        }
        if !(1..=86400).contains(&self.timeout_s) {
            return Err(vivi_error::VivariumError::Config(
                "check.timeout_s must be 1..=86400 seconds".into(),
            ));
        }
        Ok(())
    }
}
