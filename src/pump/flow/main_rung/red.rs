//! Files or replies on the repair need when the main rung is red.
//!
//! One repair-kind need per target: a first red files it, a repeat red with
//! the same failing id set does nothing, and a different set replies on the
//! open need. Sets are compared as ids (`RungResult.failing`), never as text.

use std::collections::BTreeSet;
use std::fs;

use super::{FAILING, key, record_target};
use crate::error::VivariumError;
use crate::mailspace::marked_line;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::handlers::FlowMail;
use crate::pump::flow::kind;
use crate::pump::flow::mail::{MailReader, MailWriter, NeedStatus};
use crate::pump::flow::tick::raise_once;
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::store::PumpStore;

/// Marker of the reply naming a later red main rung on an open repair need.
pub(crate) const RED_MARKER: &str = "pump-main-red:";

/// Main-state key (per target): the tip whose repair need is being sent.
/// Written before `need send`; cleared once the need's record is open.
const SENDING: &str = "main-repair-sending";

/// Main-state key (per target): the handle of the repair need this module
/// filed, written right after the handle is parsed, before the record opens.
const FILED: &str = "main-repair-need";

/// Alarm: a repair need may have been sent but its handle is unknown.
pub(crate) const REPAIR_NEED_UNKNOWN: &str = "flow-repair-need-unknown";

/// The open repair need for a target: the first need whose open record reads
/// as a repair run and names the target.
///
/// # Errors
/// Returns an error when the open records or a need's marks cannot be read.
pub(crate) fn find_repair_need<M: FlowMail>(
    mail: &M,
    target: &str,
) -> Result<Option<String>, VivariumError> {
    for (need, _date) in mail.open_record_needs()? {
        let Some((_handle, body)) = MailReader::open_record(mail, &need)? else {
            continue;
        };
        let (flow_kind, _) = kind::of(Some(&body), &mail.need_marks(&need)?);
        if flow_kind == FlowKind::Repair && record_target(&body) == Some(target) {
            return Ok(Some(need));
        }
    }
    Ok(None)
}

/// A main rung went red on `tip`: file or reply on the repair need.
///
/// # Errors
/// Returns an error when the state, the mailspace, or the `need send` fails.
pub(crate) fn report_red<M: FlowMail>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    target: &str,
    tip: &str,
    failing: &BTreeSet<String>,
    tail: &str,
) -> Result<(), VivariumError> {
    let state_key = key(FAILING, target);
    let previous: BTreeSet<String> = match store.main_state(&state_key)? {
        Some(json) => serde_json::from_str(&json)
            .map_err(|e| VivariumError::Message(format!("bad {state_key} state: {e}")))?,
        None => BTreeSet::new(),
    };
    let existing = match resume_filed(store, mail, target, tip)? {
        Resumed::Opened => return store_failing(store, &state_key, failing),
        Resumed::Open(need) => Some(need),
        Resumed::None => find_repair_need(&*mail, target)?,
    };
    match existing {
        Some(need) => {
            store.clear_main_state(&key(SENDING, target))?;
            if previous == *failing {
                return Ok(());
            }
            let rest = format!("tested {tip}\n{}", body_tail(failing, tail));
            mail.reply(&need, &marked_line(RED_MARKER, &rest))?;
        }
        None => {
            let Some(need) = file_need(ctx, store, target, tip, failing, tail)? else {
                return Ok(());
            };
            open_filed(store, mail, target, tip, &need)?;
        }
    }
    store_failing(store, &state_key, failing)
}

/// Remember the failing set this target last reported.
fn store_failing(
    store: &PumpStore,
    state_key: &str,
    failing: &BTreeSet<String>,
) -> Result<(), VivariumError> {
    let json = serde_json::to_string(failing)
        .map_err(|e| VivariumError::Message(format!("cannot store the failing set: {e}")))?;
    store.set_main_state(state_key, &json)
}

/// What the recorded filing state says about this target.
enum Resumed {
    /// Nothing recorded, or the recorded need is closed (keys cleared).
    None,
    /// The recorded need is open and has its record.
    Open(String),
    /// The recorded need was open without a record; the record is open now.
    Opened,
}

/// Resume an earlier filing: a recorded handle whose need is still open is
/// this target's repair need (its record is opened when missing); one whose
/// need is closed is forgotten, together with the sending marker.
fn resume_filed<M: FlowMail>(
    store: &PumpStore,
    mail: &mut M,
    target: &str,
    tip: &str,
) -> Result<Resumed, VivariumError> {
    let Some(need) = store.main_state(&key(FILED, target))? else {
        return Ok(Resumed::None);
    };
    if mail.need_state(&need)? != NeedStatus::Open {
        store.clear_main_state(&key(FILED, target))?;
        store.clear_main_state(&key(SENDING, target))?;
        return Ok(Resumed::None);
    }
    if MailReader::open_record(&*mail, &need)?.is_some() {
        return Ok(Resumed::Open(need));
    }
    open_filed(store, mail, target, tip, &need)?;
    Ok(Resumed::Opened)
}

/// Open the pump record on a filed repair need, then drop the sending marker.
fn open_filed<M: FlowMail>(
    store: &PumpStore,
    mail: &mut M,
    target: &str,
    tip: &str,
    need: &str,
) -> Result<(), VivariumError> {
    let record = format!("kind: repair\ntarget: {target}\ntested: {tip}\n");
    MailWriter::open_record(mail, need, &record)?;
    store.clear_main_state(&key(SENDING, target))
}

/// Raise the unknown-handle alarm: the send may have landed.
fn alarm_unknown(ctx: &Ctx, target: &str, tip: &str, detail: &str) {
    raise_once(
        ctx,
        REPAIR_NEED_UNKNOWN,
        target,
        &format!(
            "repair need for target {target} at {tip} may have been sent but its handle is unknown ({detail}); no second need is filed: find the need and open a repair record on it that names the target"
        ),
    );
}

/// The failing ids, one per line, then the tail.
fn body_tail(failing: &BTreeSet<String>, tail: &str) -> String {
    let mut text = String::from("failing:\n");
    for id in failing {
        text.push_str(&format!("{id}\n"));
    }
    text.push_str(&format!("\n{tail}\n"));
    text
}

/// Send the repair need (marked `repair`) and return its handle. The sending
/// marker is written first and the handle is recorded as soon as it parses.
/// `None` means the send may have landed with an unknown handle: the alarm is
/// raised and nothing is sent again.
fn file_need(
    ctx: &Ctx,
    store: &PumpStore,
    target: &str,
    tip: &str,
    failing: &BTreeSet<String>,
    tail: &str,
) -> Result<Option<String>, VivariumError> {
    if let Some(sent_tip) = store.main_state(&key(SENDING, target))? {
        alarm_unknown(ctx, target, &sent_tip, "an earlier send left no handle");
        return Ok(None);
    }
    let short: String = tip.chars().take(8).collect();
    let safe: String = target
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let dir = ctx.home.join("repair");
    fs::create_dir_all(&dir)
        .map_err(|e| VivariumError::Message(format!("cannot create {}: {e}", dir.display())))?;
    let path = dir.join(format!("repair-{safe}-{short}.txt"));
    let body = format!(
        "target: {target}\ntested: {tip}\n\n{}",
        body_tail(failing, tail)
    );
    fs::write(&path, body)
        .map_err(|e| VivariumError::Message(format!("cannot write {}: {e}", path.display())))?;
    let subject = format!("pump: main rung red on {target} at {short}");
    let args = format!(
        "need send --from {} --to {} --subject {} --body-file {} --mark repair",
        ctx.cfg.reply_from,
        ctx.cfg.reply_to(),
        shell_quote(&subject),
        shell_quote(&path.display().to_string()),
    );
    store.set_main_state(&key(SENDING, target), tip)?;
    let out = ctx.vivi(&args, 60);
    let last = out.text.lines().last().unwrap_or("").trim();
    if out.code == 124 {
        alarm_unknown(ctx, target, tip, &format!("the send timed out: {last}"));
        return Ok(None);
    }
    if out.code != 0 {
        store.clear_main_state(&key(SENDING, target))?;
        return Err(VivariumError::Message(format!(
            "repair need send failed: {last}"
        )));
    }
    let Some(need) = need_handle(&out.text) else {
        alarm_unknown(ctx, target, tip, &format!("unparsed send output: {last}"));
        return Ok(None);
    };
    store.set_main_state(&key(FILED, target), &need)?;
    Ok(Some(need))
}

/// The handle from `need send` output: the third token of the first
/// `created ` line, else the second token of the first `sent ` line.
fn need_handle(text: &str) -> Option<String> {
    let created = text
        .lines()
        .find(|line| line.starts_with("created "))
        .and_then(|line| line.split_whitespace().nth(2));
    created
        .or_else(|| {
            text.lines()
                .find(|line| line.starts_with("sent "))
                .and_then(|line| line.split_whitespace().nth(1))
        })
        .map(str::to_string)
}

#[cfg(test)]
#[path = "red_test.rs"]
mod tests;
