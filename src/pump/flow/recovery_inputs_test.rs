//! Tests for the recovery-input checkpoint on a real mailspace: the shared
//! exclusions, the visible date failure, and a reply that arrives after the
//! capture.

use crate::error::VivariumError;
use crate::mailspace::{MailspaceThreadMessage, PumpActionRequest};
use crate::pump::fixture::{Project, project, send_need};

use super::checkpoint;
use super::{capture, is_reply};

type R = Result<(), VivariumError>;

/// A need thread holding the letter kinds a capture meets: the root, a
/// member reply, a lifecycle event, and the pump's own mail. Only the member
/// reply is an eligible recovery input. The pump record and action letters
/// are separate threads, so [`is_reply`] covers them on their own letters.
fn thread_with_every_kind() -> Result<(Project, String), VivariumError> {
    let p = project();
    let need = send_need(&p.mailspace, "checkpoint inputs", "operator");
    p.mailspace.reply(
        &need,
        "mind",
        vec!["operator".into()],
        Vec::new(),
        None,
        "the member reply".into(),
    )?;
    // A lifecycle event letter: marking the need posts a marked event on the
    // same thread.
    p.mailspace
        .set_marks("operator", &need, "need", true, &["urgent".to_string()])?;
    // The pump's own mail on the thread.
    p.mailspace.reply(
        &need,
        "operator",
        vec!["mind".into()],
        Vec::new(),
        None,
        "the pump's own mail".into(),
    )?;
    Ok((p, need))
}

fn reply_of(thread: &[MailspaceThreadMessage], body: &str) -> MailspaceThreadMessage {
    thread
        .iter()
        .find(|m| m.body.contains(body))
        .expect("thread letter")
        .clone()
}

#[test]
fn only_member_replies_are_eligible_recovery_inputs() -> R {
    let (p, need) = thread_with_every_kind()?;
    let thread = p.mailspace.thread(&need, false, usize::MAX, usize::MAX)?;
    let snapshot = checkpoint(&p.mailspace, "operator", &thread)?;

    // The checkpoint holds exactly the member reply: never the root letter,
    // the lifecycle event, or the pump's own mail.
    let member = reply_of(&thread, "the member reply");
    assert_eq!(snapshot, member.message_id, "{snapshot}");
    for letter in thread {
        if letter.message_id != member.message_id {
            assert!(!snapshot.contains(&letter.message_id), "{snapshot}");
        }
    }
    Ok(())
}

#[test]
fn a_record_or_action_letter_is_never_a_reply() -> R {
    let p = project();
    let need = send_need(&p.mailspace, "checkpoint letters", "operator");
    let mut port = crate::pump::flow::mail::MailspacePort {
        mailspace: p.mailspace.clone(),
        pump_identity: "operator".into(),
    };
    crate::pump::flow::mail::MailWriter::open_record(&mut port, &need, "the pump record")?;
    let action = p
        .mailspace
        .pump_post_action(None, &need, &PumpActionRequest::replan())?;

    // Each letter, read on its own thread, is excluded by its letter shape.
    let mut letters = p
        .mailspace
        .thread(&action.record, false, usize::MAX, usize::MAX)?;
    let record = letters.remove(0);
    let action = letters
        .into_iter()
        .find(|m| m.subject.ends_with(" action"))
        .expect("action letter");
    assert_eq!(record.kind.as_deref(), Some("pump"));
    assert!(action.subject.starts_with("pump ") && action.subject.ends_with(" action"));
    assert!(!is_reply(&p.mailspace, "operator", &record));
    assert!(!is_reply(&p.mailspace, "operator", &action));
    // The need thread itself carries no eligible reply: nothing to capture.
    let thread = p.mailspace.thread(&need, false, usize::MAX, usize::MAX)?;
    assert_eq!(
        checkpoint(&p.mailspace, "operator", &thread)?,
        String::new()
    );
    Ok(())
}

#[test]
fn an_eligible_reply_with_a_broken_date_is_a_visible_error() -> R {
    let (p, need) = thread_with_every_kind()?;
    let mut thread = p.mailspace.thread(&need, false, usize::MAX, usize::MAX)?;
    let member = thread
        .iter_mut()
        .find(|m| m.body.contains("the member reply"))
        .expect("member reply");
    member.date = String::from("not a date");
    let error = checkpoint(&p.mailspace, "operator", &thread)
        .expect_err("a broken date must fail the snapshot");
    assert!(error.to_string().contains("thread date"), "{error}");
    Ok(())
}

#[test]
fn a_reply_after_the_capture_is_not_in_the_snapshot() -> R {
    let (p, need) = thread_with_every_kind()?;
    let captured = capture(&p.mailspace, "operator", &need)?;
    // The reply arrives after the execution's input capture; whether the step
    // later parks or not, it is not part of the captured input.
    p.mailspace.reply(
        &need,
        "mind",
        vec!["operator".into()],
        Vec::new(),
        None,
        "a reply after the capture".into(),
    )?;
    let thread = p.mailspace.thread(&need, false, usize::MAX, usize::MAX)?;
    let late = reply_of(&thread, "a reply after the capture");
    assert!(!captured.is_empty());
    assert!(!captured.contains(&late.message_id), "{captured}");
    let now = capture(&p.mailspace, "operator", &need)?;
    assert!(now.contains(&late.message_id), "{now}");
    Ok(())
}
