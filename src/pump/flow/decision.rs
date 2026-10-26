//! The pump's decision seat: the configured role flow decisions route to.
//!
//! `pump.decision_to` names the seat (by role name, alias, or this
//! mailspace's address for it) that judges, raises, or steers when a flow
//! reaches its decision point. The seat is resolved read-only, when a
//! decision is made: resolution never creates or amends a role and never
//! falls back to another recipient.

use crate::error::VivariumError;
use crate::mailspace::Mailspace;
use crate::pump::config::PumpConfig;

/// The decision seat: the canonical role name flow decisions route to.
///
/// Resolves `pump.decision_to` against the mailspace config and requires a
/// configured role of kind `mind`. A missing, blank, unknown, or wrong-kind
/// seat fails: there is no fallback to the operator, `pump.from`, the need
/// author, or `board.reply_to`. Read-only: no role is created and no config
/// is written, so an unsettled seat never blocks unrelated flow work.
///
/// # Errors
/// Returns [`VivariumError`] when the configured seat is blank, names no
/// known role, or names a role whose kind is not `mind`.
pub fn recipient(mailspace: &Mailspace, pump: &PumpConfig) -> Result<String, VivariumError> {
    let seat = pump.decision_to.trim();
    if seat.is_empty() {
        return Err(VivariumError::Message(String::from(
            "pump.decision_to is blank; set it to a role of kind `mind`",
        )));
    }
    let name = mailspace.resolve_identity(seat)?;
    let role = mailspace
        .config
        .role_named(&name)
        .ok_or_else(|| VivariumError::Message(format!("unknown local role '{name}'")))?;
    let kind = role.kind.as_deref().unwrap_or_default();
    if !kind.eq_ignore_ascii_case("mind") {
        let shown = if kind.is_empty() { "none" } else { kind };
        return Err(VivariumError::Message(format!(
            "pump.decision_to '{seat}' is role '{name}' of kind `{shown}`, not `mind`; \
             set it to a role of kind `mind`"
        )));
    }
    Ok(name)
}

#[cfg(test)]
#[path = "decision_test.rs"]
mod tests;
