//! Bound units of needs, read from one load of the backlog graph.
//!
//! A board prints a run count for every open need, and each count needs the
//! need's bound units. Loading the whole backlog graph once per need made the
//! board quadratic in the number of needs (5.3 s at 2,000 needs). The units
//! of every need asked about now come from one [`BacklogIndex`].

use std::collections::HashMap;

use super::super::backlog_index::BacklogIndex;
use super::{BacklogBoundUnit, Mailspace};
use vivi_error::VivariumError;

impl Mailspace {
    /// Units bound to the need named by `handle`, with their node states.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure.
    pub fn backlog_bound_units(
        &self,
        handle: &str,
    ) -> Result<Vec<BacklogBoundUnit>, VivariumError> {
        let handles = [handle.to_string()];
        Ok(self
            .backlog_bound_units_many(&handles)?
            .remove(handle)
            .unwrap_or_default())
    }

    /// Units bound to each need in `handles`, keyed by the handle as given,
    /// from one read of the backlog graph. A handle with no backlog node has
    /// no entry. Each answer equals [`Mailspace::backlog_bound_units`] for
    /// that handle.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure.
    pub fn backlog_bound_units_many(
        &self,
        handles: &[String],
    ) -> Result<HashMap<String, Vec<BacklogBoundUnit>>, VivariumError> {
        let storage = self.storage()?;
        let index = BacklogIndex::load(&storage)?;
        let mut out = HashMap::new();
        for handle in handles {
            let Some(parent) = index.node_by_token(&storage, handle)? else {
                continue;
            };
            let units = index
                .subgraph_nodes(&parent.source_id)
                .into_iter()
                .map(|unit| BacklogBoundUnit {
                    handle: unit.source_id.clone(),
                    state: unit.state.clone(),
                })
                .collect();
            out.insert(handle.clone(), units);
        }
        Ok(out)
    }
}
