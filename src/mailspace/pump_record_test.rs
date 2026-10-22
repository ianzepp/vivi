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
    // The need must reach the pump identity, or the board could never plan it.
    let need = mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["pumper".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "fix auth".into(),
            body: "done_when: it works".into(),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle;
    let record = mailspace.pump_send(None, &need, None).unwrap();
    assert!(record.to.contains("pumper"), "{record:?}");
}

/// Run `work` on `count` threads at once, each with its own mailspace
/// connection (as separate processes would have), released together.
fn race<T: Send + 'static>(
    root: &std::path::Path,
    count: usize,
    work: impl Fn(&Mailspace) -> T + Send + Sync + 'static,
) -> Vec<T> {
    let work = std::sync::Arc::new(work);
    let start = std::sync::Arc::new(std::sync::Barrier::new(count));
    let handles: Vec<_> = (0..count)
        .map(|_| {
            let (work, start) = (work.clone(), start.clone());
            let root = root.to_path_buf();
            std::thread::spawn(move || {
                let mailspace = Mailspace::discover(Some(&root)).unwrap();
                start.wait();
                work(&mailspace)
            })
        })
        .collect();
    handles.into_iter().map(|h| h.join().unwrap()).collect()
}

#[test]
fn concurrent_sends_for_one_need_open_exactly_one_record() {
    let (mailspace, tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    let target = need.clone();
    let results = race(tmp.path(), 6, move |space| {
        space.pump_send(None, &target, None)
    });
    let opened: Vec<_> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
    assert_eq!(opened.len(), 1, "exactly one send wins: {results:?}");
    for refused in results.iter().filter_map(|r| r.as_ref().err()) {
        let text = refused.to_string();
        assert!(text.contains("already has an open pump record"), "{text}");
        assert!(text.contains(&opened[0].handle), "{text}");
    }
    let records = mailspace.pump_history(&need).unwrap();
    assert_eq!(records.len(), 1, "no loser left a record behind");
    assert_eq!(records[0].status, PumpState::Open);
}

#[test]
fn concurrent_reopens_of_two_done_records_open_exactly_one() {
    let (mailspace, tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    let first = mailspace.pump_send(None, &need, None).unwrap();
    mailspace.pump_done(&first.handle, None).unwrap();
    let second = mailspace.pump_send(None, &need, None).unwrap();
    mailspace.pump_done(&second.handle, None).unwrap();
    let handles = [first.handle.clone(), second.handle.clone()];
    let turn = std::sync::atomic::AtomicUsize::new(0);
    let results = race(tmp.path(), 2, move |space| {
        let which = turn.fetch_add(1, std::sync::atomic::Ordering::SeqCst) % 2;
        space.pump_reopen(&handles[which])
    });
    assert_eq!(
        results.iter().filter(|r| r.is_ok()).count(),
        1,
        "{results:?}"
    );
    let open = mailspace
        .pump_list(Some(&need), Some(PumpState::Open))
        .unwrap();
    assert_eq!(open.len(), 1);
}

#[test]
fn a_refused_send_leaves_the_mailspace_unchanged() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    let first = mailspace.pump_send(None, &need, None).unwrap();
    assert!(mailspace.pump_send(None, &need, None).is_err());
    assert!(
        mailspace
            .pump_send(Some("nobody"), "deadbeef", None)
            .is_err()
    );
    let all = mailspace.pump_list(None, None).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].handle, first.handle);
    // The connection is usable again: the failed batches were rolled back.
    mailspace.pump_done(&first.handle, None).unwrap();
    mailspace.pump_send(None, &need, None).unwrap();
}

#[test]
fn send_is_refused_for_a_need_the_pump_could_never_plan() {
    let (mut mailspace, _tmp) = roster();
    mailspace.add_identity("elsewhere").unwrap();
    let stray = mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["elsewhere".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "not for the pump".into(),
            body: "done_when: it works".into(),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle;

    let error = mailspace
        .pump_send(None, &stray, None)
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("not addressed to the pump identity 'mind'"),
        "{error}"
    );
    assert!(mailspace.pump_list(None, None).unwrap().is_empty());
    let routed = send_need(&mailspace, "for the pump");
    mailspace.pump_send(None, &routed, None).unwrap();
}

#[test]
fn a_need_copied_to_the_pump_identity_is_plannable() {
    let (mut mailspace, _tmp) = roster();
    mailspace.add_identity("elsewhere").unwrap();
    let copied = mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["elsewhere".into()],
            cc: vec!["mind".into()],
            bcc: Vec::new(),
            subject: "cc the pump".into(),
            body: "done_when: it works".into(),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle;

    mailspace.pump_send(None, &copied, None).unwrap();
}

#[test]
fn a_need_the_pump_identity_sent_itself_gets_a_record_whoever_it_is_addressed_to() {
    let (mut mailspace, _tmp) = roster();
    mailspace.add_identity("elsewhere").unwrap();
    let send = |from: &str, to: &str, subject: &str| {
        mailspace
            .send(SendRequest {
                from: from.into(),
                to: vec![to.into()],
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
    };
    let own = send(
        "mind",
        "operator",
        "pump: gate fix for g01234567 (1 new, 0 residual)",
    );
    let stray = send("operator", "elsewhere", "not for the pump");

    let record = mailspace.pump_send(None, &own, None).unwrap();

    assert_eq!(record.need, own);
    assert_eq!(record.status, PumpState::Open);
    // A need another identity sent to a third party is still refused.
    let error = mailspace
        .pump_send(None, &stray, None)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("not addressed to the pump identity"),
        "{error}"
    );
}
