//! Wants on the board: a want is listed exactly while its computed state is
//! open, carrying the needs that serve it.

use std::collections::{BTreeSet, HashMap};

use super::BoardServing;
use crate::error::VivariumError;
use crate::mailspace::want_state::{WantState, serving_needs, want_states};
use crate::storage::{Storage, StoredMessageView};

/// Rows of wants that sit in a board account's `done` folder but have an open
/// serving need, shown as `wants` rows. A hand-closed want that a later need
/// serves is open again.
pub(super) fn reopened_rows(
    storage: &Storage,
    account_identity: &HashMap<String, String>,
) -> Result<Vec<StoredMessageView>, VivariumError> {
    let served: BTreeSet<String> = storage
        .item_metadata_values("serves")?
        .into_iter()
        .map(|(_, want_id)| want_id)
        .collect();
    let mut rows = Vec::new();
    for want_id in served {
        if serving_needs(storage, &want_id)?
            .iter()
            .all(|need| need.done)
        {
            continue;
        }
        let Some(want) = storage.live_message_by_rfc_id(&want_id)? else {
            continue;
        };
        for mut row in storage.live_messages_for_content(&want.content_id)? {
            if row.local_role == "done"
                && row.absorbed_at.is_none()
                && account_identity.contains_key(&row.account)
            {
                row.local_role = "wants".into();
                rows.push(row);
            }
        }
    }
    Ok(rows)
}

/// The wants whose computed state is open, with each want's serving needs
/// keyed by the want's board handle.
pub(super) fn open_wants(
    storage: &Storage,
    wants: Vec<StoredMessageView>,
) -> Result<(Vec<StoredMessageView>, HashMap<String, Vec<BoardServing>>), VivariumError> {
    let states = want_states(storage, &wants)?;
    let mut serving = HashMap::new();
    let mut open = Vec::new();
    for want in wants {
        let Some(report) = states.get(&want.content_id) else {
            continue;
        };
        if report.state == WantState::Done {
            continue;
        }
        let needs: Vec<BoardServing> = report
            .serving
            .iter()
            .map(|need| BoardServing {
                handle: need.handle.clone(),
                state: need.state().as_str().into(),
            })
            .collect();
        if !needs.is_empty() {
            serving.insert(want.handle.clone(), needs);
        }
        open.push(want);
    }
    Ok((open, serving))
}
