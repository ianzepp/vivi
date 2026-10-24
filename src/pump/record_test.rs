//! Tests for [`crate::pump::record`]: how a dispatch's pump record is closed
//! (`landed`, `gave-up`, `withdrawn`), driven against a real
//! mailspace so the records are the ones `vivi pump list` shows.

use std::path::Path;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::*;
use crate::mailspace::{Mailspace, PumpRecord, SendRequest};
use crate::pump::config::PumpConfig;
use crate::pump::host::{FakeHost, test_ctx};

/// A context whose project is a real mailspace with the `operator` and
/// `mind` identities, plus the mailspace and one open need.
struct World {
    _dir: TempDir,
    ctx: Ctx,
    host: Arc<FakeHost>,
    mailspace: Mailspace,
    need: String,
}

fn world() -> World {
    let dir = tempdir().unwrap();
    let project = dir.path().join("project");
    let mut mailspace = Mailspace::init(Some(&project)).unwrap();
    mailspace.add_identity("operator").unwrap();
    mailspace.add_identity("mind").unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let need = send_need(&mailspace, "fix auth");
    World {
        _dir: dir,
        ctx,
        host,
        mailspace,
        need,
    }
}

fn send_need(mailspace: &Mailspace, subject: &str) -> String {
    mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: crate::letter::fixture::need_body(&["it works"], ""),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

fn history(world: &World) -> Vec<PumpRecord> {
    world.mailspace.pump_history(&world.need).unwrap()
}

#[test]
fn finish_closes_only_the_open_record_and_a_second_call_is_a_no_op() {
    let world = world();
    let first = world.mailspace.pump_send(None, &world.need, None).unwrap();
    world
        .mailspace
        .pump_done(&first.handle, Some("gave-up"))
        .unwrap();
    assert_eq!(finish(&world.ctx, &world.need, "withdrawn", ""), None);
    let second = world.mailspace.pump_send(None, &world.need, None).unwrap();

    assert_eq!(
        finish(&world.ctx, &world.need, "gave-up", "").as_deref(),
        Some(second.handle.as_str())
    );
    assert_eq!(finish(&world.ctx, &world.need, "gave-up", ""), None);

    let outcomes: Vec<_> = history(&world)
        .into_iter()
        .map(|record| record.outcome)
        .collect();
    assert_eq!(
        outcomes,
        vec![Some("gave-up".to_owned()), Some("gave-up".to_owned())]
    );
}

#[test]
fn finish_replies_the_detail_on_the_record_before_closing_it() {
    let world = world();
    let record = world.mailspace.pump_send(None, &world.need, None).unwrap();
    world.host.reply(
        "pump list --status open --json",
        0,
        &format!(
            "[{{\"handle\": \"{}\", \"need\": \"{}\", \"status\": \"open\"}}]",
            record.handle, world.need
        ),
    );

    finish(&world.ctx, &world.need, "gave-up", "unit u1 is stuck");

    let commands = world.host.commands();
    assert!(
        commands
            .iter()
            .any(|cmd| cmd.contains(&format!("mail reply {} --from mind", record.handle))),
        "{commands:?}"
    );
}

#[test]
fn the_open_lookups_read_the_cli_list() {
    let world = world();
    world.host.reply(
        "pump list --status open --json",
        0,
        "noise\n[{\"handle\": \"p1\", \"need\": \"n1\"}, {\"handle\": \"p2\", \"need\": \"n2\"}]",
    );
    let needs = open_need_handles(&world.ctx).unwrap();
    assert_eq!(needs, ["n1".to_owned(), "n2".to_owned()].into());
    world.host.reply(
        "pump list --status open --json --need n2",
        0,
        "[{\"handle\": \"p2\", \"need\": \"n2\"}]",
    );
    assert_eq!(open_handle(&world.ctx, "n2").as_deref(), Some("p2"));
    world.host.reply("pump list", 1, "error");
    assert_eq!(open_need_handles(&world.ctx), None);
    assert_eq!(open_handle(&world.ctx, "n2"), None);
}

#[test]
fn a_context_without_a_mailspace_closes_nothing_and_does_not_fail() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    assert!(!Path::new(&ctx.project).exists());
    assert_eq!(finish(&ctx, "n1", "withdrawn", "x"), None);
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("pump records unavailable")),
        "{:?}",
        host.logs()
    );
}

/// The `mail reply` commands the fake host saw for `target`.
fn reply_commands(world: &World, target: &str) -> Vec<String> {
    let needle = format!("mail reply {target} ");
    world
        .host
        .commands()
        .into_iter()
        .filter(|cmd| cmd.contains(&needle))
        .collect()
}

#[test]
fn a_reply_on_a_pump_to_pump_task_always_names_the_pump_identity() {
    let world = world();

    assert!(reply_on_task(&world.ctx, "t1", "t1", "brief", "body"));

    let sent = reply_commands(&world, "t1");
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].contains("--from mind --to mind"), "{}", sent[0]);
}

#[test]
fn a_reply_on_a_pump_to_pump_task_ignores_board_reply_to() {
    let mut cfg = PumpConfig::default();
    cfg.board.reply_to = Some(String::from("operator"));
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), cfg);

    assert!(reply_on_task(&ctx, "t1", "t1", "brief", "body"));
    assert!(reply_on(&ctx, "t2", "t2", "brief", "body"));

    let commands = host.commands();
    let task = commands
        .iter()
        .find(|cmd| cmd.contains("mail reply t1 "))
        .unwrap();
    assert!(task.contains("--to mind"), "{task}");
    assert!(!task.contains("operator"), "{task}");
    let record = commands
        .iter()
        .find(|cmd| cmd.contains("mail reply t2 "))
        .unwrap();
    assert!(record.contains("--to operator"), "{record}");
}

#[test]
fn a_reply_on_a_record_keeps_leaving_the_recipient_to_the_thread() {
    let world = world();

    assert!(reply_on(&world.ctx, "p1", "n1", "planned", "body"));

    let sent = reply_commands(&world, "p1");
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(!sent[0].contains("--to"), "{}", sent[0]);
}
