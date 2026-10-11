//! `vivi step`: mechanical adjudication of the backlog graph into a
//! dispatch/exception manifest for the coordination host. Shadow form is
//! read-only — no mutation, no network, no provider.

use serde::Serialize;

use super::Mailspace;
use super::graph::GraphNodeView;
use super::holding::{Holding, READINESS_AWAITING};
use crate::error::VivariumError;
use crate::storage::BACKLOG_GRAPH_CODE;

/// One dispatchable ready node with its mechanical checks.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct StepDispatch {
    pub node: String,
    pub item: String,
    pub kind: String,
    pub subject: String,
    pub done_when_clauses: usize,
    pub write_scope: bool,
}

/// One ready node that needs attention instead of a seat.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct StepException {
    pub node: String,
    pub item: String,
    pub kind: String,
    pub reason: String,
    pub detail: String,
}

/// The full step manifest. `decisions` is empty in shadow form; apply mode
/// (Phase 05) fills it without changing the shape.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct StepManifest {
    pub graph: String,
    pub dispatches: Vec<StepDispatch>,
    pub exceptions: Vec<StepException>,
    pub decisions: Vec<String>,
}

impl Mailspace {
    /// Adjudicate ready backlog nodes mechanically. Read-only.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure.
    pub fn step_shadow(&self) -> Result<StepManifest, VivariumError> {
        let storage = self.storage()?;
        let Some(graph) = storage.work_graph_by_code(BACKLOG_GRAPH_CODE)? else {
            return Ok(StepManifest {
                graph: BACKLOG_GRAPH_CODE.to_string(),
                dispatches: Vec::new(),
                exceptions: Vec::new(),
                decisions: Vec::new(),
            });
        };
        let show = self.graph_show(BACKLOG_GRAPH_CODE)?;
        let mut dispatches = Vec::new();
        let mut exceptions = Vec::new();
        let frontier = show
            .nodes
            .iter()
            .filter(|node| matches!(node.readiness.as_str(), "ready" | READINESS_AWAITING));
        for node in frontier {
            if node.readiness == READINESS_AWAITING {
                exceptions.push(self.awaiting_remote(&storage, node)?);
                continue;
            }
            match adjudicate_node(&storage, &graph.handle, node)? {
                StepOutcome::Dispatch(dispatch) => dispatches.push(dispatch),
                StepOutcome::Exception(exc) => exceptions.push(exc),
            }
        }
        Ok(StepManifest {
            graph: graph.code,
            dispatches,
            exceptions,
            decisions: Vec::new(),
        })
    }

    /// The exception for a ready node that is this mailspace's own copy of
    /// work handed to another one: tracked here, dispatched there.
    fn awaiting_remote(
        &self,
        storage: &crate::storage::Storage,
        node: &GraphNodeView,
    ) -> Result<StepException, VivariumError> {
        let to = match self.node_holding(storage, &node.source_id)? {
            Holding::Sent(address) => address,
            _ => "another mailspace".into(),
        };
        Ok(exception(
            node,
            &node.kind,
            "awaiting_remote",
            &format!("handed to {to}; the receiving mailspace dispatches it"),
        ))
    }

    /// Apply mode without a judgment provider (mechanical only).
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure or when the handle
    /// does not resolve.
    pub fn step_apply(&self, handle: &str) -> Result<StepManifest, VivariumError> {
        self.step_apply_with(handle, None)
    }

    /// Apply mode: adjudicate one settled item, complete its backlog node if
    /// it is still completable (recording the decision atomically with the
    /// transition), then emit the full manifest. When a judgment provider is
    /// supplied, the item's receipt is screened (shadow: the provider's
    /// answers are recorded to the calibration corpus and never gate the
    /// mechanical completion). An item that is not settled yet becomes a
    /// `not_settled` exception — apply never settles work itself; settling
    /// (`task done` with its receipt flags) stays with the caller.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure or when the handle
    /// does not resolve.
    pub fn step_apply_with(
        &self,
        handle: &str,
        provider: Option<&dyn crate::judgment::JudgmentProvider>,
    ) -> Result<StepManifest, VivariumError> {
        let mut manifest = self.step_shadow()?;
        let storage = self.storage()?;
        let message_id = storage
            .resolve_message_token(handle)
            .map_err(|e| VivariumError::Message(format!("step item '{handle}': {e}")))?;
        let message = storage
            .message_by_id(&message_id)?
            .ok_or_else(|| VivariumError::Message(format!("step item not found: {handle}")))?;
        drop(storage);
        if message.local_role != "done" {
            manifest.exceptions.push(StepException {
                node: String::new(),
                item: handle.to_string(),
                kind: kind_for_role(&message.local_role),
                reason: "not_settled".into(),
                detail: "settle the item (task done with receipt flags) before apply".into(),
            });
            return Ok(manifest);
        }
        let display = self.storage()?.display_handle(&message_id)?;
        // Screen every settled item — the lifecycle hook usually completed
        // the node at settle time, so the screen must not depend on apply
        // being the completer. Shadow: the corpus records the answers; only
        // transitions this call performed appear in manifest decisions.
        let note = match provider {
            Some(provider) => Some(screen_receipt(self, provider, &display, &message_id)?),
            None => None,
        };
        if self.backlog_complete_item_via(&display, "via=step-apply")? {
            manifest.decisions.push(format!(
                "complete item={display} via=step-apply {}",
                note.as_deref().unwrap_or("judgment=off")
            ));
        } else if self.backlog_item_tracked(&display)? {
            manifest.decisions.push(format!(
                "item={display} via=lifecycle (already settled; nothing to apply)"
            ));
        } else {
            manifest.exceptions.push(StepException {
                node: String::new(),
                item: display.clone(),
                kind: "task".into(),
                reason: "untracked_item".into(),
                detail: "no backlog node for this item; run vivi graph audit --repair".into(),
            });
        }
        Ok(manifest)
    }
}

/// Per-node adjudication result.
enum StepOutcome {
    Dispatch(StepDispatch),
    Exception(StepException),
}

fn adjudicate_node(
    storage: &crate::storage::Storage,
    graph_handle: &str,
    node: &GraphNodeView,
) -> Result<StepOutcome, VivariumError> {
    let message = match storage.resolve_message_token(&node.source_id) {
        Ok(message_id) => storage.message_by_id(&message_id)?,
        Err(_) => None,
    };
    let Some(message) = message else {
        return Ok(StepOutcome::Exception(exception(
            node,
            "unknown",
            "item_missing",
            "no message resolves to this node",
        )));
    };
    let kind = kind_for_role(&message.local_role);
    let body = item_body(storage, &message.message_id)?;
    let clauses = count_labeled_clauses(&body, "done_when");
    if kind == "want" {
        let mut detail = "parked: wants never dispatch before explicit promotion".to_string();
        if clauses == 0 {
            detail.push_str("; body declares no done_when clause");
        }
        return Ok(StepOutcome::Exception(exception(
            node,
            &kind,
            "want_requires_promotion",
            &detail,
        )));
    }
    if !storage
        .work_graph_nodes_by_subgraph(graph_handle, &node.source_id)?
        .is_empty()
    {
        return Ok(StepOutcome::Exception(exception(
            node,
            &kind,
            "lowered_awaiting_units",
            "completion derives from bound unit tasks",
        )));
    }
    if clauses == 0 {
        return Ok(StepOutcome::Exception(exception(
            node,
            &kind,
            "no_done_when",
            &no_done_when_detail(&body),
        )));
    }
    Ok(StepOutcome::Dispatch(StepDispatch {
        node: node.handle.clone(),
        item: node.source_id.clone(),
        kind,
        subject: message.subject,
        done_when_clauses: clauses,
        write_scope: has_labeled_field(&body, "write_scope"),
    }))
}

fn kind_for_role(role: &str) -> String {
    match role {
        "tasks" => "task".into(),
        "needs" => "need".into(),
        "wants" => "want".into(),
        other => other.into(),
    }
}

fn exception(node: &GraphNodeView, kind: &str, reason: &str, detail: &str) -> StepException {
    StepException {
        node: node.handle.clone(),
        item: node.source_id.clone(),
        kind: kind.to_string(),
        reason: reason.to_string(),
        detail: detail.to_string(),
    }
}

fn item_body(storage: &crate::storage::Storage, message_id: &str) -> Result<String, VivariumError> {
    let data = storage.read_message(message_id)?;
    crate::extract::extract_text(&data)
}

fn count_labeled_clauses(body: &str, label: &str) -> usize {
    body.lines()
        .filter(|line| {
            line.trim_start()
                .to_ascii_lowercase()
                .starts_with(&format!("{label}:"))
        })
        .count()
}

fn has_labeled_field(body: &str, label: &str) -> bool {
    body.lines().any(|line| {
        line.trim_start()
            .to_ascii_lowercase()
            .starts_with(&format!("{label}:"))
    })
}

/// Whether a body carries at least one `label:` clause line. Shared with
/// the send runners' clause warning.
#[must_use]
pub fn body_has_labeled_clause(body: &str, label: &str) -> bool {
    has_labeled_field(body, label)
}

/// Detail for clauseless bodies: name the labeled fields the body does
/// carry, so coordination work (verdict/output bodies) reads as such
/// instead of as a defect.
fn no_done_when_detail(body: &str) -> String {
    let mut detail = String::from("body declares no done_when clause");
    let mut fields: Vec<String> = Vec::new();
    for label in body.lines().filter_map(labeled_line_label) {
        if label == "done_when" || fields.iter().any(|f| f == &label) || fields.len() >= 5 {
            continue;
        }
        fields.push(label);
    }
    if fields.is_empty() {
        detail.push_str("; completion could not be verified");
    } else {
        detail.push_str(&format!("; labeled fields present: {}", fields.join(", ")));
    }
    detail
}

/// Lowercase `label:` prefix of a line; prose labels (capitalized, spaced)
/// and continuation lines do not count.
fn labeled_line_label(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let end = trimmed.find(':')?;
    let label = &trimmed[..end];
    let mut chars = label.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    valid.then(|| label.to_ascii_lowercase())
}

/// Extract the text of each labeled clause (e.g. every `done_when:` line).
fn labeled_clauses(body: &str, label: &str) -> Vec<String> {
    let prefix = format!("{label}:");
    body.lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            let lower = trimmed.to_ascii_lowercase();
            lower
                .starts_with(&prefix)
                .then(|| trimmed[prefix.len()..].trim().to_string())
        })
        .collect()
}

/// Screen one settled item's receipt through the judgment provider. Shadow:
/// answers are appended to the calibration corpus and the returned note
/// summarizes them, but the caller's mechanical completion has already
/// happened and is never gated on the provider.
fn screen_receipt(
    mailspace: &Mailspace,
    provider: &dyn crate::judgment::JudgmentProvider,
    display: &str,
    message_id: &str,
) -> Result<String, VivariumError> {
    let storage = mailspace.storage()?;
    let body = item_body(&storage, message_id)?;
    let receipt = storage.item_metadata(message_id)?;
    let subject = storage
        .message_by_id(message_id)?
        .map(|message| message.subject)
        .unwrap_or_default();
    let clauses = labeled_clauses(&body, "done_when");
    let questions = receipt_questions(&clauses);
    let state = serde_json::json!({
        "item": display,
        "subject": subject,
        "clauses": clauses,
        "validation": body,
        "receipt": receipt,
    });
    match provider.ask(&state, &questions) {
        Ok(answers) => {
            append_corpus(
                mailspace,
                &corpus_record(provider, display, &questions, &answers),
            )?;
            let covered = answers
                .iter()
                .filter(|a| a.id != "completion-honesty" && a.noul >= 0.5)
                .count();
            let honesty = answers
                .iter()
                .find(|a| a.id == "completion-honesty")
                .map_or(0.0, |a| a.noul);
            Ok(format!(
                "judgment=screened({covered}/{}) honesty={honesty:.2} model={}",
                clauses.len(),
                provider.model()
            ))
        }
        Err(error) => Ok(format!(
            "judgment=skipped({})",
            error_class(&error.to_string())
        )),
    }
}

/// One coverage Noul per done_when clause, plus one completion-honesty Noul.
fn receipt_questions(clauses: &[String]) -> Vec<crate::judgment::JudgmentQuestion> {
    let mut questions: Vec<crate::judgment::JudgmentQuestion> = clauses
        .iter()
        .enumerate()
        .map(|(index, clause)| {
            crate::judgment::JudgmentQuestion::noul(
                &format!("coverage-{}", index + 1),
                &format!("A task reports completion with the validation evidence in state. Judge whether the evidence proves this acceptance clause: {clause}"),
                "The validation evidence demonstrates the clause is satisfied.",
                "The evidence does not address, or does not prove, the clause.",
            )
        })
        .collect();
    questions.push(crate::judgment::JudgmentQuestion::noul(
        "completion-honesty",
        "Judge whether anything in this completion report suggests the work did not actually finish (blocked, partial, placeholder, or deferred).",
        "The report shows signs the work did not finish.",
        "The report is a genuine completion.",
    ));
    questions
}

/// Build one shadow calibration-corpus record.
fn corpus_record(
    provider: &dyn crate::judgment::JudgmentProvider,
    display: &str,
    questions: &[crate::judgment::JudgmentQuestion],
    answers: &[crate::judgment::JudgmentAnswer],
) -> serde_json::Value {
    serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "item": display,
        "via": "step-apply",
        "provider": provider.name(),
        "model": provider.model(),
        "questions": questions.iter().map(|q| serde_json::json!({
            "id": q.id, "kind": q.kind, "instructions": q.instructions,
        })).collect::<Vec<_>>(),
        "answers": answers,
        "mechanical": "complete",
        "shadow": true,
    })
}

/// Short failure class for corpus-free skip notes.
fn error_class(message: &str) -> &'static str {
    if message.contains("timed out") {
        "timeout"
    } else if message.contains("credentials") {
        "auth"
    } else if message.contains("key_cmd") {
        "key_cmd"
    } else if message.contains("unreachable") {
        "unreachable"
    } else {
        "provider"
    }
}

/// Append one calibration-corpus record to `.vivi/judgment-corpus.jsonl`.
fn append_corpus(mailspace: &Mailspace, record: &serde_json::Value) -> Result<(), VivariumError> {
    use std::io::Write as _;
    let path = mailspace.dir.join("judgment-corpus.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| VivariumError::Other(format!("failed to open judgment corpus: {e}")))?;
    let line = serde_json::to_string(record)
        .map_err(|e| VivariumError::Other(format!("failed to encode corpus record: {e}")))?;
    writeln!(file, "{line}")
        .map_err(|e| VivariumError::Other(format!("failed to append corpus record: {e}")))
}

#[cfg(test)]
#[path = "step_test.rs"]
mod tests;
