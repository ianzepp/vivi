//! Advancing a flow run: routing, terminals and the completion guard.

use crate::error::VivariumError;
use crate::pump::flow::def::FlowDef;
use crate::pump::flow::mail::{MailReader, MailWriter};
use crate::pump::flow::outcome::Outcome;
use crate::pump::host::Ctx;
use crate::pump::store::PumpStore;

pub(crate) mod guard;
pub(crate) mod route;
pub(crate) mod start;
pub(crate) mod terminal;

pub use terminal::withdraw;

/// What one advance did: the routing result and each terminal handled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Advanced {
    pub routed: route::Routed,
    pub finished: Vec<terminal::Finished>,
}

/// Route a finished step, then handle every terminal routing reached.
///
/// # Errors
/// Returns a [`VivariumError`] from routing or from a terminal.
pub(crate) fn advance<M: MailWriter + MailReader>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    def: &FlowDef,
    outcome: &Outcome,
    decide: route::Decide<'_>,
) -> Result<Advanced, VivariumError> {
    let routed = route::route(ctx, store, mail, def, outcome, decide)?;
    let mut finished = Vec::new();
    for terminal in &routed.terminal {
        finished.push(terminal::finish(ctx, store, mail, def, outcome, terminal)?);
    }
    Ok(Advanced { routed, finished })
}
