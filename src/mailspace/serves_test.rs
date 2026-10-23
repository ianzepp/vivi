use crate::letter::fixture::{need_body_with, want_body};
use crate::letter::work::NeedFront;
use crate::mailspace::{Mailspace, SendRequest};

fn space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("mind").unwrap();
    mailspace.add_identity("hand").unwrap();
    (tmp, mailspace)
}

fn send(mailspace: &Mailspace, kind: &str, body: String) -> Result<String, String> {
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
            depends_on: Vec::new(),
        })
        .map_err(|err| err.to_string())?;
    Ok(result.delivered[0].handle.clone())
}

fn need_front(serves: Option<&str>, explores: Option<&str>, deps: &[&str]) -> String {
    let mut front = NeedFront::new(vec!["done".to_string()]);
    front.serves = serves.map(str::to_string);
    front.explores = explores.map(str::to_string);
    front.depends_on = deps.iter().map(ToString::to_string).collect();
    need_body_with(&front, "prose")
}

fn want_id(mailspace: &Mailspace, handle: &str) -> String {
    let storage = mailspace.storage().unwrap();
    let row = storage.resolve_message_token(handle).unwrap();
    storage
        .message_by_id(&row)
        .unwrap()
        .unwrap()
        .normalized_message_id
        .unwrap()
}

fn metadata(mailspace: &Mailspace, handle: &str) -> std::collections::BTreeMap<String, String> {
    mailspace.storage().unwrap().item_metadata(handle).unwrap()
}

#[test]
fn a_need_serving_a_want_stores_the_canonical_id_and_indexes_it() {
    let (_tmp, mailspace) = space();
    let want = send(&mailspace, "want", want_body("q", "p")).unwrap();
    let id = want_id(&mailspace, &want);

    let need = send(&mailspace, "need", need_front(Some(&want), None, &[])).unwrap();

    let eml = {
        let storage = mailspace.storage().unwrap();
        String::from_utf8(storage.read_message(&need).unwrap()).unwrap()
    };
    assert!(eml.contains(&format!("serves = \"<{id}>\"")), "{eml}");
    let meta = metadata(&mailspace, &need);
    assert_eq!(meta.get("serves"), Some(&id), "{meta:?}");
    assert!(!meta.contains_key("explores"));
}

#[test]
fn a_need_exploring_a_want_indexes_explores() {
    let (_tmp, mailspace) = space();
    let want = send(&mailspace, "want", want_body("q", "p")).unwrap();
    let id = want_id(&mailspace, &want);

    let need = send(&mailspace, "need", need_front(None, Some(&want), &[])).unwrap();

    let meta = metadata(&mailspace, &need);
    assert_eq!(meta.get("explores"), Some(&id), "{meta:?}");
}

#[test]
fn serving_or_exploring_a_task_or_a_missing_handle_is_refused() {
    let (_tmp, mailspace) = space();
    let task = send(&mailspace, "task", crate::letter::fixture::task_body("p")).unwrap();

    let err = send(&mailspace, "need", need_front(Some(&task), None, &[])).unwrap_err();
    assert!(
        err.ends_with(&format!("{task} is a task, not a want")),
        "{err}"
    );
    let err = send(&mailspace, "need", need_front(None, Some(&task), &[])).unwrap_err();
    assert!(
        err.ends_with(&format!("{task} is a task, not a want")),
        "{err}"
    );
    let err = send(&mailspace, "need", need_front(Some("zzzzzzzz"), None, &[])).unwrap_err();
    assert!(err.ends_with("serves zzzzzzzz: no such letter"), "{err}");
}

#[test]
fn serving_a_want_it_also_depends_on_is_refused() {
    let (_tmp, mailspace) = space();
    let want = send(&mailspace, "want", want_body("q", "p")).unwrap();

    let err = send(&mailspace, "need", need_front(Some(&want), None, &[&want])).unwrap_err();

    assert!(
        err.ends_with(&format!(
            "need serves {want} and depends on it: a need serves a want, it does not wait for it"
        )),
        "{err}"
    );
}

#[test]
fn a_need_with_both_serves_and_explores_is_refused_by_the_schema() {
    let (_tmp, mailspace) = space();
    let want = send(&mailspace, "want", want_body("q", "p")).unwrap();

    let err = send(
        &mailspace,
        "need",
        need_front(Some(&want), Some(&want), &[]),
    )
    .unwrap_err();

    assert!(err.contains("serves") && err.contains("explores"), "{err}");
}

#[test]
fn a_stored_want_id_resolves_to_this_mailspaces_own_copy() {
    let (_tmp, mailspace) = space();
    let want = send(&mailspace, "want", want_body("q", "p")).unwrap();
    let id = format!("<{}>", want_id(&mailspace, &want));
    let storage = mailspace.storage().unwrap();

    // The stored id is the letter's RFC Message-ID. No local row is keyed by it,
    // so a reader that handed it to the handle resolver would not find the want.
    assert!(storage.resolve_message_token(&id).is_err());
    let row = storage.resolve_message_token(&want).unwrap();
    let copies = storage
        .message_ids_by_content(&storage.message_by_id(&row).unwrap().unwrap().content_id)
        .unwrap();
    assert!(copies.contains(&row), "{copies:?}");
    assert_ne!(id.trim_matches(['<', '>']), row);

    let handle = mailspace.resolve_want(&id).unwrap().expect("a local copy");
    let resolved = storage.resolve_message_token(&handle).unwrap();
    assert!(copies.contains(&resolved), "{resolved} not in {copies:?}");
    assert_eq!(
        storage
            .message_by_id(&resolved)
            .unwrap()
            .unwrap()
            .normalized_message_id,
        Some(id.trim_matches(['<', '>']).to_string())
    );
    // Without the brackets, and as a plain handle, it finds a copy too.
    assert!(
        mailspace
            .resolve_want(id.trim_matches(['<', '>']))
            .unwrap()
            .is_some()
    );
    assert_eq!(mailspace.resolve_want(&want).unwrap(), Some(want));
}

#[test]
fn a_want_with_no_copy_here_resolves_to_none() {
    let (_tmp, mailspace) = space();
    assert_eq!(
        mailspace
            .resolve_want("<nobody-sent-this@example.test>")
            .unwrap(),
        None
    );
}
