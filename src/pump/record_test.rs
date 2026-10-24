//! Tests for [`crate::pump::record`]: replies on a task the pump sent to
//! itself, driven against a real mailspace.

use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::*;
use crate::mailspace::Mailspace;
use crate::pump::config::PumpConfig;
use crate::pump::host::{FakeHost, test_ctx};

/// A context whose project is a real mailspace with the `operator` and
/// `mind` identities.
struct World {
    _dir: TempDir,
    ctx: Ctx,
    host: Arc<FakeHost>,
}

fn world() -> World {
    let dir = tempdir().unwrap();
    let project = dir.path().join("project");
    let mut mailspace = Mailspace::init(Some(&project)).unwrap();
    mailspace.add_identity("operator").unwrap();
    mailspace.add_identity("mind").unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    World {
        _dir: dir,
        ctx,
        host,
    }
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

    let commands = host.commands();
    let task = commands
        .iter()
        .find(|cmd| cmd.contains("mail reply t1 "))
        .unwrap();
    assert!(task.contains("--to mind"), "{task}");
    assert!(!task.contains("operator"), "{task}");
}
