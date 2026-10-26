//! Dispatch identity coverage lands here: the minted record keeps the
//! ordinary `pump send` sender and recipient — the configured `[pump] from`
//! and `[pump] reply_from`, by name, alias or local address — across Cc
//! copies and the delivered-copy resolution the dispatch requires, and never
//! inherits the need sender.
//!
//! The tests in this module reach the fixtures of the parent integration
//! test (`roster`, `vivi`, `ok`, `records`, `handle_after`, ...) through
//! `super::*`.

use super::*;
use serde_json::Value;

#[test]
fn dispatch_uses_configured_pump_identity_and_defaults() {
    let project = tempfile::tempdir().unwrap();
    let root = roster(project.path());
    ok(&vivi([
        "role",
        "add",
        "dispatcher",
        "--kind",
        "freeform",
        "--project",
        &root,
    ]));
    ok(&vivi([
        "role",
        "add",
        "intake",
        "--kind",
        "steward",
        "--project",
        &root,
    ]));
    // A non-default identity pair: `dispatcher` sends the record, `intake`
    // is the pump seat, `filer` only files the need.
    configure_pump(project.path(), "dispatcher", "intake");
    let domain = address_domain(project.path());

    let out = ok(&vivi([
        "need",
        "send",
        "--project",
        &root,
        "--from",
        "filer",
        "--to",
        "intake",
        "--subject",
        "fix auth",
        "--done-when",
        "it works",
        "--body",
        "please fix login",
        "--dispatch",
    ]));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 4, "unexpected receipts:\n{out}");
    let delivered = handle_after(&out, "created intake ");
    let sent = handle_after(&out, "sent ");
    assert_eq!(lines[0], format!("created intake {delivered}"));
    assert_eq!(lines[1], format!("sent {sent}"));
    assert_eq!(lines[2], format!("node {delivered}"));
    let record = handle_after(&out, "pump record ");
    assert_eq!(
        lines[3],
        format!("pump record {record} for need {delivered}")
    );

    // Exactly one open record, linked to the canonical need handle, sent by
    // the configured dispatcher to the configured intake seat — never the
    // need's sender (`filer`) or recipient-as-sender.
    let listed = records(&root);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0]["handle"], record.as_str());
    assert_eq!(listed[0]["kind"], "pump");
    assert_eq!(listed[0]["status"], "open");
    assert_eq!(listed[0]["need"], delivered.as_str());
    assert_eq!(listed[0]["from"], format!("dispatcher@{domain}"));
    assert_eq!(listed[0]["to"], format!("intake@{domain}"));
    assert_eq!(listed[0]["subject"], format!("pump {delivered}: fix auth"));
    let shown = shown_record(&root, &record);
    assert_default_dispatch_prose(&shown, &delivered);
    assert_no_pump_run(project.path());

    // An ordinary `pump send` with no overrides on a separate control need
    // (the dispatched need already carries its record) shows the same sender,
    // recipient and default prose.
    let control = ok(&vivi([
        "need",
        "send",
        "--project",
        &root,
        "--from",
        "filer",
        "--to",
        "intake",
        "--subject",
        "control need",
        "--done-when",
        "it works too",
        "--body",
        "control body",
    ]));
    assert!(!control.contains("pump record"), "{control}");
    let control_need = handle_after(&control, "created intake ");
    let control_sent = ok(&vivi([
        "pump",
        "send",
        "--need",
        &control_need,
        "--project",
        &root,
    ]));
    let control_record = handle_after(&control_sent, "pump record ");

    let listed = records(&root);
    assert_eq!(listed.len(), 2, "{listed:?}");
    let by_handle = |handle: &str, label: &str| {
        listed
            .iter()
            .find(|row| row["handle"] == handle)
            .unwrap_or_else(|| panic!("no {label} record {handle} in {listed:?}"))
    };
    let dispatched = by_handle(&record, "dispatched");
    let control_row = by_handle(&control_record, "control");
    assert_eq!(control_row["from"], dispatched["from"]);
    assert_eq!(control_row["to"], dispatched["to"]);
    assert_eq!(control_row["status"], "open");
    assert_eq!(control_row["need"], control_need.as_str());
    assert_eq!(
        control_row["subject"],
        format!("pump {control_need}: control need")
    );
    let control_shown = shown_record(&root, &control_record);
    assert_default_dispatch_prose(&control_shown, &control_need);

    // The filed need itself still carries the filer's own sender and body.
    let need_shown = ok(&vivi(["need", "show", &delivered, "--project", &root]));
    assert!(need_shown.contains("From: filer@"), "{need_shown}");
    assert!(need_shown.contains("please fix login"), "{need_shown}");
}

#[test]
fn dispatch_reaches_the_pump_through_cc_alias_and_local_addresses() {
    let project = tempfile::tempdir().unwrap();
    let root = roster(project.path());
    ok(&vivi([
        "role",
        "add",
        "dispatcher",
        "--kind",
        "freeform",
        "--project",
        &root,
    ]));
    ok(&vivi([
        "role",
        "add",
        "intake",
        "--kind",
        "steward",
        "--project",
        &root,
    ]));
    let domain = address_domain(project.path());
    let mut record_handles: Vec<String> = Vec::new();

    // The pump is reached only through Cc while To names `hand`, the
    // configured `[pump] to` seat: the delivered copy the dispatch requires
    // is the intake seat's Cc copy.
    configure_pump(project.path(), "dispatcher", "intake");
    let out = ok(&vivi([
        "need",
        "send",
        "--project",
        &root,
        "--from",
        "filer",
        "--to",
        "hand",
        "--cc",
        "intake",
        "--subject",
        "through cc",
        "--done-when",
        "it works",
        "--body",
        "please fix login",
        "--dispatch",
    ]));
    let cc_record = handle_after(&out, "pump record ");
    record_handles.push(cc_record.clone());
    let listed = records(&root);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0]["handle"], cc_record.as_str());
    assert_eq!(listed[0]["status"], "open");
    assert_eq!(listed[0]["from"], format!("dispatcher@{domain}"));
    assert_eq!(listed[0]["to"], format!("intake@{domain}"));
    let filed = filed_handles(&out);
    let cc_need = listed[0]["need"].as_str().unwrap().to_string();
    assert!(
        filed.iter().any(|handle| handle == &cc_need),
        "need {cc_need} is not a filed handle of:\n{out}"
    );

    // Renaming keeps the old name as an alias, and the dispatch resolves the
    // configured alias to the canonical seat: the record is addressed to
    // `frontdesk`, never to the configured `intake` string.
    ok(&vivi([
        "role",
        "rename",
        "intake",
        "frontdesk",
        "--project",
        &root,
    ]));
    configure_pump(project.path(), "dispatcher", "intake");
    let out = ok(&vivi(need_send_args(
        &root,
        "frontdesk",
        "alias reply from",
    )));
    let alias_record = handle_after(&out, "pump record ");
    record_handles.push(alias_record.clone());
    let shown = shown_record(&root, &alias_record);
    assert_eq!(shown["status"], "open");
    assert_eq!(shown["from"], format!("dispatcher@{domain}"));
    assert_eq!(shown["to"], format!("frontdesk@{domain}"));
    assert_default_dispatch_prose(&shown, shown["need"].as_str().unwrap());

    // The `name@local` form: both the configured reply seat and the
    // recipient may be bare-local addresses; the record still names the
    // canonical prefix-qualified address.
    configure_pump(project.path(), "dispatcher", "frontdesk@local");
    let out = ok(&vivi(need_send_args(
        &root,
        "frontdesk@local",
        "local address reply from",
    )));
    let local_record = handle_after(&out, "pump record ");
    record_handles.push(local_record.clone());
    let shown = shown_record(&root, &local_record);
    assert_eq!(shown["status"], "open");
    assert_eq!(shown["from"], format!("dispatcher@{domain}"));
    assert_eq!(shown["to"], format!("frontdesk@{domain}"));

    // The mailspace's own prefix-qualified addresses, on the configured
    // sender and reply seat alike, resolve to the same canonical record.
    configure_pump(
        project.path(),
        &format!("dispatcher@{domain}"),
        &format!("frontdesk@{domain}"),
    );
    let out = ok(&vivi(need_send_args(
        &root,
        "frontdesk",
        "prefix qualified reply from",
    )));
    let prefixed_record = handle_after(&out, "pump record ");
    record_handles.push(prefixed_record.clone());
    let shown = shown_record(&root, &prefixed_record);
    assert_eq!(shown["status"], "open");
    assert_eq!(shown["from"], format!("dispatcher@{domain}"));
    assert_eq!(shown["to"], format!("frontdesk@{domain}"));

    // Four dispatches, four distinct open records with distinct needs, and
    // no pump run or tick ever wrote a database.
    let listed = records(&root);
    assert_eq!(listed.len(), record_handles.len(), "{listed:?}");
    let mut needs: Vec<&str> = Vec::new();
    for row in &listed {
        assert_eq!(row["status"], "open", "{row:?}");
        assert!(
            record_handles
                .iter()
                .any(|handle| row["handle"] == handle.as_str()),
            "unexpected record {row:?}"
        );
        needs.push(row["need"].as_str().unwrap());
    }
    let distinct: Vec<&str> = needs.iter().fold(Vec::new(), |mut seen, need| {
        if !seen.contains(need) {
            seen.push(need);
        }
        seen
    });
    assert_eq!(distinct.len(), needs.len(), "a need carries two records");
    assert_no_pump_run(project.path());
}

/// The `need send --dispatch` arguments for one recipient, so every identity
/// form is filed from the same non-pump sender with the same body.
fn need_send_args<'a>(root: &'a str, to: &'a str, subject: &'a str) -> Vec<&'a str> {
    vec![
        "need",
        "send",
        "--project",
        root,
        "--from",
        "filer",
        "--to",
        to,
        "--subject",
        subject,
        "--done-when",
        "it works",
        "--body",
        "please fix login",
        "--dispatch",
    ]
}

/// Point the starter `[pump]` table's dispatch identities at `from` and
/// `reply_from`. The values are written raw — name, alias, `name@local` or
/// the prefix-qualified address — so the test exercises exactly the string
/// the dispatch must resolve; the next binary run rediscovers the edit.
fn configure_pump(project: &std::path::Path, from: &str, reply_from: &str) {
    let config = project.join(".vivi").join("mailspace.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| !(line.starts_with("from = ") || line.starts_with("reply_from = ")))
        .collect();
    let mut text = kept.join("\n");
    text.push('\n');
    let header = "[pump]\n";
    let at = text
        .find(header)
        .unwrap_or_else(|| panic!("no [pump] table in {}", config.display()));
    text.insert_str(
        at + header.len(),
        &format!("from = \"{from}\"\nreply_from = \"{reply_from}\"\n"),
    );
    std::fs::write(&config, text).unwrap();
}

/// The `<prefix>.local` domain this mailspace mints addresses in, read
/// through the same library call the binary makes, so no test hard-codes an
/// id. Read-only: a config edit after this stays invisible here and is
/// rediscovered by the next binary run.
fn address_domain(project: &std::path::Path) -> String {
    vivi::mailspace::Mailspace::discover(Some(project))
        .unwrap()
        .address_domain()
}

/// Every handle the receipts name as a copy of the filed letter: one per
/// `created` line (identity first, handle second) plus the sender's own
/// `sent` copy. The record's need is the content's canonical handle, one of
/// these.
fn filed_handles(output: &str) -> Vec<String> {
    let mut handles: Vec<String> = output
        .lines()
        .filter_map(|line| line.strip_prefix("created "))
        .map(|rest| {
            rest.split_whitespace()
                .nth(1)
                .unwrap_or_else(|| panic!("no handle after 'created <identity>' in:\n{output}"))
                .to_string()
        })
        .collect();
    handles.push(super::handle_after(output, "sent "));
    handles
}

/// One record, decoded from `pump show --json`.
fn shown_record(root: &str, handle: &str) -> Value {
    serde_json::from_str(&ok(&vivi([
        "pump",
        "show",
        handle,
        "--json",
        "--project",
        root,
    ])))
    .unwrap()
}

/// The record carries the default dispatch prose: the record's own front
/// matter and the `Dispatch of need` line, never the need sender's body.
fn assert_default_dispatch_prose(shown: &Value, need: &str) {
    assert_eq!(shown["need"], need);
    let body = shown["body"].as_str().unwrap();
    assert!(body.contains(&format!("need = \"{need}\"")), "{body}");
    assert!(
        body.contains(&format!("Dispatch of need {need}.")),
        "{body}"
    );
    assert!(!body.contains("please fix login"), "{body}");
}

/// No pump run or tick ever happened: the dispatch alone writes no database.
fn assert_no_pump_run(project: &std::path::Path) {
    let db = project.join(".vivi").join("pump").join("pump.db");
    assert!(!db.exists(), "{:?} exists", db);
}
