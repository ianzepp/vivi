//! Tests for filing or replying on the repair need when the main rung is red.

use std::collections::BTreeSet;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::{find_repair_need, report_red};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::mail::{MailReader, MailWriter};
use crate::pump::flow::main_rung::{FAILING, key};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    host: Arc<FakeHost>,
    store: PumpStore,
    mail: FakeMail,
    need: String,
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("pump");
    // The fake can only reply on a handle it knows: the filed need is a task.
    let need = mail.send_step("pump", "pump", "repair", "need", &[])?;
    host.reply("need send", 0, &format!("created need {need}\n"));
    Ok(Rig {
        need,
        _dir: dir,
        ctx,
        host,
        store,
        mail,
    })
}

fn ids(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

impl Rig {
    fn red(&mut self, target: &str, failing: &[&str]) -> R {
        report_red(
            &self.ctx,
            &self.store,
            &mut self.mail,
            target,
            "abcdef0123456789",
            &ids(failing),
            "tail text",
        )
    }

    fn sends(&self) -> Vec<String> {
        self.host
            .commands()
            .into_iter()
            .filter(|c| c.contains("need send"))
            .collect()
    }

    fn replies(&self) -> Vec<(String, String)> {
        self.mail
            .calls
            .iter()
            .filter_map(|call| match call {
                Call::Reply { handle, body } => Some((handle.clone(), body.clone())),
                _ => None,
            })
            .collect()
    }
}

#[test]
fn first_red_files_one_repair_need_and_opens_its_record() -> R {
    let mut rig = rig()?;
    rig.red("default", &["a::t1"])?;
    let sends = rig.sends();
    assert_eq!(sends.len(), 1);
    assert!(sends[0].contains("--mark repair"), "{}", sends[0]);
    let (_, body) = MailReader::open_record(&rig.mail, &rig.need)?
        .ok_or_else(|| VivariumError::Message("no record".into()))?;
    assert!(body.contains("kind: repair"), "{body}");
    assert!(body.contains("target: default"), "{body}");
    assert_eq!(
        rig.store.main_state(&key(FAILING, "default"))?,
        Some("[\"a::t1\"]".to_string())
    );
    Ok(())
}

#[test]
fn a_repeat_red_with_the_same_set_does_nothing() -> R {
    let mut rig = rig()?;
    rig.red("default", &["a::t1", "a::t2"])?;
    rig.red("default", &["a::t2", "a::t1"])?;
    assert_eq!(rig.sends().len(), 1);
    assert!(rig.replies().is_empty());
    Ok(())
}

#[test]
fn a_different_set_replies_on_the_open_need_and_files_nothing() -> R {
    let mut rig = rig()?;
    rig.red("default", &["a::t1"])?;
    rig.red("default", &["a::t1", "b::t9"])?;
    assert_eq!(rig.sends().len(), 1);
    let replies = rig.replies();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].0, rig.need);
    assert!(
        replies[0].1.starts_with("pump-main-red: t="),
        "{}",
        replies[0].1
    );
    assert!(replies[0].1.contains("abcdef0123456789"));
    assert!(replies[0].1.contains("b::t9"));
    assert!(replies[0].1.contains("tail text"));
    assert_eq!(
        rig.store.main_state(&key(FAILING, "default"))?,
        Some("[\"a::t1\",\"b::t9\"]".to_string())
    );
    Ok(())
}

#[test]
fn a_repair_need_for_another_target_is_not_used() -> R {
    let mut rig = rig()?;
    MailWriter::open_record(&mut rig.mail, "other", "kind: repair\ntarget: elsewhere\n")?;
    rig.red("default", &["a::t1"])?;
    assert_eq!(rig.sends().len(), 1);
    assert!(rig.replies().is_empty());
    assert_eq!(
        find_repair_need(&rig.mail, "default")?.as_deref(),
        Some(rig.need.as_str())
    );
    assert_eq!(
        find_repair_need(&rig.mail, "elsewhere")?.as_deref(),
        Some("other")
    );
    Ok(())
}

#[test]
fn find_repair_need_ignores_a_build_need_with_a_target_line() -> R {
    let mut rig = rig()?;
    MailWriter::open_record(&mut rig.mail, "build1", "kind: build\ntarget: default\n")?;
    MailWriter::open_record(&mut rig.mail, "build2", "target: default\n")?;
    assert_eq!(find_repair_need(&rig.mail, "default")?, None);
    Ok(())
}

#[test]
fn a_failed_need_send_is_an_error_and_stores_nothing() -> R {
    let mut rig = rig()?;
    rig.host.reply("need send", 1, "boom\n");
    assert!(rig.red("default", &["a::t1"]).is_err());
    assert_eq!(rig.store.main_state(&key(FAILING, "default"))?, None);
    Ok(())
}

impl Rig {
    fn alarms(&self, kind: &str) -> usize {
        std::fs::read_to_string(self.ctx.home.join("exceptions.txt"))
            .unwrap_or_default()
            .lines()
            .filter(|line| line.contains(kind))
            .count()
    }
}

#[test]
fn a_failed_open_record_resumes_on_the_same_need_without_a_second_send() -> R {
    let mut rig = rig()?;
    rig.mail.fail_at = Some(rig.mail.calls.len());
    assert!(rig.red("default", &["a::t1"]).is_err());
    assert_eq!(rig.sends().len(), 1);
    assert!(MailReader::open_record(&rig.mail, &rig.need)?.is_none());
    rig.red("default", &["a::t1"])?;
    assert_eq!(rig.sends().len(), 1);
    let (_, body) = MailReader::open_record(&rig.mail, &rig.need)?
        .ok_or_else(|| VivariumError::Message("no record".into()))?;
    assert!(body.contains("target: default"), "{body}");
    assert_eq!(
        rig.store.main_state(&key(FAILING, "default"))?,
        Some("[\"a::t1\"]".to_string())
    );
    assert_eq!(rig.store.main_state("main-repair-sending:default")?, None);
    Ok(())
}

#[test]
fn an_unparseable_send_output_raises_one_alarm_and_files_once() -> R {
    let mut rig = rig()?;
    rig.host.reply("need send", 0, "ok but no handle\n");
    rig.red("default", &["a::t1"])?;
    rig.red("default", &["a::t1"])?;
    assert_eq!(rig.sends().len(), 1);
    assert_eq!(rig.alarms("flow-repair-need-unknown"), 1);
    assert_eq!(rig.store.main_state(&key(FAILING, "default"))?, None);
    Ok(())
}

#[test]
fn a_closed_recorded_need_files_a_new_one() -> R {
    let mut rig = rig()?;
    rig.red("default", &["a::t1"])?;
    let first = rig.need.clone();
    rig.mail.close_need_by(&first, "pump");
    MailWriter::close_record(&mut rig.mail, &first, "landed")?;
    let second = rig.mail.send_step("pump", "pump", "repair2", "need", &[])?;
    rig.host
        .reply("need send", 0, &format!("created need {second}\n"));
    rig.red("default", &["a::t1"])?;
    assert_eq!(rig.sends().len(), 2);
    assert!(MailReader::open_record(&rig.mail, &second)?.is_some());
    assert_eq!(
        find_repair_need(&rig.mail, "default")?.as_deref(),
        Some(second.as_str())
    );
    Ok(())
}
