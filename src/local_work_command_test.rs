//! The `vivi need send --dispatch` chain: marks first, then a guarded pump
//! record filed from the just-sent need.

use super::*;
use vivi::cli::{LocalSendCommand, NeedSendCommand};
use vivi::mailspace::PumpState;
use vivi::pump::config::PumpConfig;

/// A mailspace with the roles a default pump configuration names, plus
/// `cedar` as a bystander recipient.
fn rig() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    for name in ["mind", "operator", "hand", "cedar"] {
        mailspace.add_identity(name).unwrap();
    }
    (tmp, mailspace)
}

/// A `need send --dispatch` command over this project, with no marks.
fn dispatch_command(project: &std::path::Path) -> NeedSendCommand {
    NeedSendCommand {
        send: LocalSendCommand {
            project: Some(project.to_path_buf()),
            from: "operator".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "fix the gate".into(),
            body: Some("please".into()),
            body_file: None,
            reply_to: None,
        },
        depends_on: Vec::new(),
        marks: Vec::new(),
        done_when: vec!["the gate opens".into()],
        target: None,
        write_scope: Vec::new(),
        serves: None,
        explores: None,
        dispatch: true,
        priority: None,
    }
}

/// File one need from `from` to `to` and return the sender's own copy
/// handle, with a valid `done_when` front matter built by `sugar::apply`.
fn file_need(mailspace: &Mailspace, from: &str, to: &[&str]) -> DeliveryResult {
    let sugared = sugar::apply(
        "need",
        "please",
        &Sugar {
            done_when: vec!["the gate opens".into()],
            ..Default::default()
        },
    )
    .unwrap();
    mailspace
        .send(SendRequest {
            from: from.into(),
            to: to.iter().map(|name| (*name).to_string()).collect(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "fix the gate".into(),
            body: sugared.body,
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap()
}

/// Append a minimal `[pump]` table to the on-disk config; every other
/// setting keeps its default, so `reply_from` is `mind`.
fn write_pump_table(project: &std::path::Path) {
    let path = project.join(".vivi/mailspace.toml");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("\n[pump]\ncommitter_name = \"Pump\"\ncommitter_email = \"pump@localhost\"\n");
    std::fs::write(path, text).unwrap();
}

#[test]
fn dispatch_chains_a_filed_need_into_one_open_record() {
    let (tmp, _mailspace) = rig();
    write_pump_table(tmp.path());

    // No marks: the empty mark list must not bypass the requested dispatch.
    let command = dispatch_command(tmp.path());
    send_need(&command).unwrap();

    let mailspace = Mailspace::discover(Some(tmp.path())).unwrap();
    let records = mailspace.pump_list(None, Some(PumpState::Open)).unwrap();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].status, PumpState::Open);
    // The record names the filed need: its history is reachable from the
    // record, which only works while the need itself is preserved.
    let need = records[0].need.clone();
    let history = mailspace.pump_history(&need).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].handle, records[0].handle);
    assert_eq!(history[0].need, need);
}

#[test]
fn dispatch_without_pump_configuration_is_refused_after_filing() {
    let (_tmp, mailspace) = rig();
    assert!(mailspace.config.pump.is_none());

    let result = file_need(&mailspace, "operator", &["mind"]);
    let error = dispatch_filed_need(&mailspace, &result).unwrap_err();
    assert!(error.to_string().contains("[pump]"), "{error}");

    // The need stays filed, but no record was minted.
    let history = mailspace.pump_history(&result.sent).unwrap();
    assert!(history.is_empty(), "{history:?}");
    let records = mailspace.pump_list(None, None).unwrap();
    assert!(records.is_empty(), "{records:?}");
}

#[test]
fn dispatch_without_a_local_copy_for_the_pump_identity_is_refused() {
    let (_tmp, mut mailspace) = rig();
    let mut pump = PumpConfig::default();
    pump.reply_from = "mind".into();
    mailspace.config.pump = Some(pump);

    // Only `cedar` receives a copy: `mind`, the configured `reply_from`,
    // never sees the need, so the dispatch is refused.
    let result = file_need(&mailspace, "operator", &["cedar"]);
    let error = dispatch_filed_need(&mailspace, &result).unwrap_err();
    assert!(error.to_string().contains("mind"), "{error}");
    assert!(
        error.to_string().contains(&result.sent),
        "the refusal names the filed need: {error}"
    );

    // The need stays filed, but no record was minted.
    let history = mailspace.pump_history(&result.sent).unwrap();
    assert!(history.is_empty(), "{history:?}");
    let records = mailspace.pump_list(None, None).unwrap();
    assert!(records.is_empty(), "{records:?}");
}
