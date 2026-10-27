//! Durable evidence for mind decision letters.
//!
//! A mind decision send marks every copy of its letter with one transport
//! header, [`DECISION_MARKER_HEADER`], whose value [`decision_marker`]
//! derives from the decision's stable canonical key. The derivation is
//! deterministic, so the copies one send left behind can be found again on
//! any later tick or restart — including a crash between the ingest steps.
//! [`evidence`] holds that discovery: from durable mail state alone it
//! reports whether the mind recipient's copy, the sender's sent copy, the
//! step-parent link, and the required send events are all present. A later
//! decision send path decides from that evidence whether a decision still
//! needs a letter; ordinary [`Mailspace::send`](crate::mailspace::Mailspace::send)
//! is unchanged.

// Foundation for the decision-specific send that lands next: until that path
// calls in, only the companion tests read these items.
#![cfg_attr(not(test), allow(dead_code))]
use sha2::{Digest, Sha256};

use crate::error::VivariumError;

mod evidence;

#[cfg_attr(not(test), allow(unused_imports))]
pub(crate) use evidence::{DecisionCopy, DecisionEvents, DecisionEvidence, find_decision_evidence};

/// The transport header that marks every copy of a mind decision letter.
pub(crate) const DECISION_MARKER_HEADER: &str = "X-Vivi-Mind-Decision";

/// The marker for a decision's stable canonical key: the SHA-256 of the
/// trimmed key bytes, hex-encoded. The same key always yields the same
/// marker — every copy of one decision letter carries one identical value,
/// so evidence matching is exact — and a fixed-length hex value never folds,
/// wraps, or leaks the key itself into a transport header.
///
/// # Errors
/// Returns an error when the key is empty or only whitespace: a marker
/// without a decision behind it would match nothing and mean nothing.
pub(crate) fn decision_marker(decision_key: &str) -> Result<String, VivariumError> {
    let key = decision_key.trim();
    if key.is_empty() {
        return Err(VivariumError::Message(
            "a mind decision marker needs a non-empty decision key".into(),
        ));
    }
    Ok(hex::encode(Sha256::digest(key.as_bytes())))
}
