//! The unrepaired waiting node: after two failed attempts the run waits here,
//! main stays red, and the one alarm tells a person where things stand.

use crate::error::VivariumError;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::main_rung;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::repair;
use crate::pump::flow::tick::raise_once;
use crate::pump::flow::verify;
use crate::pump::jobs::JobStore;

/// The exception raised once when a repair run reaches `unrepaired`.
pub const MAIN_UNREPAIRED: &str = "flow-main-unrepaired";

/// The node id this handler serves.
const NODE: &str = "unrepaired";

/// Holds the run; `poll` answers `green` when main is green at a tip that
/// contains the tested tip, and otherwise raises the alarm and never answers
/// (release is `vivi pump unlock`).
struct Unrepaired;

impl StepHandler for Unrepaired {
    fn start(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<StartAction, VivariumError> {
        Ok(StartAction::OpenAndWait)
    }

    fn poll(&self, cx: &StartCx<'_>, _step: &StepRow) -> Result<Option<String>, VivariumError> {
        let standing = Standing::read(cx)?;
        if standing.main_is_green {
            return Ok(Some(String::from("green")));
        }
        let unit = format!("{} r{}", cx.run.need, cx.run.run);
        raise_once(cx.ctx, MAIN_UNREPAIRED, &unit, &alarm_text(cx, &standing));
        Ok(None)
    }
}

/// The target and tested tip of the open record, or why they are unknown.
type RecordRead = Result<(String, Option<String>), String>;

/// Where the target stands: what the record says, what main is, and whether
/// main is already green at a tip that contains the tested tip.
struct Standing {
    /// The target and tested tip, or why the record could not be read.
    record: RecordRead,
    /// The current main tip, or why it could not be read.
    main_tip: Result<Option<String>, String>,
    /// The recorded last green tip of the target.
    last_green: Option<String>,
    /// Why the containment check failed, when it did.
    contains_error: Option<String>,
    /// Last green is the current main tip and main contains the tested tip.
    main_is_green: bool,
}

impl Standing {
    fn read(cx: &StartCx<'_>) -> Result<Self, VivariumError> {
        let record = read_record(cx);
        let mut standing = Self {
            record: record.clone(),
            main_tip: Ok(None),
            last_green: None,
            contains_error: None,
            main_is_green: false,
        };
        let Ok((target, tested)) = record else {
            return Ok(standing);
        };
        standing.last_green = cx
            .store
            .main_state(&main_rung::key(main_rung::GREEN, &target))?;
        let Some(tctx) = cx.ctx.for_target(&target) else {
            standing.main_tip = Err(format!("unknown target {target}"));
            return Ok(standing);
        };
        standing.main_tip = verify::remote_tip(&tctx, &tctx.cfg.remote, &tctx.cfg.main);
        let (Ok(Some(main)), Some(green), Some(tested)) =
            (&standing.main_tip, &standing.last_green, &tested)
        else {
            return Ok(standing);
        };
        if main != green {
            return Ok(standing);
        }
        match verify::ref_contains(cx.ctx, &target, &tctx.cfg.main, tested) {
            Ok(found) => standing.main_is_green = found.is_some(),
            Err(error) => standing.contains_error = Some(error),
        }
        Ok(standing)
    }
}

/// The target and tested tip of the open record, or why they are unknown.
fn read_record(cx: &StartCx<'_>) -> RecordRead {
    match cx.mail.open_record(&cx.run.need) {
        Ok(Some((_, body))) => match repair::record(&body) {
            Ok(rec) => Ok((rec.target, rec.tested)),
            Err(error) => Err(format!("record unreadable: {error}")),
        },
        Ok(None) => Err(String::from("no open record")),
        Err(error) => Err(format!("record unreadable: {error}")),
    }
}

/// The alarm text: need, run, target, expected and actual main, and what each
/// attempt ended with. A record that cannot be read leaves out what it would
/// have said.
fn alarm_text(cx: &StartCx<'_>, standing: &Standing) -> String {
    let need = &cx.run.need;
    let mut text = format!("repair need {need} r{} is unrepaired", cx.run.run);
    let tested = match &standing.record {
        Ok((target, tested)) => {
            let tested = tested.as_deref().unwrap_or("unknown");
            text.push_str(&format!(
                "; target {target}; expected main green at a tip containing tested {tested}"
            ));
            tested.to_owned()
        }
        Err(error) => {
            text.push_str(&format!("; {error}"));
            String::from("unknown")
        }
    };
    let main = match &standing.main_tip {
        Ok(Some(tip)) => tip.clone(),
        Ok(None) => String::from("unknown"),
        Err(error) => format!("unknown ({error})"),
    };
    text.push_str(&format!(
        "; actual: main tip {main}, last green {}",
        standing.last_green.as_deref().unwrap_or("none")
    ));
    if let Some(error) = &standing.contains_error {
        text.push_str(&format!(" (containment unchecked: {error})"));
    }
    for attempt in 1..=2 {
        text.push_str(&format!(
            "; attempt {attempt}: {}",
            attempt_text(cx, attempt)
        ));
    }
    text.push_str(&format!(
        "; main stays red at {tested}; release with vivi pump unlock {need} or a green main rung"
    ));
    text
}

/// What one attempt ended with: its last finished word and what it published.
fn attempt_text(cx: &StartCx<'_>, attempt: u32) -> String {
    let (need, run) = (&cx.run.need, cx.run.run);
    let steps = match cx.store.steps_of(need, run) {
        Ok(steps) => steps,
        Err(error) => return format!("steps unreadable: {error}"),
    };
    let nodes = [
        repair::fix_node(attempt),
        repair::rung_node(attempt),
        format!("merge{attempt}"),
    ];
    let mut word = None;
    for node in &nodes {
        let done = steps
            .iter()
            .rev()
            .find(|row| &row.node == node && row.state == StepState::Done);
        if let Some(verdict) = done.and_then(|row| row.verdict.clone()) {
            word = Some(verdict);
        }
    }
    let word = word.unwrap_or_else(|| String::from("no result"));
    let facts = repair::latest_facts(
        cx.store,
        cx.mail,
        &cx.ctx.cfg.reply_from,
        need,
        run,
        attempt,
    );
    let published = match facts {
        Ok(Some(facts)) => format!(" published {} {}", facts.branch, facts.commit),
        Ok(None) => format!(" published nothing{}", nothing_reason(cx, &steps, attempt)),
        Err(error) => format!(" published unknown (facts unreadable: {error})"),
    };
    format!("{word}{published}")
}

/// ` (reason)` from the fix step's payload, or nothing.
fn nothing_reason(cx: &StartCx<'_>, steps: &[StepRow], attempt: u32) -> String {
    let node = repair::fix_node(attempt);
    let Some(fix) = steps.iter().rev().find(|row| row.node == node) else {
        return String::new();
    };
    match cx.store.step_payload(&fix.task) {
        Ok(Some(row)) => row
            .payload
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .map(|reason| format!(" ({reason})"))
            .unwrap_or_default(),
        Ok(None) => String::new(),
        Err(error) => format!(" (payload unreadable: {error})"),
    }
}

/// Registers this module's handlers.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.node_handler(FlowKind::Repair, NODE, Box::new(Unrepaired));
}

#[cfg(test)]
#[path = "repair_unrepaired_test.rs"]
mod tests;
