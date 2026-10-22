use super::*;
use crate::mailspace::SendRequest;

fn roster() -> (Mailspace, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("operator").unwrap();
    mailspace.add_identity("mind").unwrap();
    (mailspace, tmp)
}

fn send_need(mailspace: &Mailspace, subject: &str) -> String {
    mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: "done_when: it works".into(),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

#[test]
fn send_files_an_open_record_tied_to_the_need() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    let record = mailspace.pump_send(None, &need, None).unwrap();
    assert_eq!(record.kind, "pump");
    assert_eq!(record.status, PumpState::Open);
    assert_eq!(record.need, need);
    assert!(record.to.contains("mind"), "{record:?}");
    assert!(record.from.contains("operator"), "{record:?}");
    let listed = mailspace.pump_list(None, Some(PumpState::Open)).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].handle, record.handle);
}

#[test]
fn second_send_while_open_names_the_open_record() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    let first = mailspace.pump_send(None, &need, None).unwrap();
    let error = mailspace.pump_send(None, &need, None).unwrap_err();
    assert!(error.to_string().contains(&first.handle), "{error}");
}

#[test]
fn done_then_send_files_a_second_record_and_history_orders_both() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    let first = mailspace.pump_send(None, &need, None).unwrap();
    let closed = mailspace.pump_done(&first.handle, Some("gave-up")).unwrap();
    assert_eq!(closed.status, PumpState::Done);
    assert_eq!(closed.outcome.as_deref(), Some("gave-up"));
    let second = mailspace
        .pump_send(Some("operator"), &need, Some("again"))
        .unwrap();
    assert_ne!(first.handle, second.handle);
    let history = mailspace.pump_history(&need).unwrap();
    let states: Vec<_> = history
        .iter()
        .map(|r| (r.handle.clone(), r.status))
        .collect();
    assert_eq!(
        states,
        vec![
            (first.handle.clone(), PumpState::Done),
            (second.handle.clone(), PumpState::Open),
        ]
    );
    let done = mailspace
        .pump_list(Some(&need), Some(PumpState::Done))
        .unwrap();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].outcome.as_deref(), Some("gave-up"));
}

#[test]
fn reopen_is_refused_while_another_record_of_the_need_is_open() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    let first = mailspace.pump_send(None, &need, None).unwrap();
    mailspace.pump_done(&first.handle, None).unwrap();
    let second = mailspace.pump_send(None, &need, None).unwrap();
    let error = mailspace.pump_reopen(&first.handle).unwrap_err();
    assert!(error.to_string().contains(&second.handle), "{error}");
    mailspace.pump_done(&second.handle, Some("landed")).unwrap();
    let reopened = mailspace.pump_reopen(&first.handle).unwrap();
    assert_eq!(reopened.status, PumpState::Open);
    assert_eq!(reopened.outcome, None);
}

#[test]
fn done_and_reopen_refuse_the_wrong_state() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    let record = mailspace.pump_send(None, &need, None).unwrap();
    assert!(mailspace.pump_reopen(&record.handle).is_err());
    mailspace.pump_done(&record.handle, None).unwrap();
    assert!(mailspace.pump_done(&record.handle, None).is_err());
}

#[test]
fn unknown_need_and_non_need_are_refused() {
    let (mailspace, _tmp) = roster();
    assert!(mailspace.pump_send(None, "deadbeef", None).is_err());
    let task = mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "a task".into(),
            body: "body".into(),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle;
    let error = mailspace.pump_send(None, &task, None).unwrap_err();
    assert!(error.to_string().contains("not need"), "{error}");
}

#[test]
fn show_resolves_a_sent_copy_and_reads_the_body() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    let record = mailspace
        .pump_send(None, &need, Some("dispatch it now"))
        .unwrap();
    let (shown, body) = mailspace.pump_show(&record.handle).unwrap();
    assert_eq!(shown.handle, record.handle);
    assert!(body.contains("dispatch it now"), "{body}");
    assert!(mailspace.pump_show(&need).is_err());
}

#[test]
fn kind_round_trips_through_the_listing_and_stays_out_of_the_backlog() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    let record = mailspace.pump_send(None, &need, None).unwrap();
    let rows = mailspace.list_kind(None, "pumps", "pump").unwrap();
    assert_eq!(rows.len(), 1);
    mailspace.pump_done(&record.handle, None).unwrap();
    let storage = mailspace.storage().unwrap();
    let id = storage.resolve_message_token(&record.handle).unwrap();
    let view = storage.message_by_id(&id).unwrap().unwrap();
    assert_eq!(view.local_role, "done");
    assert_eq!(mailspace.source_kind(&storage, &view).unwrap(), "pump");
    drop(storage);
    let json = serde_json::to_value(mailspace.pump_history(&need).unwrap()).unwrap();
    assert_eq!(json[0]["kind"], "pump");
    assert_eq!(json[0]["status"], "done");
    let graph = mailspace.graph_show("backlog").unwrap();
    assert!(graph.nodes.iter().all(|n| n.source_id != record.handle));
    assert_eq!(graph.nodes.len(), 1, "only the need has a node");
    let audit = mailspace.backlog_audit(true).unwrap();
    assert!(audit.findings.is_empty(), "{:?}", audit.findings);
    assert_eq!(mailspace.graph_show("backlog").unwrap().nodes.len(), 1);
}

#[test]
fn pump_config_names_the_sender_and_the_pump_identity() {
    let (mut mailspace, _tmp) = roster();
    mailspace.add_identity("pumper").unwrap();
    let mut pump = crate::pump::config::PumpConfig::default();
    pump.reply_from = "pumper".into();
    mailspace.config.pump = Some(pump);
    let need = send_need(&mailspace, "fix auth");
    let record = mailspace.pump_send(None, &need, None).unwrap();
    assert!(record.to.contains("pumper"), "{record:?}");
}
