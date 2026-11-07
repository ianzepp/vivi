//! A need's repository target: sent, refused, and retargeted.

use std::fs;
use std::path::Path;

use crate::mailspace::{DeliveryResult, Mailspace, SendRequest};
use vivi_error::VivariumError;
use vivi_mail::letter::sugar::{self, Sugar};

/// A project declaring exactly `targets` as its pump targets, with the
/// `ceo`/`cto` roster. No target table at all leaves the pump unconfigured.
fn project(targets: &[&str]) -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    Mailspace::init(Some(tmp.path())).unwrap();
    if !targets.is_empty() {
        add_targets(tmp.path(), targets);
    }
    let mut mailspace = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    for identity in ["ceo", "cto"] {
        mailspace.add_identity(identity).unwrap();
    }
    (tmp, mailspace)
}

/// Append one `[pump.targets.<id>]` table per id, under a `[pump]` table.
fn add_targets(root: &Path, targets: &[&str]) {
    let path = root.join(".vivi/mailspace.toml");
    let mut raw = fs::read_to_string(&path).unwrap();
    if !raw.contains("[pump]") {
        raw.push_str("\n[pump]\n");
    }
    for id in targets {
        raw.push_str("\n[pump.targets.");
        raw.push_str(id);
        raw.push_str("]\npath = \"");
        raw.push_str(id);
        raw.push_str("\"\n");
    }
    fs::write(&path, raw).unwrap();
}

/// Send a need from `ceo` to `cto` with the given front matter target.
fn send_need(mailspace: &Mailspace, target: Option<&str>) -> Result<DeliveryResult, VivariumError> {
    let sugar = Sugar {
        done_when: vec![String::from("it works")],
        target: target.map(str::to_string),
        ..Sugar::default()
    };
    let body = sugar::apply("need", "the prose", &sugar).unwrap().body;
    mailspace.send(SendRequest {
        from: "ceo".into(),
        to: vec!["cto".into()],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: String::from("a need"),
        body,
        role: "needs".into(),
        kind: Some("need".into()),
        reply_to: None,
    })
}

/// The handle of the recipient's copy, which is the need the roster holds.
fn need_handle(result: &DeliveryResult) -> String {
    result
        .delivered
        .first()
        .map_or_else(|| result.sent.clone(), |copy| copy.handle.clone())
}

#[test]
fn an_unknown_send_target_is_refused_with_the_valid_list() {
    let (_tmp, mailspace) = project(&["alpha", "beta"]);
    let err = send_need(&mailspace, Some("gamma")).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("gamma"), "{text}");
    assert!(text.contains("alpha, beta"), "{text}");
    let listed = mailspace.list_kind(None, "needs", "need").unwrap();
    assert!(listed.is_empty(), "a refused need stores nothing");
}

#[test]
fn a_send_without_a_target_takes_the_primary_target() {
    let (_tmp, mailspace) = project(&["alpha"]);
    let result = send_need(&mailspace, None).unwrap();
    let target = mailspace.need_target(&need_handle(&result)).unwrap();
    assert_eq!(target.as_deref(), Some("alpha"));
}

#[test]
fn several_targets_with_no_primary_refuse_a_send_without_a_target() {
    let (_tmp, mailspace) = project(&["alpha", "beta"]);
    let err = send_need(&mailspace, None).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("no primary"), "{text}");
    assert!(text.contains("alpha, beta"), "{text}");
}

#[test]
fn the_explicit_default_target_is_the_primary_one() {
    let (_tmp, mailspace) = project(&["default", "beta"]);
    let result = send_need(&mailspace, None).unwrap();
    let target = mailspace.need_target(&need_handle(&result)).unwrap();
    assert_eq!(target.as_deref(), Some("default"));
}

#[test]
fn a_legacy_pump_without_explicit_targets_stores_no_target() {
    let (_tmp, mailspace) = project(&[]);
    let result = send_need(&mailspace, None).unwrap();
    assert_eq!(mailspace.need_target(&need_handle(&result)).unwrap(), None);
}

#[test]
fn retarget_changes_the_reported_target_and_refuses_an_unknown_one() {
    let (_tmp, mailspace) = project(&["default", "beta"]);
    let handle = need_handle(&send_need(&mailspace, None).unwrap());
    assert_eq!(
        mailspace.need_target(&handle).unwrap().as_deref(),
        Some("default")
    );

    let written = mailspace
        .retarget_need(None, &handle, "beta", Some("the work moved"))
        .unwrap();
    assert_eq!(written.target, "beta");
    assert!(written.changed);
    assert_eq!(
        mailspace.need_target(&handle).unwrap().as_deref(),
        Some("beta")
    );

    let err = mailspace
        .retarget_need(None, &handle, "gamma", None)
        .unwrap_err();
    assert!(err.to_string().contains("gamma"), "{err}");
    assert_eq!(
        mailspace.need_target(&handle).unwrap().as_deref(),
        Some("beta"),
        "a refused retarget leaves the target alone"
    );
}

#[test]
fn the_newest_retarget_wins() {
    let (_tmp, mailspace) = project(&["default", "beta", "gamma"]);
    let handle = need_handle(&send_need(&mailspace, None).unwrap());
    mailspace
        .retarget_need(None, &handle, "beta", None)
        .unwrap();
    mailspace
        .retarget_need(None, &handle, "gamma", None)
        .unwrap();
    assert_eq!(
        mailspace.need_target(&handle).unwrap().as_deref(),
        Some("gamma")
    );
    mailspace
        .retarget_need(None, &handle, "default", None)
        .unwrap();
    assert_eq!(
        mailspace.need_target(&handle).unwrap().as_deref(),
        Some("default")
    );
}

#[test]
fn retargeting_to_the_target_the_need_has_writes_nothing() {
    let (_tmp, mailspace) = project(&["default", "beta"]);
    let handle = need_handle(&send_need(&mailspace, None).unwrap());
    let written = mailspace
        .retarget_need(None, &handle, "default", None)
        .unwrap();
    assert!(!written.changed);
    assert!(written.event.is_none());
    assert!(mailspace.thread(&handle, false, 50, 50).unwrap().len() == 1);
}
