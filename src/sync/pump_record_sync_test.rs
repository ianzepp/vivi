//! A pump record is the pump's own, local record: a push selects the need
//! and skips the record without reporting it as stuck.

use crate::letter::fixture::need_body;
use crate::mailspace::SendRequest;
use crate::sync::fake_server::checkout;
use crate::sync::push;
use crate::sync::select::SkipReason;

#[test]
fn push_skips_pump_records_as_a_local_kind() {
    let co = checkout("alice", &["mind", "operator"]);
    let need = co
        .mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "fix auth".into(),
            body: need_body(&["done"], "body\n"),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle;
    let record = co.mailspace.pump_send(None, &need, None).unwrap();
    let selection = push::pending(&co.mailspace, &co.settings).unwrap();
    assert!(
        selection.letters.iter().all(|l| l.kind != "pump"),
        "{selection:?}"
    );
    assert!(selection.letters.iter().any(|l| l.kind == "need"));
    let skipped: Vec<_> = selection
        .skipped
        .iter()
        .filter(|s| s.reason == SkipReason::OtherKind("pump".into()))
        .collect();
    assert_eq!(skipped.len(), 1, "{selection:?}");
    assert_eq!(skipped[0].handle, record.handle);
    assert!(!skipped[0].reason.actionable());
    assert!(!skipped[0].reason.undeliverable());
}
