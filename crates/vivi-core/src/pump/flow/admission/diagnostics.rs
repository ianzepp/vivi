//! Build prerequisite diagnostics and wait projection formatting.
use crate::pump::flow::admission::{Admission, AdmissionState};
use crate::pump::flow::milestone::prereq::Prereq;
use crate::pump::flow::milestone::prereq::PrereqState;
use crate::pump::flow::tick::raise_once;
use crate::pump::host::Ctx;
use crate::pump::store::{FlowWaitRow, PumpStore, WaitingHandle};

pub(crate) fn view(prereq: &Prereq) -> WaitingHandle {
    let (state, detail) = match &prereq.state {
        PrereqState::Open => ("open", String::new()),
        PrereqState::Held(reason) => ("held", reason.clone()),
        PrereqState::ClosedUnlanded(reason) => ("unlanded", reason.clone()),
        PrereqState::Landed => ("landed", String::new()),
    };
    WaitingHandle {
        handle: prereq.handle.clone(),
        state: state.into(),
        detail,
    }
}

pub(crate) fn report(
    ctx: &Ctx,
    store: &PumpStore,
    record: &str,
    need: &str,
    admission: &Admission,
) {
    for prereq in &admission.details {
        let (kind, state) = match &prereq.state {
            PrereqState::Open => continue,
            PrereqState::Held(state) => ("flow-build-prereq-held", state.as_str()),
            PrereqState::ClosedUnlanded(state) => ("flow-build-prereq-unlanded", state.as_str()),
            PrereqState::Landed => continue,
        };
        let unit = format!("{record}:{}:{kind}", prereq.handle);
        let text = format!(
            "build dispatch {record} for {need}: prerequisite {} is {state}",
            prereq.handle
        );
        report_once(ctx, store, kind, &unit, &text);
    }
}

pub(crate) fn unreadable(ctx: &Ctx, store: &PumpStore, record: &str, need: &str, error: &str) {
    let kind = "flow-build-prereq-unreadable";
    let unit = format!("{record}:unknown:{kind}");
    report_once(
        ctx,
        store,
        kind,
        &unit,
        &format!("build dispatch {record} for {need}: prerequisite classification failed: {error}"),
    );
}

fn report_once(ctx: &Ctx, store: &PumpStore, kind: &str, unit: &str, text: &str) {
    if store
        .exceptions()
        .is_ok_and(|rows| rows.iter().any(|(_, k, u, _)| k == kind && u == unit))
    {
        return;
    }
    raise_once(ctx, kind, unit, text);
}

#[allow(dead_code)]
fn _projection_type(_: &FlowWaitRow, _: &AdmissionState) {}
