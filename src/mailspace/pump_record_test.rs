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
            body: crate::letter::fixture::need_body(&["it works"], ""),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
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
            body: crate::letter::fixture::task_body("body"),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
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
            body: crate::letter::fixture::need_body(&["it works"], ""),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
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
fn send_admits_a_need_addressed_to_a_third_identity() {
    let (mut mailspace, _tmp) = roster();
    mailspace.add_identity("elsewhere").unwrap();
    let stray = mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["elsewhere".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "not for the pump".into(),
            body: crate::letter::fixture::need_body(&["it works"], ""),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle;

    let record = mailspace.pump_send(None, &stray, None).unwrap();

    assert_eq!(record.need, stray);
    assert_eq!(record.status, PumpState::Open);
    assert_eq!(mailspace.pump_list(None, None).unwrap().len(), 1);
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
            body: crate::letter::fixture::need_body(&["it works"], ""),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
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
                body: crate::letter::fixture::need_body(&["it works"], ""),
                role: "needs".into(),
                kind: Some("need".into()),
                reply_to: None,
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
    // A need another identity sent to a third party is admitted too.
    let other = mailspace.pump_send(None, &stray, None).unwrap();
    assert_eq!(other.need, stray);
    assert_eq!(other.status, PumpState::Open);
}

fn pump_with_targets(mailspace: &mut Mailspace, ids: &[&str]) {
    let mut pump = crate::pump::config::PumpConfig::default();
    for id in ids {
        pump.targets.insert(
            (*id).to_string(),
            crate::pump::config::PumpTarget::default(),
        );
    }
    mailspace.config.pump = Some(pump);
}

fn refused(mailspace: &Mailspace, need: &str, body: &str) -> String {
    let error = mailspace
        .pump_send(None, need, Some(body))
        .unwrap_err()
        .to_string();
    assert!(
        mailspace.pump_list(None, None).unwrap().is_empty(),
        "a refused send filed a record"
    );
    error
}

#[test]
fn send_refuses_a_closed_need() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    mailspace
        .move_item_outcome("mind", &need, "done", Some("finished"), "need done", None)
        .unwrap();
    let error = refused(&mailspace, &need, "kind: build");
    assert!(error.contains("is done"), "{error}");
    assert!(error.contains("needs an open need"), "{error}");
}

#[test]
fn send_refuses_a_kind_line_naming_no_flow_kind() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    let error = refused(&mailspace, &need, "kind: frobnicate");
    assert!(
        error.contains("build, research, release, repair"),
        "{error}"
    );
    assert!(error.contains("actual: frobnicate"), "{error}");
}

#[test]
fn send_refuses_a_repair_record_without_a_known_target() {
    let (mut mailspace, _tmp) = roster();
    pump_with_targets(&mut mailspace, &["alpha", "beta"]);
    let need = send_need(&mailspace, "fix auth");
    let none = refused(&mailspace, &need, "kind: repair");
    assert!(none.contains("alpha, beta"), "{none}");
    assert!(none.contains("actual: none"), "{none}");
    let empty = refused(&mailspace, &need, "kind: repair\ntarget:");
    assert!(empty.contains("actual: none"), "{empty}");
    let unknown = refused(&mailspace, &need, "kind: repair\ntarget: gamma");
    assert!(unknown.contains("actual: gamma"), "{unknown}");
}

#[test]
fn a_repair_mark_makes_the_record_a_repair_record() {
    let (mut mailspace, _tmp) = roster();
    pump_with_targets(&mut mailspace, &["alpha"]);
    let need = send_need(&mailspace, "fix auth");
    mailspace
        .set_marks("mind", &need, "need", true, &["repair".to_string()])
        .unwrap();
    let error = refused(&mailspace, &need, "no kind line here");
    assert!(error.contains("actual: none"), "{error}");
    mailspace
        .pump_send(None, &need, Some("target: alpha"))
        .unwrap();
}

#[test]
fn send_accepts_valid_build_and_repair_records() {
    let (mut mailspace, _tmp) = roster();
    pump_with_targets(&mut mailspace, &["alpha"]);
    let build = send_need(&mailspace, "build it");
    mailspace
        .pump_send(None, &build, Some("kind: build"))
        .unwrap();
    let repair = send_need(&mailspace, "repair it");
    mailspace
        .pump_send(None, &repair, Some("kind: repair\ntarget: alpha"))
        .unwrap();
    assert_eq!(mailspace.pump_list(None, None).unwrap().len(), 2);
}

#[test]
fn without_pump_config_a_repair_record_needs_a_target_line_but_any_name() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace, "repair it");
    let error = refused(&mailspace, &need, "kind: repair");
    assert!(error.contains("actual: none"), "{error}");
    mailspace
        .pump_send(None, &need, Some("kind: repair\ntarget: anything"))
        .unwrap();
}

#[test]
fn send_accepts_a_need_addressed_to_hand() {
    let (mut mailspace, _tmp) = roster();
    mailspace.add_identity("hand").unwrap();
    let need = mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "for hand".into(),
            body: crate::letter::fixture::need_body(&["it works"], ""),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle;
    let record = mailspace.pump_send(None, &need, None).unwrap();
    assert_eq!(record.status, PumpState::Open);
}

#[test]
fn a_record_has_no_need_header_and_its_body_parses_as_a_record_front() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    let record = mailspace.pump_send(None, &need, None).unwrap();
    let storage = mailspace.storage().unwrap();
    let id = storage.resolve_message_token(&record.handle).unwrap();
    let raw = String::from_utf8(storage.read_message(&id).unwrap()).unwrap();
    assert!(!raw.contains("X-Vivi-Need"), "{raw}");
    let (shown, body) = mailspace.pump_show(&record.handle).unwrap();
    let envelope = crate::letter::read::record_of(&shown.handle, &body).unwrap();
    assert_eq!(envelope.front.need, need);
    assert_eq!(envelope.body, format!("Dispatch of need {need}."));
}

#[test]
fn a_full_front_matter_body_is_accepted_and_a_need_mismatch_refused() {
    let (mailspace, _tmp) = roster();
    let first = send_need(&mailspace, "fix auth");
    let second = send_need(&mailspace, "fix mail");
    let body = crate::letter::fixture::record_body(&first, "build");
    let record = mailspace.pump_send(None, &first, Some(&body)).unwrap();
    assert_eq!(record.need, first);
    let (_, stored) = mailspace.pump_show(&record.handle).unwrap();
    let front = crate::letter::read::record_of("", &stored).unwrap().front;
    assert_eq!(front.kind, Some(crate::letter::records::RecordKind::Build));
    let error = mailspace
        .pump_send(None, &second, Some(&body))
        .unwrap_err()
        .to_string();
    assert!(error.contains("names need"), "{error}");
    assert_eq!(mailspace.pump_list(None, None).unwrap().len(), 1);
    assert!(error.contains(&format!("not {second}")), "{error}");
}

#[test]
fn a_record_in_the_old_form_lists_with_no_need_and_a_letter_error_naming_it() {
    let (mailspace, _tmp) = roster();
    let need = send_need(&mailspace, "fix auth");
    let extras = [("X-Vivi-Need".to_string(), need.clone())];
    let sent = mailspace
        .send_with_extras(
            SendRequest {
                from: "operator".into(),
                to: vec!["mind".into()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: "pump old".into(),
                body: "Dispatch of an old record.".into(),
                role: "pumps".into(),
                kind: Some("pump".into()),
                reply_to: None,
            },
            &extras,
        )
        .unwrap();
    let listed = mailspace.pump_list(None, None).unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].need, "");
    let error = listed[0].letter_error.as_deref().unwrap();
    assert!(error.contains(&listed[0].handle), "{error}");
    assert!(error.contains("old format"), "{error}");
    assert!(!sent.delivered.is_empty());
}
