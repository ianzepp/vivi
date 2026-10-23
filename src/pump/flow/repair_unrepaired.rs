//! The unrepaired waiting node: after two failed attempts the run waits here,
//! main stays red, and the one alarm tells a person where things stand.

use crate::error::VivariumError;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::StepRow;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::repair;
use crate::pump::flow::tick::raise_once;
use crate::pump::jobs::JobStore;

/// The exception raised once when a repair run reaches `unrepaired`.
pub const MAIN_UNREPAIRED: &str = "flow-main-unrepaired";

/// The node id this handler serves.
const NODE: &str = "unrepaired";

/// Holds the run; `poll` raises the alarm and never answers (release is
/// `vivi pump unlock` or a green main rung).
struct Unrepaired;

impl StepHandler for Unrepaired {
    fn start(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<StartAction, VivariumError> {
        Ok(StartAction::OpenAndWait)
    }

    fn poll(&self, cx: &StartCx<'_>, _step: &StepRow) -> Result<Option<String>, VivariumError> {
        let unit = format!("{} r{}", cx.run.need, cx.run.run);
        raise_once(cx.ctx, MAIN_UNREPAIRED, &unit, &alarm_text(cx));
        Ok(None)
    }
}

/// The alarm text: need, run, target, tested tip, both attempt branches. A
/// record that cannot be read leaves out what it would have said.
fn alarm_text(cx: &StartCx<'_>) -> String {
    let need = &cx.run.need;
    let mut text = format!("repair need {need} r{} is unrepaired", cx.run.run);
    match cx.mail.open_record(need) {
        Ok(Some((_, body))) => match repair::record(&body) {
            Ok(rec) => text.push_str(&format!(
                "; target {}, tested tip {}",
                rec.target,
                rec.tested.as_deref().unwrap_or("unknown")
            )),
            Err(error) => text.push_str(&format!("; record unreadable: {error}")),
        },
        Ok(None) => text.push_str("; no open record"),
        Err(error) => text.push_str(&format!("; record unreadable: {error}")),
    }
    text.push_str(&format!(
        "; attempt branches {} and {}; main stays red; \
         release with vivi pump unlock {need} or a green main rung",
        repair::branch(need, cx.run.run, 1),
        repair::branch(need, cx.run.run, 2)
    ));
    text
}

/// Registers this module's handlers.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.node_handler(FlowKind::Repair, NODE, Box::new(Unrepaired));
}

#[cfg(test)]
#[path = "repair_unrepaired_test.rs"]
mod tests;
