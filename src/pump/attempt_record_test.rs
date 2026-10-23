//! Tests for [`crate::pump::attempt_record`]: the record posted on the need's
//! open pump record thread when a hand attempt ends, driven through
//! [`FakeHost`] replies.

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use crate::pump::attempt_record::{Ending, agent_exit, agent_secs, hand_message, post_on_task};
use crate::pump::board::board;
use crate::pump::config::{BoardConfig, DispatchConfig, Kind, PumpConfig};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobSpec, PoolJob};
use crate::pump::state::State;

#[test]
fn hand_message_reads_the_receipt_section_only() {
    let receipt = "summary\n\n--- receipt\nOUT-OF-SCOPE: docs/budget.toml\n\n--- stderr tail\nOUT-OF-SCOPE: secrets.txt";
    assert_eq!(hand_message(receipt), "OUT-OF-SCOPE: docs/budget.toml");
}

fn config() -> PumpConfig {
    PumpConfig {
        board: BoardConfig {
            from: vec![String::from("operator@x")],
            ..BoardConfig::default()
        },
        dispatch: DispatchConfig {
            kinds: BTreeMap::from([(String::from("plain"), Kind::default())]),
            default_kind: String::from("plain"),
            ..DispatchConfig::default()
        },
        ..PumpConfig::default()
    }
}

fn setup() -> (TempDir, Ctx, Arc<FakeHost>) {
    setup_with(config())
}

fn setup_with(cfg: PumpConfig) -> (TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), cfg);
    open_record(&host, "n1", "pn1");
    (dir, ctx, host)
}

/// Script the lookup of `need`'s open pump record: replies go to `record`.
fn open_record(host: &FakeHost, need: &str, record: &str) {
    host.reply(
        &format!("pump list --status open --json --need {need}"),
        0,
        &format!("[{{\"handle\": \"{record}\", \"need\": \"{need}\", \"status\": \"open\"}}]"),
    );
}

/// A task receipt as the pool writes it: summary lines, then sections.
fn receipt(exit: i32, message: &str, stderr: &str) -> String {
    format!(
        "pool result j1: finished\npool fly4  slot s1  model zai/glm\nfrom next  default\n\
         push: pushed unit/u-1-a1 abcdef0123\nagent exit {exit} in 12s, job 20s, started from 1234567890\n\n\
         --- receipt\n{message}\n\n--- stderr tail\n{stderr}\n\n--- runner log tail\nlog line"
    )
}

/// The `mail reply` commands sent to `target` (a record or a need handle).
fn mails_to(host: &FakeHost, target: &str) -> Vec<String> {
    let needle = format!("mail reply {target} ");
    host.commands()
        .into_iter()
        .filter(|cmd| cmd.contains(&needle))
        .collect()
}

/// The attempt records written for need `need`: file name and text.
fn records_for(ctx: &Ctx, need: &str) -> Vec<(String, String)> {
    let prefix = format!("reply-{need}-attempt-");
    let Ok(dir) = fs::read_dir(ctx.home.join("board")) else {
        return Vec::new();
    };
    let mut found: Vec<(String, String)> = dir
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let text = fs::read_to_string(entry.path()).ok()?;
            name.starts_with(&prefix).then_some((name, text))
        })
        .collect();
    found.sort();
    found
}

#[test]
fn the_board_pass_ships_posted_records_with_a_sync_push() {
    let mut cfg = config();
    cfg.board.every = 0;
    cfg.board.pull = false;
    let (_dir, ctx, host) = setup_with(cfg);
    let mut st = State::default();
    st.board.sync_pending = true;
    board(&ctx, &mut st);
    assert!(!st.board.sync_pending);
    assert!(host.spawned().iter().any(|cmd| cmd.contains("sync push")));
    board(&ctx, &mut st);
    assert_eq!(host.spawned().len(), 1, "shipped once");
}

#[test]
fn the_agent_exit_comes_from_the_pool_summary_line() {
    assert_eq!(agent_exit(&receipt(3, "m", "")), Some(3));
    assert_eq!(agent_exit(&receipt(0, "m", "")), Some(0));
    assert_eq!(agent_exit("agent exit 124 after 600s"), Some(124));
    assert_eq!(agent_exit("no summary here"), None);
}

#[test]
fn the_agent_seconds_come_from_the_pool_summary_line() {
    assert_eq!(agent_secs(&receipt(3, "m", "")), Some(12));
    assert_eq!(agent_secs("agent exit 124 after 600s"), Some(600));
    assert_eq!(agent_secs("no summary here"), None);
}

#[test]
fn a_task_record_carries_the_jobs_attempt_pool_and_model() {
    let (_dir, ctx, _host) = setup();
    let mut spec = JobSpec::new("slice-t9-a2", "hand", "hand", "s", "b");
    spec.model = Some(String::from("zai/glm"));
    spec.target = Some(String::from("vivi"));
    spec.branch = Some(String::from("vivi/unit/t9-a2"));
    let mut job = PoolJob::from(spec);
    job.pool = Some(String::from("fly4"));
    job.payload = serde_json::json!({"attempt": 2});
    let ending = Ending::failed("hand failed", "", None);
    assert!(post_on_task(
        &ctx,
        "t9",
        &job,
        &ending,
        &receipt(1, "m", "")
    ));
    let records = records_for(&ctx, "t9");
    assert_eq!(records.len(), 1, "{records:?}");
    let text = &records[0].1;
    assert!(text.contains("attempt: t9-a2\n"), "{text}");
    assert!(text.contains("pool: fly4\n"), "{text}");
    assert!(
        text.contains("unit: t9  attempt: 2  model: zai/glm  pool: fly4  target: vivi"),
        "{text}"
    );
}

#[test]
fn a_task_record_is_posted_to_the_pump_identity() {
    let (_dir, ctx, host) = setup();
    let job = PoolJob::from(JobSpec::new("slice-t9-a1", "hand", "hand", "s", "b"));
    let ending = Ending::failed("hand failed", "", None);

    assert!(post_on_task(
        &ctx,
        "t9",
        &job,
        &ending,
        &receipt(1, "m", "")
    ));

    let sent = mails_to(&host, "t9");
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].contains("--from mind --to mind"), "{}", sent[0]);
}
