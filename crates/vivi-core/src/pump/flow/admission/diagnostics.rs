//! Build prerequisite blocker diagnostics.
use crate::pump::flow::admission::{Admission, AdmissionState};
use crate::pump::flow::milestone::prereq::PrereqState;
use crate::pump::flow::tick::raise_once;
use crate::pump::host::Ctx;
use crate::pump::store::PumpStore;

const HELD: &str = "flow-build-prereq-held";
const UNLANDED: &str = "flow-build-prereq-unlanded";
const UNREADABLE: &str = "flow-build-prereq-unreadable";

/// Raise stable, persisted-deduplicated diagnostics for every blocking dependency.
pub(crate) fn report(
    ctx: &Ctx,
    store: &PumpStore,
    record: &str,
    need: &str,
    admission: &Admission,
) {
    match &admission.state {
        AdmissionState::Unreadable(error) => raise(
            ctx,
            store,
            UNREADABLE,
            &format!("{record}:{need}:graph"),
            &format!("build {need} record {record}: prerequisite graph unreadable: {error}"),
        ),
        AdmissionState::Blocked | AdmissionState::Waiting => {
            for prereq in &admission.details {
                let (kind, state) = match &prereq.state {
                    PrereqState::Held(detail) => (HELD, format!("held ({detail})")),
                    PrereqState::ClosedUnlanded(outcome) => {
                        (UNLANDED, format!("closed unlanded ({outcome})"))
                    }
                    PrereqState::Open => continue,
                    PrereqState::Landed => continue,
                };
                let unit = format!("{record}:{}:{kind}", prereq.handle);
                raise(
                    ctx,
                    store,
                    kind,
                    &unit,
                    &format!(
                        "build {need} record {record}: dependency {} is {state}",
                        prereq.handle
                    ),
                );
            }
        }
        AdmissionState::Ready => {}
    }
}

fn raise(ctx: &Ctx, store: &PumpStore, kind: &str, unit: &str, text: &str) {
    let already = store
        .exceptions()
        .map(|rows| rows.iter().any(|(_, k, u, _)| k == kind && u == unit))
        .unwrap_or(false);
    if !already {
        raise_once(ctx, kind, unit, text);
    }
}

#[cfg(test)]
#[path = "diagnostics_test.rs"]
mod tests;
