use crate::letter::fixture::{need_body_with, want_body};
use crate::letter::work::NeedFront;
use crate::mailspace::{Mailspace, SendRequest};

fn send(mailspace: &Mailspace, kind: &str, body: String) -> String {
    let result = mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "s".into(),
            body,
            role: format!("{kind}s"),
            kind: Some(kind.into()),
            reply_to: None,
        })
        .unwrap();
    result.delivered[0].handle.clone()
}

#[test]
fn metadata_queries_return_live_rows_in_message_id_order() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("mind").unwrap();
    mailspace.add_identity("hand").unwrap();
    let want = send(&mailspace, "want", want_body("q", "p"));
    let storage = mailspace.storage().unwrap();
    let want_row = storage.resolve_message_token(&want).unwrap();
    let id = storage
        .message_by_id(&want_row)
        .unwrap()
        .unwrap()
        .normalized_message_id
        .unwrap();
    drop(storage);

    let mut front = NeedFront::new(vec!["done".to_string()]);
    front.serves = Some(want);
    let need = send(&mailspace, "need", need_body_with(&front, "p"));

    let storage = mailspace.storage().unwrap();
    let need_row = storage.resolve_message_token(&need).unwrap();
    let rows = storage.rows_with_item_metadata("serves", &id).unwrap();
    assert!(rows.contains(&need_row), "{rows:?}");
    assert!(rows.windows(2).all(|w| w[0] <= w[1]));
    assert!(
        storage
            .rows_with_item_metadata("serves", "other")
            .unwrap()
            .is_empty()
    );

    let values = storage.item_metadata_values("serves").unwrap();
    assert!(values.contains(&(need_row, id)), "{values:?}");
    assert!(storage.item_metadata_values("explores").unwrap().is_empty());
}
