#[tokio::test]
async fn dependency_actor_grant_and_for_override_work_through_recipient_handle() {
    let fx = super::fixture("unaddressed-dependency-actors");
    let ms = fx.ms();
    let work = super::library_need(&ms, "operator", "hand", "work");
    let release = super::library_need(&ms, "operator", "hand", "release");

    // A mind grant authorizes edits through the hand's copy.
    let mind = fx.key(Some("mind"));
    let added = fx
        .call(
            &mind,
            "depend",
            serde_json::json!({"handle": release, "add": work}),
        )
        .await;
    assert_eq!(added["changes"][0]["changed"], true);
    assert_eq!(ms.backlog_dependencies(&release).unwrap().len(), 1);

    // `for` selects the actor as well; the default operator grant can select
    // the authoring operator while still addressing the recipient's handle.
    let work2 = super::library_need(&ms, "operator", "hand", "work two");
    let operator = fx.key(None);
    let added = fx
        .call(
            &operator,
            "depend",
            serde_json::json!({"handle": release, "add": work2, "for": "operator"}),
        )
        .await;
    assert_eq!(added["changes"][0]["changed"], true);
    assert_eq!(ms.backlog_dependencies(&release).unwrap().len(), 2);
}

#[tokio::test]
async fn unaddressed_dependency_item_accepts_dependency_edit() {
    let fx = super::fixture("unaddressed-dependency-item");
    let ms = fx.ms();
    let dep = super::library_need(&ms, "operator", "hand", "work");
    let item = ms
        .send(vivi_core::mailspace::SendRequest {
            from: "operator".into(),
            to: vec![],
            cc: vec![],
            bcc: vec![],
            subject: "unaddressed".into(),
            body: "+++\ndone_when = [\"done\"]\n+++\n\nwork".into(),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap()
        .sent;
    let actor = fx.key(None);
    let result = fx
        .call(
            &actor,
            "depend",
            serde_json::json!({"handle": item, "add": dep}),
        )
        .await;
    assert_eq!(result["changes"][0]["changed"], true);
}
