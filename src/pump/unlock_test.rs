//! Tests for [`super::unlock`]: it posts an unlock on the open pump record
//! and returns that record. It does not print.

use std::collections::BTreeSet;

use crate::mailspace::PumpActionKind;
use crate::pump::fixture::project;
use crate::pump::unlock::unlock;

#[test]
fn unlock_returns_the_record_and_entry_it_posted() {
    let project = project();
    let need = project.need("fix auth");
    let record = project.mailspace.pump_send(None, &need, None).unwrap();

    let posted = unlock(project.root(), &need, Some("slice"), Some("fixed it")).unwrap();

    assert_eq!(posted.need, need);
    assert_eq!(posted.record, record.handle);
    assert_eq!(posted.entry.as_deref(), Some("slice"));
    assert_eq!(
        posted.to_string(),
        format!(
            "unlock posted on {} for need {need}; the pump applies it at its next tick",
            record.handle
        )
    );
    let pending = project
        .mailspace
        .pump_pending_actions(&BTreeSet::new())
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, posted.id);
    assert_eq!(pending[0].need, need);
    assert_eq!(pending[0].request.kind, PumpActionKind::Unlock);
    assert_eq!(pending[0].request.word.as_deref(), Some("slice"));
    assert_eq!(pending[0].request.reason, "fixed it");
}

#[test]
fn unlock_without_an_entry_posts_no_word() {
    let project = project();
    let need = project.need("fix auth");
    project.mailspace.pump_send(None, &need, None).unwrap();

    let posted = unlock(project.root(), &need, None, None).unwrap();

    assert_eq!(posted.entry, None);
    let pending = project
        .mailspace
        .pump_pending_actions(&BTreeSet::new())
        .unwrap();
    assert_eq!(pending[0].request.word, None);
}

#[test]
fn unlock_without_a_record_is_refused() {
    let project = project();
    let need = project.need("fix auth");

    let error = unlock(project.root(), &need, None, None).unwrap_err();

    assert!(error.to_string().contains("no open pump record"), "{error}");
    assert!(
        project
            .mailspace
            .pump_pending_actions(&BTreeSet::new())
            .unwrap()
            .is_empty()
    );
}
