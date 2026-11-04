//! The steps of the live test on a host whose slots live per dispatch (a
//! rented pod): one pod, one stand-in job, and proof that the pod is gone
//! afterwards. Each step returns `Ok(detail)` or an `Err` that says what was
//! expected and what was observed.

use super::selftest_source::MAIN_BRANCH;
use super::selftest_steps::{StepResult, observed, require_pushed};
use super::selftest_world::World;

/// Budget of the stand-in job, in seconds.
const JOB_BUDGET: u64 = 120;
const BRANCH: &str = "doctor/pod";

/// Provider: rent a pod, seed it and start a job on it.
pub(super) fn dispatch_one(w: &mut World) -> StepResult {
    let made = w
        .dispatch("pod", 0, JOB_BUDGET, (MAIN_BRANCH, BRANCH))
        .map_err(|e| observed("a pod rented and a job started", e))?;
    let hardware = if made.hardware.is_empty() {
        "hardware not reported".to_string()
    } else {
        made.hardware.clone()
    };
    Ok(format!("{} ({hardware})", made.slot))
}

/// Vivi: `wait` collects the job's result from the pod.
pub(super) fn wait_collects(w: &mut World) -> StepResult {
    let fields = require_pushed(w, "pod", "finished")?;
    let sha = fields.get("sha").map_or("", String::as_str);
    if fields.get("branch").map(String::as_str) != Some(BRANCH) || sha.is_empty() {
        return Err(observed(
            &format!("a push of {BRANCH} with a commit"),
            format!("{fields:?}"),
        ));
    }
    Ok(format!("pushed {BRANCH} {}", &sha[..sha.len().min(10)]))
}

/// Provider: after collection the pod is terminated and no longer listed.
pub(super) fn pod_gone(w: &mut World) -> StepResult {
    let record = w.job("pod")?.record.clone();
    let state = w
        .provider
        .machine(&record.machine)
        .map(|m| m.state)
        .map_err(|e| observed("the collected pod gone", e))?;
    let slots = w.slots()?;
    if state != "destroyed" || !slots.is_empty() {
        let saw = format!("state {state}; {}", World::describe(&slots));
        return Err(observed("the pod terminated and absent from the list", saw));
    }
    Ok(format!(
        "pod {} is absent from the provider's list",
        record.machine
    ))
}
