use super::*;
use crate::mailspace::SendRequest;
use crate::storage::MessageIngestRequest;

fn roster() -> (Mailspace, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("ceo").unwrap();
    mailspace.add_identity("cto").unwrap();
    (mailspace, tmp)
}

fn send_item(mailspace: &Mailspace, role: &str, kind: &str, subject: &str, body: &str) -> String {
    mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: body.into(),
            role: role.into(),
            kind: Some(kind.into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

/// File an inbound work letter the way a pull does, honouring the dependency
/// headers another mailspace's sender wrote, and attach it to the backlog.
fn file_letter(
    mailspace: &Mailspace,
    account: &str,
    kind: &str,
    subject: &str,
    deps: &[&str],
) -> String {
    let domain = mailspace.address_domain();
    let mut headers = format!(
        "From: mind@{domain}\r\nTo: hand@{domain}\r\nSubject: {subject}\r\n\
         Message-ID: <{subject}@x.test>\r\nDate: Sun, 11 Oct 2026 12:00:00 +0000\r\n\
         X-Vivi-Kind: {kind}\r\n"
    );
    for dep in deps {
        headers.push_str(&format!("X-Vivi-Depends-On: {dep}\r\n"));
    }
    let bytes = format!("{headers}\r\ndone_when: {subject}\r\n").into_bytes();
    let storage = mailspace.storage().unwrap();
    let stored = storage
        .ingest_message(
            &MessageIngestRequest {
                account: account.into(),
                local_role: format!("{kind}s"),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: format!("{account}\0{subject}"),
                remote: None,
            },
            &bytes,
        )
        .unwrap();
    let handle = storage.display_handle(&stored.message_id).unwrap();
    drop(storage);
    mailspace
        .backlog_attach_letter(kind, &stored.message_id, subject, &bytes)
        .unwrap();
    mailspace
        .backlog_settle_pending_dependencies(&[(stored.message_id.clone(), stored.content_id)])
        .unwrap();
    handle
}

fn manifest_for(mailspace: &Mailspace, item: &str) -> Option<StepDispatch> {
    mailspace
        .step_shadow()
        .unwrap()
        .dispatches
        .iter()
        .find(|d| d.item == item)
        .cloned()
}

fn exception_for(mailspace: &Mailspace, item: &str) -> Option<StepException> {
    mailspace
        .step_shadow()
        .unwrap()
        .exceptions
        .iter()
        .find(|e| e.item == item)
        .cloned()
}

#[test]
fn step_manifest_empty_without_backlog_graph() {
    let tmp = tempfile::tempdir().unwrap();
    let mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    let manifest = mailspace.step_shadow().unwrap();
    assert!(manifest.dispatches.is_empty());
    assert!(manifest.exceptions.is_empty());
    assert!(manifest.decisions.is_empty());
}

#[test]
fn ready_task_with_done_when_dispatches() {
    let (mailspace, _tmp) = roster();
    let task = send_item(
        &mailspace,
        "tasks",
        "task",
        "unit A",
        "goal: g.md\nunit: U-1\ndone_when: malformed input rejected\nwrite_scope: src/validate.rs",
    );
    let dispatch =
        manifest_for(&mailspace, &task).unwrap_or_else(|| panic!("expected dispatch for {task}"));
    assert_eq!(dispatch.kind, "task");
    assert_eq!(dispatch.done_when_clauses, 1);
    assert!(dispatch.write_scope);
    assert_eq!(dispatch.subject, "unit A");
}

#[test]
fn ready_wants_require_promotion() {
    let (mailspace, _tmp) = roster();
    let want = send_item(
        &mailspace,
        "wants",
        "want",
        "later work",
        "done_when: exists",
    );
    let exception =
        exception_for(&mailspace, &want).unwrap_or_else(|| panic!("expected exception for {want}"));
    assert_eq!(exception.reason, "want_requires_promotion");
    assert!(exception.detail.contains("parked"), "{exception:?}");
}

#[test]
fn clauseless_wants_combine_parked_and_missing_clause() {
    let (mailspace, _tmp) = roster();
    let want = send_item(&mailspace, "wants", "want", "parked idea", "someday");
    let exception =
        exception_for(&mailspace, &want).unwrap_or_else(|| panic!("expected exception for {want}"));
    assert_eq!(exception.reason, "want_requires_promotion");
    assert!(exception.detail.contains("parked"), "{exception:?}");
    assert!(exception.detail.contains("no done_when"), "{exception:?}");
}

#[test]
fn task_without_done_when_is_exception() {
    let (mailspace, _tmp) = roster();
    let task = send_item(&mailspace, "tasks", "task", "vague work", "just do it");
    let exception =
        exception_for(&mailspace, &task).unwrap_or_else(|| panic!("expected exception for {task}"));
    assert_eq!(exception.reason, "no_done_when");
    assert!(
        exception
            .detail
            .contains("completion could not be verified"),
        "{exception:?}"
    );
}

#[test]
fn no_done_when_detail_names_coordination_fields() {
    let (mailspace, _tmp) = roster();
    let task = send_item(
        &mailspace,
        "tasks",
        "task",
        "audit pass",
        "verdict: clean_pass\nrepo: vivi\ntip: 0a6d1a1\nNote: prose stays out.",
    );
    let exception =
        exception_for(&mailspace, &task).unwrap_or_else(|| panic!("expected exception for {task}"));
    assert_eq!(exception.reason, "no_done_when");
    assert!(
        exception
            .detail
            .contains("labeled fields present: verdict, repo, tip"),
        "{exception:?}"
    );
}

#[test]
fn lowered_need_awaits_units() {
    let (mailspace, _tmp) = roster();
    let need = send_item(
        &mailspace,
        "needs",
        "need",
        "lower me",
        "done_when: join complete",
    );
    let unit = send_item(
        &mailspace,
        "tasks",
        "task",
        "unit 1",
        "done_when: unit lands",
    );
    mailspace.backlog_bind_units(&need, &[unit]).unwrap();

    let exception =
        exception_for(&mailspace, &need).unwrap_or_else(|| panic!("expected exception for {need}"));
    assert_eq!(exception.reason, "lowered_awaiting_units");
}

#[test]
fn blocked_nodes_do_not_appear() {
    let (mailspace, _tmp) = roster();
    let dep = send_item(&mailspace, "tasks", "task", "first", "done_when: lands");
    // A task depending on the first one stays out of the manifest entirely
    // until its prerequisite completes.
    let result = mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "second".into(),
            body: "done_when: lands".into(),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: vec![dep],
        })
        .unwrap();
    let blocked_item = &result.delivered[0].handle;

    let manifest = mailspace.step_shadow().unwrap();
    assert!(
        !manifest.dispatches.iter().any(|d| d.item == *blocked_item),
        "{manifest:?}"
    );
    assert!(
        !manifest.exceptions.iter().any(|e| e.item == *blocked_item),
        "{manifest:?}"
    );
}

#[test]
fn blocked_dependent_awaiting_unheld_dependency_is_exception() {
    let (mailspace, _tmp) = roster();
    // A task whose letter names a dependency this mailspace does not hold.
    let waiting = file_letter(&mailspace, "cto", "task", "waits on a stranger", &["deadbeef"]);
    let control = send_item(
        &mailspace,
        "tasks",
        "task",
        "free to go",
        "done_when: lands",
    );

    let manifest = mailspace.step_shadow().unwrap();
    assert!(
        !manifest.dispatches.iter().any(|d| d.item == waiting),
        "a blocked dependent must not dispatch: {manifest:?}"
    );
    let reported: Vec<&StepException> = manifest
        .exceptions
        .iter()
        .filter(|e| e.item == waiting)
        .collect();
    assert_eq!(reported.len(), 1, "exactly one exception: {manifest:?}");
    assert_eq!(reported[0].reason, "awaiting_dependency");
    assert_eq!(reported[0].kind, "task");
    assert!(
        reported[0].detail.contains("deadbeef"),
        "detail names the unheld reference: {:?}",
        reported[0]
    );

    // A control task with no dependencies still dispatches.
    assert!(
        manifest.dispatches.iter().any(|d| d.item == control),
        "control task dispatches: {manifest:?}"
    );
}

#[test]
fn apply_rejects_unsettled_item() {
    let (mailspace, _tmp) = roster();
    let task = send_item(
        &mailspace,
        "tasks",
        "task",
        "still open",
        "done_when: lands",
    );
    let manifest = mailspace.step_apply(&task).unwrap();
    let exception = manifest
        .exceptions
        .iter()
        .find(|e| e.item == task)
        .unwrap_or_else(|| panic!("expected not_settled: {manifest:?}"));
    assert_eq!(exception.reason, "not_settled");
    assert!(manifest.decisions.is_empty());
}

#[test]
fn apply_completes_settled_task_and_records_decision() {
    let (mailspace, _tmp) = roster();
    let task = send_item(
        &mailspace,
        "tasks",
        "task",
        "settled work",
        "done_when: lands",
    );
    mailspace
        .move_item("ceo", &task, "done", None, "task done", None)
        .unwrap();

    // Rewind the node to open to model an out-of-band settle the hook
    // never saw, then let apply complete it.
    let storage = mailspace.storage().unwrap();
    let graph = storage.work_graph_by_code("backlog").unwrap().unwrap();
    let node = storage
        .work_graph_nodes(&graph.handle)
        .unwrap()
        .into_iter()
        .find(|n| n.source_id == task)
        .unwrap();
    drop(storage);
    mailspace
        .storage()
        .unwrap()
        .set_work_graph_node_state(&graph.handle, &node.handle, "open", None)
        .unwrap();

    let manifest = mailspace.step_apply(&task).unwrap();
    assert_eq!(manifest.decisions.len(), 1, "{manifest:?}");
    assert!(manifest.decisions[0].contains("via=step-apply"));

    let storage = mailspace.storage().unwrap();
    let events = storage.list_work_graph_events_after(0).unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.event_type == "step_decision"
                && e.note.as_deref() == Some("via=step-apply")),
        "{events:?}"
    );

    // Applying again does not re-complete: only the settled note remains.
    let again = mailspace.step_apply(&task).unwrap();
    assert_eq!(again.decisions.len(), 1, "{again:?}");
    assert!(again.decisions[0].contains("via=lifecycle"), "{again:?}");
    assert!(!again.decisions[0].contains("via=step-apply"), "{again:?}");
}

#[test]
fn lifecycle_completion_records_decision_event() {
    let (mailspace, _tmp) = roster();
    let task = send_item(&mailspace, "tasks", "task", "unit work", "done_when: lands");
    mailspace
        .move_item("ceo", &task, "done", None, "task done", None)
        .unwrap();
    let storage = mailspace.storage().unwrap();
    let events = storage.list_work_graph_events_after(0).unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.event_type == "step_decision" && e.note.as_deref() == Some("via=lifecycle")),
        "{events:?}"
    );
}

/// Deterministic fake provider for shadow-screen tests.
struct FakeProvider {
    answers: Vec<f64>,
    fail: bool,
}

impl crate::judgment::JudgmentProvider for FakeProvider {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn model(&self) -> &str {
        "fake-1"
    }

    fn ask(
        &self,
        _state: &serde_json::Value,
        questions: &[crate::judgment::JudgmentQuestion],
    ) -> Result<Vec<crate::judgment::JudgmentAnswer>, crate::VivariumError> {
        if self.fail {
            return Err(crate::VivariumError::Message(
                "judgment provider unreachable: boom".into(),
            ));
        }
        Ok(questions
            .iter()
            .zip(&self.answers)
            .map(|(q, noul)| crate::judgment::JudgmentAnswer {
                id: q.id.clone(),
                noul: *noul,
            })
            .collect())
    }
}

/// Settle a two-clause task and rewind its node to open, ready for apply.
fn settled_rewound_task(mailspace: &Mailspace) -> String {
    let task = send_item(
        mailspace,
        "tasks",
        "task",
        "judged work",
        "done_when: tests pass\ndone_when: lint clean\nwrite_scope: src/x.rs",
    );
    mailspace
        .move_item("ceo", &task, "done", None, "task done", None)
        .unwrap();
    let storage = mailspace.storage().unwrap();
    let graph = storage.work_graph_by_code("backlog").unwrap().unwrap();
    let node = storage
        .work_graph_nodes(&graph.handle)
        .unwrap()
        .into_iter()
        .find(|n| n.source_id == task)
        .unwrap();
    let (graph_handle, node_handle) = (graph.handle.clone(), node.handle.clone());
    drop(storage);
    mailspace
        .storage()
        .unwrap()
        .set_work_graph_node_state(&graph_handle, &node_handle, "open", None)
        .unwrap();
    task
}

#[test]
fn judged_apply_screens_and_records_corpus() {
    let (mailspace, _tmp) = roster();
    let task = settled_rewound_task(&mailspace);
    let fake = FakeProvider {
        answers: vec![0.9, 0.8, 0.1],
        fail: false,
    };

    let manifest = mailspace.step_apply_with(&task, Some(&fake)).unwrap();
    assert_eq!(manifest.decisions.len(), 1, "{manifest:?}");
    let note = &manifest.decisions[0];
    assert!(note.contains("judgment=screened(2/2)"), "{note}");
    assert!(note.contains("model=fake-1"), "{note}");
    assert!(note.contains("honesty=0.10"), "{note}");

    let corpus = std::fs::read_to_string(_tmp.path().join(".vivi/judgment-corpus.jsonl")).unwrap();
    assert!(corpus.contains("\"coverage-1\""), "{corpus}");
    assert!(corpus.contains("\"completion-honesty\""), "{corpus}");
    assert!(corpus.contains("\"shadow\":true"), "{corpus}");
}

#[test]
fn judged_apply_is_shadow_and_never_gates() {
    let (mailspace, _tmp) = roster();
    let task = settled_rewound_task(&mailspace);
    let fake = FakeProvider {
        answers: vec![0.1, 0.2, 0.9],
        fail: false,
    };

    let manifest = mailspace.step_apply_with(&task, Some(&fake)).unwrap();
    assert!(manifest.decisions[0].contains("judgment=screened(0/2)"));
    // Completion happened regardless of the provider's low coverage answers.
    assert_eq!(
        mailspace
            .graph_show("backlog")
            .unwrap()
            .nodes
            .into_iter()
            .find(|n| n.source_id == task)
            .unwrap()
            .state,
        "done"
    );
}

#[test]
fn judged_apply_skips_cleanly_on_provider_error() {
    let (mailspace, _tmp) = roster();
    let task = settled_rewound_task(&mailspace);
    let fake = FakeProvider {
        answers: Vec::new(),
        fail: true,
    };

    let manifest = mailspace.step_apply_with(&task, Some(&fake)).unwrap();
    assert!(
        manifest.decisions[0].contains("judgment=skipped(unreachable)"),
        "{}",
        manifest.decisions[0]
    );
    assert!(
        !_tmp.path().join(".vivi/judgment-corpus.jsonl").exists(),
        "no corpus record on provider failure"
    );
}
