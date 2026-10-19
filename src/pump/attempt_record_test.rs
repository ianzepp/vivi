//! Tests for [`crate::pump::attempt_record`]: the record posted on the need
//! thread when a hand attempt ends, driven through [`FakeHost`] replies.

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use crate::pump::attempt_record::{agent_exit, agent_secs, clear_ready, mark_ready, post_status};
use crate::pump::board::board;
use crate::pump::collect::{collect, fail};
use crate::pump::config::{BoardConfig, DispatchConfig, Kind, PumpConfig, PumpTarget};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::quarantine::set_aside;
use crate::pump::state::{MergeResult, Quarantine, State, Status, Unit};

const T0: u64 = 1_000_000;
const TASK: &str = "1a2b3c4d";
const BRANCH: &str = "unit/u-1-a1";
const RECONCILE: &str = "collected 1a2b3c4d bridge slot-1 finished \"pushed unit/u-1-a1 abc\"";

fn config() -> PumpConfig {
    PumpConfig {
        branch: String::from("next"),
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
    (dir, ctx, host)
}

/// An inflight unit of need `n1`, attempt 1, with a model, a pool and a task.
fn inflight() -> Unit {
    Unit {
        need: String::from("n1"),
        status: Status::Inflight,
        attempts: 1,
        task: Some(String::from(TASK)),
        branch: Some(String::from(BRANCH)),
        model: Some(String::from("zai/glm")),
        pool: Some(String::from("fly4")),
        since: T0,
        ready_at: Some(T0 - 90),
        since_dispatch: Some(T0 - 60),
        ..Unit::default()
    }
}

fn state_with(unit: Unit) -> State {
    let mut st = State::default();
    st.units.insert(String::from("u-1"), unit);
    st
}

/// A task receipt as the pool writes it: summary lines, then sections.
fn receipt(exit: i32, message: &str, stderr: &str) -> String {
    format!(
        "pool result j1: finished\npool fly4  slot s1  model zai/glm\nfrom next  default\n\
         push: pushed unit/u-1-a1 abcdef0123\nagent exit {exit} in 12s, job 20s, started from 1234567890\n\n\
         --- receipt\n{message}\n\n--- stderr tail\n{stderr}\n\n--- runner log tail\nlog line"
    )
}

/// The branch is on the remote.
fn branch_on_remote(host: &FakeHost) {
    host.reply("ls-remote", 0, "abc\trefs/heads/unit/u-1-a1\n");
}

/// The `mail reply` commands sent to need `need`.
fn mails_to(host: &FakeHost, need: &str) -> Vec<String> {
    let needle = format!("mail reply {need} ");
    host.commands()
        .into_iter()
        .filter(|cmd| cmd.contains(&needle))
        .collect()
}

fn mails(host: &FakeHost) -> Vec<String> {
    mails_to(host, "n1")
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

/// The attempt records written for need `n1`: file name and text.
fn records(ctx: &Ctx) -> Vec<(String, String)> {
    records_for(ctx, "n1")
}

/// The one record; panics unless there is exactly one.
fn the_record(ctx: &Ctx) -> String {
    let mut found = records(ctx);
    assert_eq!(found.len(), 1, "{found:?}");
    found.remove(0).1
}

#[test]
fn a_delivered_attempt_posts_the_full_record_once_and_marks_the_sync() {
    let (_dir, ctx, host) = setup();
    host.reply("pool reconcile", 0, RECONCILE);
    branch_on_remote(&host);
    host.reply("task show", 0, &receipt(0, "All done: fixed it.", ""));
    host.reply("log --oneline", 0, "c2 second\nc1 first\n");
    host.reply(
        "diff --stat",
        0,
        " src/a.rs | 2 +-\n 1 file changed, 1 insertion(+), 1 deletion(-)\n",
    );
    let mut unit = inflight();
    unit.slot = Some(String::from("s1"));
    let mut st = state_with(unit);
    collect(&ctx, &mut st);
    assert_eq!(st.units["u-1"].status, Status::Delivered);
    assert_eq!(st.units["u-1"].ready_at, None);
    let expected = "pump: attempt record\n\
        attempt: u-1-a1\n\
        need: n1\n\
        task: 1a2b3c4d\n\
        actor: none\n\
        role: hand\n\
        kind: plain\n\
        pool: fly4\n\
        slot: s1\n\
        model: zai/glm\n\
        ready-at: 999910\n\
        start: 999940\n\
        end: 1000000\n\
        agent-seconds: 12\n\
        delivered-at: 1000000\n\
        merged-at: none\n\
        outcome: delivered\n\
        unit: u-1  attempt: 1  model: zai/glm  pool: fly4  target: default\n\
        branch: unit/u-1-a1 (pushed to the target remote, kept)\n\
        task: 1a2b3c4d\n\
        base: next\n\
        commits:\n  c2 second\n  c1 first\n\
        files changed:\n   src/a.rs | 2 +-\n   1 file changed, 1 insertion(+), 1 deletion(-)\n\
        hand's final message:\n  All done: fixed it.";
    let (name, text) = records(&ctx).remove(0);
    assert_eq!(name, "reply-n1-attempt-u-1-a1-delivered.txt");
    assert_eq!(text.trim_end(), expected);
    let sent = mails(&host);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].contains("--from mind") && sent[0].contains("--to operator@x"));
    assert!(st.board.sync_pending);
    // The unit left `inflight`: a second pass posts nothing more.
    collect(&ctx, &mut st);
    assert_eq!(mails(&host).len(), 1);
}

#[test]
fn a_merged_status_posts_a_synced_attempt_event() {
    let (_dir, ctx, host) = setup();
    let mut st = state_with(inflight());

    post_status(&ctx, &mut st, "u-1", "merged", T0 + 42);

    let text =
        fs::read_to_string(ctx.home.join("board/reply-n1-attempt-u-1-a1-merged.txt")).unwrap();
    assert_eq!(
        text.trim_end(),
        "pump: attempt status event\nattempt: u-1-a1\nneed: n1\nstatus: merged\nat: 1000042"
    );
    assert_eq!(mails(&host).len(), 1);
    assert!(st.board.sync_pending);
}

#[test]
fn ready_time_keeps_the_first_observation_until_cleared() {
    let mut st = state_with(Unit::default());

    mark_ready(&mut st, "u-1", T0);
    mark_ready(&mut st, "u-1", T0 + 20);
    assert_eq!(st.units["u-1"].ready_at, Some(T0));

    clear_ready(&mut st, "u-1");
    mark_ready(&mut st, "u-1", T0 + 30);
    assert_eq!(st.units["u-1"].ready_at, Some(T0 + 30));
}

#[test]
fn a_nonzero_agent_exit_with_a_pushed_branch_is_a_failed_attempt_not_a_delivery() {
    let (_dir, ctx, host) = setup();
    host.reply("pool reconcile", 0, RECONCILE);
    branch_on_remote(&host);
    host.reply("log --oneline", 0, "w1 WIP (attempt failed): fix it\n");
    host.reply(
        "task show",
        0,
        &receipt(3, "I could not finish.", "thread panicked: boom"),
    );
    let mut st = state_with(inflight());
    collect(&ctx, &mut st);
    let unit = &st.units["u-1"];
    assert_eq!(unit.status, Status::Pending);
    assert_eq!(unit.task, None);
    let attempt = unit.history.last().unwrap();
    assert_eq!(attempt.why, "finished/agent exit 3");
    assert_eq!(attempt.branch.as_deref(), Some(BRANCH));
    assert_eq!(attempt.task.as_deref(), Some(TASK));
    let text = the_record(&ctx);
    assert!(
        text.contains("outcome: agent failed: finished/agent exit 3\n"),
        "{text}"
    );
    assert!(text.contains("branch: unit/u-1-a1 (pushed to the target remote, kept)"));
    assert!(text.contains("commits:\n  w1 WIP (attempt failed): fix it\n"));
    assert!(text.contains("failing command:\n  agent exit 3\nexcerpt:\n  thread panicked: boom\n"));
    assert!(text.contains("hand's final message:\n  I could not finish."));
    assert_eq!(mails(&host).len(), 1);
}

const FIX_NEED: &str = "pumpfix-g01234567";
const FIX_UID: &str = "pumpfix-g01234567-1";

/// A gate-fix unit: uid `pumpfix-g01234567-1`, on need `need`.
fn fix_state(need: &str) -> State {
    let mut st = State::default();
    st.units.insert(
        String::from(FIX_UID),
        Unit {
            need: String::from(need),
            ..inflight()
        },
    );
    st
}

/// The wiring of a failed attempt that pushed its branch, on a fix unit.
fn a_failed_fix_attempt(host: &FakeHost) {
    host.reply("pool reconcile", 0, RECONCILE);
    branch_on_remote(host);
    host.reply("log --oneline", 0, "w1 WIP (attempt failed): fix it\n");
    host.reply(
        "task show",
        0,
        &receipt(3, "I could not finish.", "thread panicked: boom"),
    );
}

#[test]
fn a_fix_unit_with_a_pump_synthesized_need_posts_no_reply() {
    let (_dir, ctx, host) = setup();
    a_failed_fix_attempt(&host);
    host.reply("mail reply", 1, "message not found");
    let mut st = fix_state(FIX_NEED);
    collect(&ctx, &mut st);
    // The attempt itself is still collected.
    assert_eq!(st.units[FIX_UID].status, Status::Pending);
    assert_eq!(st.units[FIX_UID].history.len(), 1);
    let replies: Vec<String> = host
        .commands()
        .into_iter()
        .filter(|cmd| cmd.contains("mail reply"))
        .collect();
    assert!(replies.is_empty(), "{replies:?}");
    let names: Vec<String> = fs::read_dir(ctx.home.join("board"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("reply-pumpfix-"))
        .collect();
    assert!(names.is_empty(), "{names:?}");
    let logs = host.logs();
    assert!(
        !logs
            .iter()
            .any(|line| line.contains("attempt record send failed")),
        "{logs:?}"
    );
    assert!(!st.board.sync_pending);
}

#[test]
fn a_fix_unit_of_a_real_gatefix_need_posts_its_record_on_that_thread() {
    let (_dir, ctx, host) = setup();
    a_failed_fix_attempt(&host);
    let mut st = fix_state("ab12cd34");
    collect(&ctx, &mut st);
    let sent = mails_to(&host, "ab12cd34");
    assert_eq!(sent.len(), 1, "{sent:?}");
    let found = records_for(&ctx, "ab12cd34");
    assert_eq!(found.len(), 1, "{found:?}");
    // The stage is `attempt-{uid}-a{attempt}-{slug}`, and `slug` is the
    // ending label (`agent failed: finished/agent exit 3`) made file-safe and
    // cut to 24 characters.
    assert_eq!(
        found[0].0,
        "reply-ab12cd34-attempt-pumpfix-g01234567-1-a1-agent-failed--finished-a.txt"
    );
    assert!(found[0].1.contains("pump: attempt record\n"));
    assert!(st.board.sync_pending);
}

#[test]
fn a_failed_attempt_with_no_branch_says_the_hand_left_nothing() {
    let (_dir, ctx, host) = setup();
    host.reply("pool reconcile", 0, RECONCILE);
    host.reply("ls-remote", 0, "");
    host.reply("task show", 0, "agent replied: did nothing");
    let mut st = state_with(inflight());
    collect(&ctx, &mut st);
    let text = the_record(&ctx);
    assert!(
        text.contains("outcome: agent failed: finished/pushed unit/u-1-a1 abc\n"),
        "{text}"
    );
    assert!(text.contains("branch: unit/u-1-a1 (none: the hand left no changes)\n"));
    assert!(!text.contains("commits:") && !text.contains("failing command:"));
    assert!(
        text.trim_end().ends_with(
            "hand's final message:
  agent replied: did nothing"
        ),
        "{text}"
    );
}

#[test]
fn a_timeout_posts_a_split_record_and_keeps_the_branch_on_the_attempt() {
    let (_dir, ctx, host) = setup();
    host.reply("pool reconcile", 0, RECONCILE);
    branch_on_remote(&host);
    host.reply("task show", 0, &receipt(124, "ran out of time", ""));
    let mut st = state_with(inflight());
    collect(&ctx, &mut st);
    let unit = &st.units["u-1"];
    assert_eq!(unit.status, Status::Splitting);
    assert_eq!(unit.history[0].branch.as_deref(), Some(BRANCH));
    assert_eq!(unit.history[0].task.as_deref(), Some(TASK));
    let text = the_record(&ctx);
    assert!(text.contains("outcome: timeout -> split\n"), "{text}");
    assert!(text.contains("task: 1a2b3c4d\n"));
    assert!(text.contains("hand's final message:\n  ran out of time"));
}

#[test]
fn an_overdue_running_unit_is_recorded_without_fetching_a_receipt() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d bridge slot-1 running \"pid 7\"",
    );
    let mut st = state_with(inflight());
    st.units.get_mut("u-1").unwrap().since = T0 - 100_000;
    collect(&ctx, &mut st);
    assert_eq!(st.units["u-1"].status, Status::Splitting);
    let text = the_record(&ctx);
    assert!(text.contains("outcome: timeout -> split\n"), "{text}");
    assert!(text.contains("overdue: still running long past its budget"));
    assert!(!host.commands().iter().any(|cmd| cmd.contains("task show")));
}

#[test]
fn a_no_defect_verdict_posts_its_record() {
    let (_dir, ctx, host) = setup();
    host.reply("pool reconcile", 0, RECONCILE);
    host.reply("ls-remote", 0, "");
    host.reply(
        "task show",
        0,
        &receipt(0, "VERDICT: no-defect\nthe code is already right", ""),
    );
    let mut st = state_with(inflight());
    collect(&ctx, &mut st);
    assert_eq!(st.units["u-1"].status, Status::NoDefect);
    let text = the_record(&ctx);
    assert!(text.contains("outcome: no-defect\n"), "{text}");
    assert!(text.contains("  VERDICT: no-defect\n  the code is already right"));
}

#[test]
fn a_proof_failure_names_the_failing_command_and_the_output() {
    let (_dir, ctx, host) = setup();
    branch_on_remote(&host);
    host.reply("task show", 0, &receipt(0, "pushed the fix", ""));
    let mut unit = inflight();
    unit.status = Status::Merging;
    let mut st = state_with(unit);
    let detail = "After merging into the integration branch, this command failed (exit 101):\ncargo test --lib\n\nWhat failed:\nerror[E0308]: mismatched types\n";
    fail(&ctx, &mut st, "u-1", "proof-failed", detail);
    assert_eq!(st.units["u-1"].status, Status::Pending);
    let text = the_record(&ctx);
    assert!(text.contains("outcome: proof-failed\n"), "{text}");
    assert!(text.contains(
        "failing command:\n  cargo test --lib\nexcerpt:\n  error[E0308]: mismatched types\n"
    ));
    assert!(text.contains("hand's final message:\n  pushed the fix"));
    assert_eq!(mails(&host).len(), 1);
}

#[test]
fn a_merge_conflict_posts_the_conflict_text_as_the_excerpt() {
    let (_dir, ctx, host) = setup();
    branch_on_remote(&host);
    host.reply("task show", 0, &receipt(0, "done", ""));
    let mut unit = inflight();
    unit.status = Status::Merging;
    let mut st = state_with(unit);
    fail(
        &ctx,
        &mut st,
        "u-1",
        "merge-conflict",
        "CONFLICT (content): src/a.rs",
    );
    let text = the_record(&ctx);
    assert!(text.contains("outcome: merge-conflict\n"), "{text}");
    assert!(
        text.contains("failing command:\n  (none)\nexcerpt:\n  CONFLICT (content): src/a.rs\n")
    );
}

#[test]
fn commits_and_files_are_capped() {
    let (_dir, ctx, host) = setup();
    host.reply("pool reconcile", 0, RECONCILE);
    branch_on_remote(&host);
    host.reply("task show", 0, &receipt(0, "ok", ""));
    let commits: String = (1..=25).map(|n| format!("c{n} commit {n}\n")).collect();
    host.reply("log --oneline", 0, &commits);
    let mut stat: String = (1..=59).map(|n| format!(" src/f{n}.rs | 1 +\n")).collect();
    stat.push_str(" 59 files changed, 59 insertions(+)\n");
    host.reply("diff --stat", 0, &stat);
    let mut st = state_with(inflight());
    collect(&ctx, &mut st);
    let text = the_record(&ctx);
    let commits_block = text.split("commits:\n").nth(1).unwrap();
    let commits_block = commits_block.split("files changed:").next().unwrap();
    assert_eq!(commits_block.lines().count(), 21, "{commits_block}");
    assert!(commits_block.contains("  ... and 5 more"));
    let files_block = text.split("files changed:\n").nth(1).unwrap();
    let files_block = files_block.split("hand's final message:").next().unwrap();
    assert_eq!(files_block.lines().count(), 40, "{files_block}");
    assert!(files_block.contains("... 21 more files"));
    assert!(
        files_block
            .trim_end()
            .ends_with("59 files changed, 59 insertions(+)")
    );
}

#[test]
fn a_failed_send_is_logged_once_and_never_blocks_the_tick() {
    let (_dir, ctx, host) = setup();
    host.reply("pool reconcile", 0, RECONCILE);
    branch_on_remote(&host);
    host.reply("task show", 0, &receipt(0, "ok", ""));
    host.reply("mail reply", 1, "relay down");
    let mut st = state_with(inflight());
    collect(&ctx, &mut st);
    assert_eq!(st.units["u-1"].status, Status::Delivered);
    assert!(
        host.logs()
            .contains(&String::from("attempt record send failed u-1"))
    );
    assert_eq!(mails(&host).len(), 1, "one try per attempt end");
    assert!(!st.board.sync_pending);
}

#[test]
fn a_repair_unit_posts_no_record() {
    let (_dir, ctx, host) = setup();
    host.reply("task show", 0, &receipt(0, "ok", ""));
    let mut unit = inflight();
    unit.repairs = Some(String::from("q1"));
    let mut st = state_with(unit);
    fail(&ctx, &mut st, "u-1", "proof-failed", "x");
    assert!(records(&ctx).is_empty());
    assert!(mails(&host).is_empty());
}

#[test]
fn a_quarantined_unit_posts_a_record_naming_the_batch_and_the_repair() {
    let (_dir, ctx, host) = setup();
    branch_on_remote(&host);
    host.reply("task show", 0, &receipt(0, "pushed", ""));
    let mut unit = inflight();
    unit.status = Status::Merging;
    let mut st = state_with(unit);
    st.order.push(String::from("u-1"));
    let result = MergeResult {
        uid: String::from("u-1"),
        ok: None,
        why: Some(String::from("quarantined")),
        quarantine: Some(Quarantine {
            id: String::from("q1-abc"),
            cmd: String::from("cargo test"),
            code: 1,
            text: String::from("boom"),
            units: vec![String::from("u-1")],
        }),
        ..MergeResult::default()
    };
    set_aside(&ctx, &mut st, &result);
    assert_eq!(st.units["u-1"].status, Status::Quarantined);
    let repair = st.units.keys().find(|uid| uid.as_str() != "u-1").unwrap();
    let text = the_record(&ctx);
    assert!(
        text.contains(&format!(
            "outcome: quarantined (batch q1-abc, repair unit {repair})\n"
        )),
        "{text}"
    );
    assert_eq!(mails(&host).len(), 1, "the repair unit posts nothing");
}

fn local_target_config() -> PumpConfig {
    let mut cfg = config();
    cfg.targets.insert(
        String::from("tela"),
        PumpTarget {
            path: String::from("tela"),
            branch: String::from("main"),
            ..PumpTarget::default()
        },
    );
    cfg
}

fn local_unit() -> Unit {
    Unit {
        target: String::from("tela"),
        ..inflight()
    }
}

#[test]
fn a_failed_local_worktree_attempt_branch_is_pushed_without_force() {
    let (_dir, ctx, host) = setup_with(local_target_config());
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d bridge slot-1 finished \"committed unit/u-1-a1 abc\"",
    );
    host.reply("show-ref --verify --quiet refs/heads/unit/u-1-a1", 0, "");
    host.reply("task show", 0, &receipt(2, "gave up", ""));
    let mut st = state_with(local_unit());
    collect(&ctx, &mut st);
    assert_eq!(st.units["u-1"].status, Status::Pending);
    let pushes: Vec<String> = host
        .commands()
        .into_iter()
        .filter(|cmd| cmd.contains("push origin"))
        .collect();
    assert_eq!(pushes.len(), 1, "{pushes:?}");
    assert!(pushes[0].ends_with("push origin refs/heads/unit/u-1-a1:refs/heads/unit/u-1-a1"));
    assert!(!pushes[0].contains("--force") && !pushes[0].contains(" +"));
    assert!(
        host.logs()
            .contains(&String::from("attempt branch unit/u-1-a1 pushed"))
    );
    assert!(the_record(&ctx).contains("outcome: agent failed: finished/agent exit 2\n"));
}

#[test]
fn a_delivered_local_worktree_attempt_branch_is_pushed_too_and_a_refused_push_is_logged() {
    let (_dir, ctx, host) = setup_with(local_target_config());
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d bridge slot-1 finished \"committed unit/u-1-a1 abc\"",
    );
    host.reply("show-ref --verify --quiet refs/heads/unit/u-1-a1", 0, "");
    host.reply("task show", 0, &receipt(0, "done", ""));
    host.reply("push origin", 1, "rejected: non-fast-forward");
    let mut st = state_with(local_unit());
    collect(&ctx, &mut st);
    assert_eq!(st.units["u-1"].status, Status::Delivered);
    assert!(
        host.logs()
            .iter()
            .any(|line| { line.starts_with("attempt branch unit/u-1-a1 push failed: rejected") })
    );
    assert_eq!(mails(&host).len(), 1);
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

/// The `(command, timeout)` pairs of every command the host ran.
fn command_timeouts(host: &FakeHost) -> Vec<(String, u64)> {
    host.commands().into_iter().zip(host.timeouts()).collect()
}

/// The timeout of the first command containing `fragment`; panics without
/// one.
fn timeout_of(host: &FakeHost, fragment: &str) -> u64 {
    command_timeouts(host)
        .into_iter()
        .find(|(cmd, _)| cmd.contains(fragment))
        .map(|(_, secs)| secs)
        .unwrap_or_else(|| panic!("no command matching {fragment:?}"))
}

#[test]
fn the_record_fetch_writes_no_shared_refs() {
    let (_dir, ctx, host) = setup();
    host.reply("pool reconcile", 0, RECONCILE);
    branch_on_remote(&host);
    host.reply("task show", 0, &receipt(0, "done", ""));
    host.reply("log --oneline", 0, "c1 fix\n");
    host.reply("diff --stat", 0, " src/a.rs | 1 +\n");
    let mut st = state_with(inflight());
    collect(&ctx, &mut st);
    let cmds = host.commands();
    assert!(
        !cmds.iter().any(|cmd| cmd.contains("refs/remotes")),
        "{cmds:?}"
    );
    assert!(
        !cmds.iter().any(|cmd| cmd.contains("update-ref")),
        "{cmds:?}"
    );
    let fetch = cmds
        .iter()
        .find(|cmd| cmd.contains("git fetch"))
        .unwrap_or_else(|| panic!("{cmds:?}"));
    assert!(fetch.contains("--no-write-fetch-head"), "{fetch}");
    assert!(
        fetch.contains("refs/heads/unit/u-1-a1:refs/pump-records/unit/u-1-a1"),
        "{fetch}"
    );
    assert!(the_record(&ctx).contains("(pushed to the target remote, kept)"));
}

#[test]
fn a_local_record_reads_refs_heads_directly_and_pushes() {
    let (_dir, ctx, host) = setup_with(local_target_config());
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d bridge slot-1 finished \"committed unit/u-1-a1 abc\"",
    );
    host.reply("show-ref --verify --quiet refs/heads/unit/u-1-a1", 0, "");
    host.reply("task show", 0, &receipt(0, "done", ""));
    host.reply("log --oneline", 0, "c1 fix\n");
    host.reply("diff --stat", 0, " src/a.rs | 1 +\n");
    let mut st = state_with(local_unit());
    collect(&ctx, &mut st);
    let cmds = host.commands();
    assert!(
        !cmds.iter().any(|cmd| cmd.contains("git fetch")),
        "{cmds:?}"
    );
    let log = cmds
        .iter()
        .find(|cmd| cmd.contains("log --oneline"))
        .unwrap_or_else(|| panic!("{cmds:?}"));
    assert!(log.contains("refs/heads/unit/u-1-a1"), "{log}");
    let pushes: Vec<&String> = cmds.iter().filter(|cmd| cmd.contains("push")).collect();
    assert_eq!(pushes.len(), 1, "the push is the only write: {cmds:?}");
    assert!(the_record(&ctx).contains("(pushed to the target remote, kept)"));
}

#[test]
fn a_rejected_push_is_recorded_as_rejected_not_pushed() {
    let (_dir, ctx, host) = setup_with(local_target_config());
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d bridge slot-1 finished \"committed unit/u-1-a1 abc\"",
    );
    host.reply("show-ref --verify --quiet refs/heads/unit/u-1-a1", 0, "");
    host.reply("task show", 0, &receipt(0, "done", ""));
    host.reply("push origin", 1, "! [rejected] non-fast-forward");
    let mut st = state_with(local_unit());
    collect(&ctx, &mut st);
    let text = the_record(&ctx);
    assert!(
        text.contains("branch: unit/u-1-a1 (push rejected: ! [rejected] non-fast-forward)\n"),
        "{text}"
    );
    assert!(
        !text.contains("(pushed to the target remote, kept)"),
        "{text}"
    );
}

#[test]
fn a_failed_remote_listing_is_branch_not_readable_not_no_changes() {
    let (_dir, ctx, host) = setup();
    host.reply("pool reconcile", 0, RECONCILE);
    host.reply("ls-remote", 1, "could not resolve host");
    host.reply("task show", 0, &receipt(0, "done", ""));
    let mut st = state_with(inflight());
    collect(&ctx, &mut st);
    let text = the_record(&ctx);
    assert!(
        text.contains("branch: unit/u-1-a1 (branch not readable: could not resolve host)\n"),
        "{text}"
    );
    assert!(!text.contains("none: the hand left no changes"), "{text}");
}

#[test]
fn the_record_path_gives_every_git_call_30_seconds() {
    let (_dir, ctx, host) = setup();
    host.reply("pool reconcile", 0, RECONCILE);
    branch_on_remote(&host);
    host.reply("task show", 0, &receipt(0, "done", ""));
    host.reply("log --oneline", 0, "c1 fix\n");
    host.reply("diff --stat", 0, " src/a.rs | 1 +\n");
    let mut st = state_with(inflight());
    collect(&ctx, &mut st);
    assert_eq!(timeout_of(&host, "git fetch"), 30);
    assert_eq!(timeout_of(&host, "log --oneline"), 30);
    assert_eq!(timeout_of(&host, "diff --stat"), 30);
    let ls_remotes: Vec<u64> = command_timeouts(&host)
        .into_iter()
        .filter(|(cmd, _)| cmd.contains("git ls-remote"))
        .map(|(_, secs)| secs)
        .collect();
    assert!(
        ls_remotes.contains(&30),
        "record ls-remote at 30: {ls_remotes:?}"
    );
}

#[test]
fn the_record_push_also_runs_within_30_seconds() {
    let (_dir, ctx, host) = setup_with(local_target_config());
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d bridge slot-1 finished \"committed unit/u-1-a1 abc\"",
    );
    host.reply("show-ref --verify --quiet refs/heads/unit/u-1-a1", 0, "");
    host.reply("task show", 0, &receipt(0, "done", ""));
    let mut st = state_with(local_unit());
    collect(&ctx, &mut st);
    assert_eq!(timeout_of(&host, "push origin"), 30);
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
