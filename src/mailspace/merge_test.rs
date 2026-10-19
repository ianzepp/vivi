use super::*;
use crate::mailspace::SendRequest;

fn roster() -> (Mailspace, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("ceo").unwrap();
    mailspace.add_identity("cto").unwrap();
    (mailspace, tmp)
}

fn send_work(mailspace: &Mailspace, kind: &str, subject: &str, deps: &[String]) -> String {
    mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: "done_when: done".into(),
            role: format!("{kind}s"),
            kind: Some(kind.into()),
            reply_to: None,
            depends_on: deps.to_vec(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

#[test]
fn import_mints_backlog_nodes_and_dependency_edges() {
    let (source, src_tmp) = roster();
    let (target, _tgt_tmp) = roster();

    let first = send_work(&source, "need", "first", &[]);
    send_work(&source, "task", "second", &[first]);

    let report = import_mailspace(
        &target,
        src_tmp.path(),
        MailspaceImportOptions { dry_run: false },
    )
    .unwrap();
    // Each send lands a delivered row plus a sent row.
    assert_eq!(report.imported_messages, 4);

    let show = target.graph_show("backlog").unwrap();
    let node = |label: &str| {
        show.nodes
            .iter()
            .find(|node| node.label == label)
            .unwrap_or_else(|| panic!("no backlog node labelled {label}: {show:?}"))
            .clone()
    };
    let first_node = node("first");
    assert_eq!(first_node.readiness, "ready");
    assert_eq!(first_node.state, "open");
    assert_eq!(node("second").readiness, "blocked");
    assert_eq!(show.edges.len(), 1, "edges: {:?}", show.edges);

    let audit = target.backlog_audit(false).unwrap();
    assert!(
        audit.findings.is_empty(),
        "audit findings after import: {:?}",
        audit.findings
    );
}

#[test]
fn import_mints_settled_work_as_done() {
    let (source, src_tmp) = roster();
    let (mut target, _tgt_tmp) = roster();
    // An imported item's From address is not this mailspace's own, so only a
    // role of kind mind or operator accepts it: cto's settled row is an
    // acceptance, not a report awaiting one.
    target
        .set_role(
            "cto",
            crate::mailspace::RoleUpdate {
                kind: Some(Some("mind".into())),
                ..Default::default()
            },
        )
        .unwrap();

    let settled = send_work(&source, "task", "settled", &[]);
    // Settle it behind the graph's back: no event letter, no node completion,
    // only the storage move into the done folder.
    source
        .storage()
        .unwrap()
        .move_message_to_role("cto", &settled, "done")
        .unwrap();

    import_mailspace(
        &target,
        src_tmp.path(),
        MailspaceImportOptions { dry_run: false },
    )
    .unwrap();

    let show = target.graph_show("backlog").unwrap();
    let node = show
        .nodes
        .iter()
        .find(|node| node.label == "settled")
        .unwrap_or_else(|| panic!("no backlog node labelled settled: {show:?}"));
    assert_eq!(
        node.state, "done",
        "the done folder mints and completes the node: {node:?}"
    );

    let audit = target.backlog_audit(false).unwrap();
    assert!(
        audit.findings.is_empty(),
        "audit findings after import: {:?}",
        audit.findings
    );
}
