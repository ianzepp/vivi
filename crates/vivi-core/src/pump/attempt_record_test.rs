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
fn a_task_record_is_an_attempt_note_labelled_with_the_ending() {
    let (_dir, ctx, _host) = setup();
    let job = PoolJob::from(JobSpec::new("slice-t9-a1", "hand", "hand", "s", "b"));
    let ending = Ending::failed("hand failed", "", None);
    assert!(post_on_task(
        &ctx,
        "t9",
        &job,
        &ending,
        &receipt(1, "m", "")
    ));
    let records = records_for(&ctx, "t9");
    let note = crate::mailspace::pump_note::note_of(&records[0].1).expect("a pump note");
    assert_eq!(
        note.pump.note,
        vivi_mail::letter::records::NoteWord::Attempt
    );
    assert!(note.pump.reference.is_some_and(|label| !label.is_empty()));
    assert!(
        note.prose.contains("pump: attempt record"),
        "{}",
        note.prose
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

// ---- the failed-attempt record body -----------------------------------------
//
// `slice_dispatch::post_failed_attempt` builds `Ending::failed(why, detail,
// receipt)` and calls `post_on_task` with the same receipt as the facts text;
// these tests drive that pair with each `why` it uses.

/// Post the record of a failed attempt on task `t9` the way the slice flow
/// does, and return the one record body posted.
fn failed_record(why: &str, detail: &str, receipt: Option<&str>) -> String {
    let (_dir, ctx, _host) = setup();
    let mut job = PoolJob::from(JobSpec::new("slice-t9-a1", "hand", "hand", "s", "b"));
    job.payload = serde_json::json!({"attempt": 1});
    let ending = Ending::failed(why, detail, receipt);
    assert!(post_on_task(
        &ctx,
        "t9",
        &job,
        &ending,
        receipt.unwrap_or_default()
    ));
    let records = records_for(&ctx, "t9");
    assert_eq!(records.len(), 1, "{records:?}");
    records[0].1.clone()
}

#[test]
fn a_hand_failure_record_names_the_exit_the_stderr_excerpt_and_the_final_message() {
    let text = failed_record(
        "hand failed",
        "finished/pushed unit/u-1-a1 abc",
        Some(&receipt(
            1,
            "I gave up on the borrow.",
            "error[E0308]: mismatched types",
        )),
    );
    assert!(
        text.contains("outcome: agent failed: hand failed\n"),
        "{text}"
    );
    assert!(
        text.contains(
            "failing command:\n  agent exit 1\nexcerpt:\n  error[E0308]: mismatched types\n"
        ),
        "{text}"
    );
    assert!(
        text.trim_end()
            .ends_with("hand's final message:\n  I gave up on the borrow."),
        "{text}"
    );
}

#[test]
fn a_timeout_record_carries_the_outcome_label_and_the_exit() {
    let text = failed_record(
        "job timed out",
        "",
        Some(&receipt(124, "ran out of time", "")),
    );
    assert!(
        text.contains("outcome: agent failed: job timed out\n"),
        "{text}"
    );
    assert!(
        text.contains("failing command:\n  agent exit 124\n"),
        "{text}"
    );
    assert!(!text.contains("excerpt:"), "{text}");
    assert!(
        text.contains("hand's final message:\n  ran out of time"),
        "{text}"
    );
}

#[test]
fn a_lost_job_record_says_so_and_has_no_failing_command() {
    let text = failed_record("job lost", "slot vanished", None);
    assert!(text.contains("outcome: agent failed: job lost\n"), "{text}");
    assert!(!text.contains("failing command:"), "{text}");
    assert!(
        text.trim_end().ends_with("hand's final message:\n  (none)"),
        "{text}"
    );
}

#[test]
fn a_record_for_a_job_with_no_receipt_names_that_ending() {
    let text = failed_record("job ended without a receipt", "", None);
    assert!(
        text.contains("outcome: agent failed: job ended without a receipt\n"),
        "{text}"
    );
    assert!(!text.contains("failing command:"), "{text}");
    assert!(
        text.trim_end().ends_with("hand's final message:\n  (none)"),
        "{text}"
    );
}

#[test]
fn the_final_message_and_the_excerpt_keep_their_tails() {
    let long_message = format!("HEAD-OF-MESSAGE {} TAIL-OF-MESSAGE", "m".repeat(3000));
    let long_stderr = format!("HEAD-OF-STDERR {} TAIL-OF-STDERR", "e".repeat(3000));
    let text = failed_record(
        "hand failed",
        "",
        Some(&receipt(1, &long_message, &long_stderr)),
    );
    assert!(text.contains("TAIL-OF-MESSAGE"), "message tail kept");
    assert!(!text.contains("HEAD-OF-MESSAGE"), "message head cut");
    assert!(text.contains("TAIL-OF-STDERR"), "excerpt tail kept");
    assert!(!text.contains("HEAD-OF-STDERR"), "excerpt head cut");
}

#[test]
fn a_proof_failure_record_names_the_failing_command_and_the_output() {
    let detail = "After merging into the integration branch, this command failed (exit 101):\ncargo test --lib\n\nWhat failed:\nerror[E0308]: mismatched types\n";
    let text = failed_record(
        "proof-failed",
        detail,
        Some(&receipt(0, "pushed the fix", "")),
    );
    assert!(text.contains("outcome: proof-failed\n"), "{text}");
    assert!(
        text.contains(
            "failing command:\n  cargo test --lib\nexcerpt:\n  error[E0308]: mismatched types\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("hand's final message:\n  pushed the fix"),
        "{text}"
    );
}

#[test]
fn a_merge_conflict_record_posts_the_conflict_text_as_the_excerpt() {
    let text = failed_record(
        "merge-conflict",
        "CONFLICT (content): src/a.rs",
        Some(&receipt(0, "done", "")),
    );
    assert!(text.contains("outcome: merge-conflict\n"), "{text}");
    assert!(
        text.contains("failing command:\n  (none)\nexcerpt:\n  CONFLICT (content): src/a.rs\n"),
        "{text}"
    );
}

#[test]
fn a_failed_send_is_logged_and_the_post_reports_false_after_one_try() {
    let (_dir, ctx, host) = setup();
    host.reply("mail reply", 1, "relay down");
    let job = PoolJob::from(JobSpec::new("slice-t9-a1", "hand", "hand", "s", "b"));
    let ending = Ending::failed("hand failed", "", None);

    assert!(!post_on_task(
        &ctx,
        "t9",
        &job,
        &ending,
        &receipt(1, "m", "")
    ));

    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("attempt record send failed on task t9")),
        "{:?}",
        host.logs()
    );
    assert_eq!(mails_to(&host, "t9").len(), 1, "one try per attempt end");
}

#[test]
fn agent_silent_reads_a_pool_summary_without_a_receipt_section() {
    let summary = "pool result j1: finished\npool p  slot s  model m\nagent exit 1 in 2s, job 3s, started from abc";
    assert!(crate::pump::attempt_record::agent_silent(&format!(
        "{summary}\n\n--- stderr tail\nboom"
    )));
    assert!(crate::pump::attempt_record::agent_silent(summary));
    assert!(!crate::pump::attempt_record::agent_silent(&format!(
        "{summary}\n\n--- receipt\n+++\nanswer = \"planned\"\n+++"
    )));
    // A bare message (no pool summary) is the hand's message whole, never silent.
    assert!(!crate::pump::attempt_record::agent_silent(
        "ANSWER: planned"
    ));
    assert!(!crate::pump::attempt_record::agent_silent(""));
}

/// The summary of hand attempt de2a92a6-a1 (2026-11-04 local pump): pushed,
/// model on the pool line, and the base commit after `started from`.
const PUSHED_SUMMARY: &str = "pool result ba29e2bb600a: finished\n\
    pool studio  slot s1  model zai/glm-5.3-flash\n\
    from 9d85bb4e526f75aa43e7084afcd8271122b4befb  branch vivi/unit/de2a92a6-a1\n\
    push: pushed vivi/unit/de2a92a6-a1 2694a5191b\n\
    agent exit 0 in 37s, job 40s, started from 9d85bb4e52\n\n\
    --- receipt\nall checks pass";

fn record_of(job: &PoolJob, ending: &Ending, receipt: &str) -> String {
    let (_dir, ctx, _host) = setup();
    assert!(post_on_task(&ctx, "t9", job, ending, receipt));
    let records = records_for(&ctx, "t9");
    assert_eq!(records.len(), 1, "{records:?}");
    records[0].1.clone()
}

#[test]
fn a_record_of_a_pushed_attempt_names_its_branch_commit_model_and_no_fake_start() {
    let mut spec = JobSpec::new("slice-de2a92a6-a1", "hand", "hand", "s", "b");
    spec.branch = Some(String::from("vivi/unit/de2a92a6-a1"));
    let mut job = PoolJob::from(spec);
    job.payload = serde_json::json!({"attempt": 1});
    let text = record_of(
        &job,
        &Ending::failed("hand failed", "", Some(PUSHED_SUMMARY)),
        PUSHED_SUMMARY,
    );
    assert!(
        text.contains("branch: vivi/unit/de2a92a6-a1 at 2694a5191b (pushed"),
        "{text}"
    );
    assert!(!text.contains("left no changes"), "{text}");
    assert!(text.contains("model: zai/glm-5.3-flash\n"), "{text}");
    assert!(text.contains("start: none\n"), "{text}");
}

#[test]
fn a_record_of_an_attempt_with_no_changes_says_so() {
    let receipt = PUSHED_SUMMARY.replace(
        "push: pushed vivi/unit/de2a92a6-a1 2694a5191b",
        "push: no_changes",
    );
    let job = PoolJob::from(JobSpec::new("slice-t9-a1", "hand", "hand", "s", "b"));
    let text = record_of(
        &job,
        &Ending::failed("hand failed", "", Some(&receipt)),
        &receipt,
    );
    assert!(
        text.contains("branch: none (the hand left no changes)\n"),
        "{text}"
    );
}

#[test]
fn a_record_of_a_refused_attempt_names_the_refusal_not_an_agent_failure() {
    let job = PoolJob::from(JobSpec::new("slice-t9-a1", "hand", "hand", "s", "b"));
    let reason = "the envelope says built but reports failing checks: ./scripta/check (before fix)";
    let text = record_of(
        &job,
        &Ending::refused(reason, Some(PUSHED_SUMMARY)),
        PUSHED_SUMMARY,
    );
    assert!(text.contains("outcome: refused by the pump\n"), "{text}");
    assert!(text.contains(reason), "{text}");
    assert!(!text.contains("agent failed"), "{text}");
    assert!(!text.contains("agent exit 0"), "{text}");
}
