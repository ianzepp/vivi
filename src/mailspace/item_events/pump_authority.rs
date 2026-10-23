//! The pump identity's word on a need it is running (operator ruling Q7).
//!
//! The pump identity (`[pump] reply_from`, the identity the flow engine writes
//! as and the one pump records are addressed to) may close and reopen a need while the
//! need has an open pump record here, whoever authored the need and whatever
//! its delegation history. Pump records do not sync, so another member
//! cannot see that record: it accepts the pump's letter only from a sender
//! whose role is the pump identity and whose address already posted on the
//! need's thread.

use std::collections::HashSet;

use super::{HeldItem, Mailspace, parse_event, parse_property_event};
use crate::mailspace::PumpState;
use crate::mailspace::backlog::canonical_content_handle;

impl Mailspace {
    /// True when `identity` is this mailspace's pump identity and `item` is a
    /// need with an open pump record here. Writing a close or reopen needs
    /// this.
    pub(in crate::mailspace) fn pump_controls(&self, item: &HeldItem, identity: &str) -> bool {
        self.pump_record_state(item, identity, true)
    }

    /// True when `identity` is this mailspace's pump identity and `item` is a
    /// need with ANY pump record here, open or done. Reading a pump letter
    /// judges it as of when it was written, so it keeps counting after the
    /// record closes; it never lets the pump write a new close.
    pub(in crate::mailspace) fn pump_spoke_for(&self, item: &HeldItem, identity: &str) -> bool {
        self.pump_record_state(item, identity, false)
    }

    fn pump_record_state(&self, item: &HeldItem, identity: &str, open_only: bool) -> bool {
        if item.kind != "need" || !identity.eq_ignore_ascii_case(&self.pump_names().1) {
            return false;
        }
        let Some(row) = item.rows.first() else {
            return false;
        };
        let Ok(storage) = self.storage() else {
            return false;
        };
        let Ok(handle) = canonical_content_handle(&storage, &row.message_id) else {
            return false;
        };
        let state = if open_only {
            Some(PumpState::Open)
        } else {
            None
        };
        self.pump_records(&storage, state)
            .is_ok_and(|records| records.iter().any(|record| record.need == handle))
    }

    /// True when `from_addr`, a sender in another member's mailspace, is the
    /// pump identity by role and has already posted on the need's thread.
    /// The letter being judged (`letter_id`) and event letters do not count
    /// as that earlier post.
    pub(in crate::mailspace) fn remote_pump_speaks(
        &self,
        item: &HeldItem,
        from_addr: &str,
        letter_id: &str,
    ) -> bool {
        let Some((role, _)) = from_addr.rsplit_once('@') else {
            return false;
        };
        if item.kind != "need" || !role.trim().eq_ignore_ascii_case(&self.pump_names().1) {
            return false;
        }
        let Ok(storage) = self.storage() else {
            return false;
        };
        let mut seen = HashSet::new();
        let mut queue = vec![item.content_id.clone()];
        while let Some(parent) = queue.pop() {
            let Ok(children) = storage.reply_child_contents(&parent) else {
                return false;
            };
            for child in children {
                if !seen.insert(child.clone()) {
                    continue;
                }
                queue.push(child.clone());
                let Ok(Some(view)) = storage.message_by_content_id(&child) else {
                    continue;
                };
                if view.normalized_message_id.as_deref() == Some(letter_id)
                    || !view.from_addr.eq_ignore_ascii_case(from_addr)
                {
                    continue;
                }
                let Ok(bytes) = storage.read_listed_blob(&view.blob_relpath) else {
                    continue;
                };
                if parse_event(&bytes).is_none() && parse_property_event(&bytes).is_none() {
                    return true;
                }
            }
        }
        false
    }
}

#[cfg(test)]
#[path = "pump_authority_test.rs"]
mod tests;
