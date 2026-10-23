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
use crate::pump::flow::mail::{MailReader, MailWriter};
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::store::PumpStore;

/// Marker of the reply naming a later red main rung on an open repair need.
pub(crate) const RED_MARKER: &str = "pump-main-red:";

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
    match find_repair_need(&*mail, target)? {
        Some(_) if previous == *failing => return Ok(()),
        Some(need) => {
            let rest = format!("tested {tip}\n{}", body_tail(failing, tail));
            mail.reply(&need, &marked_line(RED_MARKER, &rest))?;
        }
        None => {
            let need = file_need(ctx, target, tip, failing, tail)?;
            let record = format!("kind: repair\ntarget: {target}\ntested: {tip}\n");
            MailWriter::open_record(mail, &need, &record)?;
        }
    }
    let json = serde_json::to_string(failing)
        .map_err(|e| VivariumError::Message(format!("cannot store the failing set: {e}")))?;
    store.set_main_state(&state_key, &json)
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

/// Send the repair need (marked `repair`) and return its handle.
fn file_need(
    ctx: &Ctx,
    target: &str,
    tip: &str,
    failing: &BTreeSet<String>,
    tail: &str,
) -> Result<String, VivariumError> {
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
    let out = ctx.vivi(&args, 60);
    if out.code != 0 {
        return Err(VivariumError::Message(format!(
            "repair need send failed: {}",
            out.text.lines().last().unwrap_or("").trim()
        )));
    }
    need_handle(&out.text).ok_or_else(|| {
        VivariumError::Message("repair need send gave no handle in its output".into())
    })
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
