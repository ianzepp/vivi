use super::{fixture, library_need};
use serde_json::{Value, json};
use vivi_core::mailspace::SendRequest;

fn send(
    ms: &vivi_core::mailspace::Mailspace,
    from: &str,
    to: Vec<&str>,
    kind: &str,
    role: &str,
    subject: &str,
) -> String {
    ms.send(SendRequest {
        from: from.into(),
        to: to.into_iter().map(str::to_owned).collect(),
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: subject.into(),
        body: if kind == "task" {
            "+++\ndone_when = [\"done\"]\n+++\n\nbody".into()
        } else if kind == "want" {
            "+++\nquestion = \"help\"\n+++\n\nbody".into()
        } else {
            "body".into()
        },
        role: role.into(),
        kind: Some(kind.into()),
        reply_to: None,
    })
    .unwrap()
    .sent
}

fn subjects(value: &Value) -> Vec<&str> {
    value["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["subject"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn mcp_default_list_is_project_wide() {
    let fx = fixture("list-project-wide");
    let ms = fx.ms();
    let one = send(&ms, "operator", vec![], "task", "tasks", "unaddressed task");
    let two = send(&ms, "mind", vec![], "want", "wants", "unaddressed want");
    let addressed = library_need(&ms, "operator", "mind", "addressed");
    let key = fx.key(Some("hand"));

    let work = fx.call(&key, "list", json!({})).await;
    let titles = subjects(&work);
    assert!(titles.contains(&"unaddressed task"), "{work}");
    assert!(titles.contains(&"unaddressed want"), "{work}");
    assert!(titles.contains(&"addressed"), "{work}");
    assert_eq!(
        fx.call(&key, "list", json!({"kind":"task", "for":"operator"}))
            .await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        subjects(&fx.call(&key, "list", json!({"for":"all"})).await).contains(&"unaddressed want")
    );
    assert_eq!(
        fx.call(&key, "list", json!({"status":"done"})).await["items"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert!(!one.is_empty() && !two.is_empty() && !addressed.is_empty());

    let board = fx.call(&key, "board", json!({})).await;
    assert!(board["identities"].is_array(), "{board}");
}

#[tokio::test]
async fn mcp_unaddressed_mail_is_listed_without_sent_duplicates() {
    let fx = fixture("list-unaddressed-mail");
    let ms = fx.ms();
    send(&ms, "operator", vec![], "mail", "sent", "unaddressed mail");
    send(
        &ms,
        "operator",
        vec!["mind"],
        "mail",
        "sent",
        "ordinary sent",
    );
    send(
        &ms,
        "operator",
        vec!["mind"],
        "mail",
        "inbox",
        "delivered mail",
    );
    ms.save_memo("operator", "private memo", "body", &[])
        .unwrap();
    ms.save_memo("mind", "other memo", "body", &[]).unwrap();
    let key = fx.key(Some("operator"));

    let mail = fx.call(&key, "list", json!({"kind":"mail"})).await;
    assert!(subjects(&mail).contains(&"unaddressed mail"), "{mail}");
    assert!(subjects(&mail).contains(&"delivered mail"), "{mail}");
    assert!(!subjects(&mail).contains(&"ordinary sent"), "{mail}");
    assert_eq!(
        subjects(
            &fx.call(&key, "list", json!({"kind":"mail", "for":"mind"}))
                .await
        ),
        vec!["delivered mail"]
    );
    assert!(
        subjects(
            &fx.call(&key, "list", json!({"kind":"mail", "for":"all"}))
                .await
        )
        .contains(&"unaddressed mail")
    );

    let default_memos = fx.call(&key, "list", json!({"kind":"memo"})).await;
    assert_eq!(subjects(&default_memos), vec!["private memo"]);
    let explicit_memos = fx
        .call(&key, "list", json!({"kind":"memo", "for":"mind"}))
        .await;
    assert_eq!(subjects(&explicit_memos), vec!["other memo"]);
}
