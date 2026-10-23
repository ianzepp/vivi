//! Tests for filing or replying on the repair need when the main rung is red.

use std::collections::BTreeSet;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::{find_repair_need, report_red};
use crate::error::VivariumError;
use crate::letter::fixture::repair_record_body;
use crate::letter::read::record_of;
use crate::letter::records::{RecordFront, RecordKind};
use crate::pump::config::PumpConfig;
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::mail::{MailReader, MailWriter};
use crate::pump::flow::main_rung::{FAILING, REPAIRED, key};
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
    assert!(sends[0].contains(" --target default"), "{}", sends[0]);
    assert!(
        sends[0].contains("--done-when 'the main rung on default passes'"),
        "{}",
        sends[0]
    );
    let (_, body) = MailReader::open_record(&rig.mail, &rig.need)?
        .ok_or_else(|| VivariumError::Message("no record".into()))?;
    let front = record_of("", &body).unwrap().front;
    assert_eq!(front.need, rig.need);
    assert_eq!(front.kind, Some(RecordKind::Repair));
    assert_eq!(front.target.as_deref(), Some("default"));
    assert!(front.tested.is_some(), "{body}");
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
    MailWriter::open_record(
        &mut rig.mail,
        "other",
        &repair_record_body("other", "elsewhere", None, ""),
    )?;
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
    for (need, kind) in [("build1", Some(RecordKind::Build)), ("build2", None)] {
        let front = RecordFront {
            need: need.to_string(),
            kind,
            target: Some("default".to_string()),
            tested: None,
        };
        let body = crate::letter::render(&front, "").unwrap();
        MailWriter::open_record(&mut rig.mail, need, &body)?;
    }
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
    let front = record_of("", &body).unwrap().front;
    assert_eq!(front.target.as_deref(), Some("default"), "{body}");
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

#[test]
fn a_send_that_created_the_need_and_exited_nonzero_keeps_the_handle() -> R {
    let mut rig = rig()?;
    let created = format!("created need {}\n", rig.need);
    rig.host.reply("need send", 1, &created);
    rig.red("default", &["a::t1"])?;
    assert_eq!(
        rig.store.main_state("main-repair-need:default")?,
        Some(rig.need.clone())
    );
    rig.red("default", &["a::t1", "b::t9"])?;
    assert_eq!(rig.sends().len(), 1);
    let replies = rig.replies();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].0, rig.need);
    Ok(())
}

#[test]
fn a_nonzero_send_without_a_handle_names_the_expectation() -> R {
    let mut rig = rig()?;
    rig.host.reply("need send", 1, "boom\n");
    let err = rig
        .red("default", &["a::t1"])
        .err()
        .ok_or_else(|| VivariumError::Message("expected an error".into()))?;
    let text = err.to_string();
    assert!(
        text.contains("expected a created need handle; actual: exit 1: boom"),
        "{text}"
    );
    assert_eq!(rig.store.main_state("main-repair-sending:default")?, None);
    Ok(())
}

const MERGE: &str = "3333333333333333333333333333333333333333";
const OLD_TIP: &str = "2222222222222222222222222222222222222222";

impl Rig {
    fn red_at(&mut self, tip: &str, failing: &[&str]) -> R {
        report_red(
            &self.ctx,
            &self.store,
            &mut self.mail,
            "default",
            tip,
            &ids(failing),
            "tail text",
        )
    }

    fn repaired(&self) -> R {
        self.store.set_main_state(&key(REPAIRED, "default"), MERGE)
    }
}

#[test]
fn a_red_on_a_tip_without_the_repair_merge_files_nothing() -> R {
    let mut rig = rig()?;
    rig.repaired()?;
    rig.host.reply("cat-file -e", 0, "");
    rig.host.reply("merge-base --is-ancestor", 1, "");
    rig.red_at(OLD_TIP, &["a::t1"])?;
    assert!(rig.sends().is_empty());
    assert!(rig.replies().is_empty());
    assert_eq!(rig.store.main_state(&key(FAILING, "default"))?, None);
    Ok(())
}

#[test]
fn a_red_on_a_tip_with_the_repair_merge_files_a_need() -> R {
    let mut rig = rig()?;
    rig.repaired()?;
    rig.host.reply("cat-file -e", 0, "");
    rig.host.reply("merge-base --is-ancestor", 0, "");
    rig.red_at(OLD_TIP, &["a::t1"])?;
    assert_eq!(rig.sends().len(), 1);
    Ok(())
}

#[test]
fn an_error_on_the_ancestry_check_leaves_the_red_unapplied() -> R {
    let mut rig = rig()?;
    rig.repaired()?;
    rig.host.reply("cat-file -e", 1, "");
    let err = rig.red_at(OLD_TIP, &["a::t1"]).unwrap_err().to_string();
    assert!(err.contains(OLD_TIP) && err.contains(MERGE), "{err}");
    assert!(rig.sends().is_empty());
    Ok(())
}

#[test]
fn no_repaired_key_keeps_todays_behavior() -> R {
    let mut rig = rig()?;
    rig.red_at(OLD_TIP, &["a::t1"])?;
    assert_eq!(rig.sends().len(), 1);
    assert!(
        rig.host
            .commands()
            .iter()
            .all(|c| !c.contains("is-ancestor"))
    );
    Ok(())
}
