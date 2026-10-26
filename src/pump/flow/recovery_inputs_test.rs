//! Tests for the recovery-input snapshot: the exclusions, per-ID eligibility
//! without a freshness boundary, and the visible date failure.

use std::sync::Arc;
use std::time::Duration;

use rusqlite::Connection;
use tempfile::tempdir;

use super::{checkpoint, eligible, replies};
use crate::error::VivariumError;
use crate::letter::fixture::record_body;
use crate::mailspace::MailspaceThreadMessage;
use crate::pump::config::PumpConfig;
use crate::pump::fixture::{Project, project};
use crate::pump::flow::mail::{MailWriter, MailspacePort};
use crate::pump::host::{Ctx, test_ctx};

type R = Result<(), VivariumError>;

/// Any third-party error as a [`VivariumError`], for `?` in a test.
fn other(error: impl std::fmt::Display) -> VivariumError {
    VivariumError::Other(error.to_string())
}

/// The pump's mailspace identity (`reply_from` of the test context).
const PUMP: &str = "operator";

struct Rig {
    p: Project,
    ctx: Ctx,
    port: MailspacePort,
    need: String,
}

fn rig() -> Result<Rig, VivariumError> {
    let p = project();
    let mut ctx = p.pump_ctx();
    let mut cfg = (*ctx.cfg).clone();
    cfg.reply_from = PUMP.into();
    ctx.cfg = Arc::new(cfg);
    let need = p.need("recover from what the thread said");
    let mut port = MailspacePort {
        mailspace: p.mailspace.clone(),
        pump_identity: PUMP.into(),
    };
    MailWriter::open_record(&mut port, &need, &record_body(&need, "build"))?;
    Ok(Rig { p, ctx, port, need })
}

impl Rig {
    /// A reply on the need's thread from `from`.
    fn reply(&self, from: &str, body: &str) -> R {
        self.p.mailspace.reply(
            &self.need,
            from,
            vec![PUMP.into()],
            Vec::new(),
            None,
            body.to_owned(),
        )?;
        Ok(())
    }

    /// A done lifecycle event letter on the need's thread.
    fn close_need(&self) -> R {
        self.p.mailspace.move_item_outcome(
            "mind",
            &self.need,
            "done",
            None,
            "closed by hand",
            None,
        )?;
        Ok(())
    }

    fn ids(&self) -> Result<Vec<String>, VivariumError> {
        eligible(&self.p.mailspace, PUMP, &self.need)
    }

    /// One `handle|subject|kind|event` line per raw thread message.
    fn dump(&self) -> Result<String, VivariumError> {
        let lines = self
            .p
            .mailspace
            .thread(&self.need, false, usize::MAX, usize::MAX)?
            .into_iter()
            .map(|message| {
                format!(
                    "{}|{}|{}|{:?}",
                    message.handle,
                    message.subject,
                    message.kind.as_deref().unwrap_or(""),
                    message.event
                )
            })
            .collect::<Vec<_>>();
        Ok(lines.join("\n"))
    }
}

/// A synthetic thread message with the given facts and a parsable date.
fn letter(
    handle: &str,
    from: &str,
    subject: &str,
    kind: Option<&str>,
    event: Option<String>,
) -> MailspaceThreadMessage {
    MailspaceThreadMessage {
        handle: handle.to_owned(),
        message_id: format!("<{handle}@test>"),
        content_id: format!("c-{handle}"),
        account: String::new(),
        role: String::new(),
        kind: kind.map(str::to_owned),
        event,
        date: "2026-01-01T00:00:00Z".to_owned(),
        from: from.to_owned(),
        to: PUMP.to_owned(),
        cc: String::new(),
        subject: subject.to_owned(),
        body: String::new(),
        parent_content_id: None,
        link_source: None,
        inferred: false,
        marks: Vec::new(),
        close_branch: None,
        close_commit: None,
    }
}

#[test]
fn only_member_replies_are_eligible_input() -> R {
    let rig = rig()?;
    rig.reply("elsewhere", "narrow the scope")?;
    let first = rig.ids()?;
    assert_eq!(first.len(), 1, "{}", rig.dump()?);

    // Mail the pump authored is not input.
    rig.reply(PUMP, "pump: run r1 started")?;
    assert_eq!(rig.ids()?, first);

    rig.reply("mind", "a second member reply")?;
    let both = rig.ids()?;
    assert_eq!(both.len(), 2, "{}", rig.dump()?);
    assert!(both.contains(&first[0]));
    Ok(())
}

#[test]
fn a_done_event_letter_is_not_a_reply() -> R {
    let rig = rig()?;
    rig.reply("elsewhere", "the real input")?;
    rig.close_need()?;

    let dump = rig.dump()?;
    assert!(dump.contains("Some(\"done\")"), "{dump}");
    let ids = rig.ids()?;
    assert_eq!(ids.len(), 1, "{dump}");
    Ok(())
}

#[test]
fn a_record_an_action_a_pump_letter_and_the_root_are_not_replies() -> R {
    let rig = rig()?;
    let thread = vec![
        letter(&rig.need, "operator", "the need", Some("need"), None),
        letter("m1", "elsewhere", "Re: the need", None, None),
        letter(
            "r1",
            PUMP,
            &format!("pump {}: plan", rig.need),
            Some("pump"),
            None,
        ),
        letter(
            "a1",
            "operator",
            &format!("pump {}: replan action", rig.need),
            None,
            None,
        ),
        letter("p1", PUMP, "pump: run r1 started", None, None),
        letter("e1", "mind", "Re: the need", None, Some("done".into())),
    ];
    let ids = replies(&rig.p.mailspace, PUMP, &rig.need, &thread)?;
    assert_eq!(ids, vec!["<m1@test>".to_owned()]);
    Ok(())
}

#[test]
fn eligibility_follows_ids_not_the_clock() -> R {
    let rig = rig()?;
    rig.reply("elsewhere", "before the capture")?;
    let first = checkpoint(&rig.ctx, &rig.need)?;
    assert_eq!(first, serde_json::to_string(&rig.ids()?).map_err(other)?);

    // A reply captured in the same second is still new input: eligibility is
    // per message ID, with no one-second freshness boundary to fall foul of.
    rig.reply("mind", "a reply in the same second")?;
    let both = checkpoint(&rig.ctx, &rig.need)?;
    assert_ne!(first, both);
    let ids: Vec<String> = serde_json::from_str(&both).map_err(other)?;
    assert_eq!(ids.len(), 2, "{both}");
    Ok(())
}

#[test]
fn a_corrupt_thread_date_fails_the_capture_visibly() -> R {
    let rig = rig()?;
    rig.reply("elsewhere", "the input")?;
    let db = rig.p.root().join(".vivi").join("mail.sqlite");
    let corrupt = Connection::open(&db).map_err(other)?;
    corrupt
        .busy_timeout(Duration::from_secs(5))
        .map_err(other)?;
    corrupt
        .execute("UPDATE message_metadata SET date = 'not-a-date'", [])
        .map_err(other)?;
    drop(corrupt);

    let error = rig.ids().expect_err("the capture fails");
    assert!(error.to_string().contains("thread date"), "{error}");
    Ok(())
}

#[test]
fn a_project_without_a_mailspace_snapshots_to_the_empty_checkpoint() -> R {
    let dir = tempdir().expect("tempdir");
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    assert_eq!(checkpoint(&ctx, "n1")?, "[]");
    Ok(())
}
