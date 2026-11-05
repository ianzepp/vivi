//! Repairer start: a job from the current main tip, the need_branches row,
//! the prior-attempt brief.

use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::RepairerStart;
use crate::mailspace::pump_note::note_body;
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::restart::restart;
use crate::pump::flow::advance::start::start as start_run;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::repair::{self, AttemptFacts};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::store::step_payload::StepPayloadRow;
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::RoutedRole;
use vivi_error::VivariumError;
use vivi_mail::letter::fixture::repair_record_body;
use vivi_mail::letter::records::{NoteWord, RecordFront, RecordKind};
use vivi_mail::letter::render;

type R = Result<(), VivariumError>;

const NEED: &str = "abcd1234";
const MAIN_A: &str = "1111111111111111111111111111111111111111";
const MAIN_B: &str = "2222222222222222222222222222222222222222";

fn record() -> String {
    repair_record_body(NEED, "default", Some("0000"), "FAILED a::b")
}

/// A repair record that names no target.
fn targetless_record() -> String {
    let front = RecordFront {
        need: NEED.into(),
        kind: Some(RecordKind::Repair),
        target: None,
        tested: None,
    };
    render(&front, "FAILED a::b").unwrap()
}

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    host: Arc<FakeHost>,
    store: PumpStore,
    mail: FakeMail,
}

fn rig(role: bool, record: &str) -> Result<Rig, VivariumError> {
    rig_cfg(role, record, PumpConfig::default())
}

/// [`rig_cfg`] with the arguments in the order the budget tests use.
fn rig_with(cfg: PumpConfig, role: bool, record: &str) -> Result<Rig, VivariumError> {
    rig_cfg(role, record, cfg)
}

fn rig_cfg(role: bool, record: &str, cfg: PumpConfig) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (mut ctx, host) = test_ctx(dir.path(), cfg);
    if role {
        let mut routing = (*ctx.routing).clone();
        routing.roles.push(RoutedRole {
            name: String::from("repairer"),
            kind: Some(String::from("repairer")),
            status: String::from("active"),
            pool: Some(String::from("test-planner-pool")),
            read_only: None,
            model: None,
            models: Vec::new(),
        });
        ctx = ctx.with_routing(routing);
    }
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, NEED, record)?;
    Ok(Rig {
        _dir: dir,
        ctx,
        host,
        store,
        mail,
    })
}

fn main_tip(host: &FakeHost, sha: &str) {
    host.reply(
        "ls-remote origin main",
        0,
        &format!("{sha}\trefs/heads/main\n"),
    );
}

/// A pump configuration parsed from mailspace-file text.
fn pump_cfg(text: &str) -> PumpConfig {
    let ms: crate::mailspace::MailspaceConfig =
        toml::from_str(text).expect("mailspace file text should parse");
    ms.pump.expect("pump table should be present")
}

fn run() -> RunRow {
    RunRow {
        need: NEED.into(),
        run: 1,
        flow: "repair".into(),
        flow_version: 2,
        kind: "repair".into(),
        entry: "fix1".into(),
        state: "live".into(),
    }
}

fn step(node: &str, task: &str, state: StepState, verdict: Option<&str>) -> StepRow {
    StepRow {
        run: 1,
        node: node.into(),
        task: task.into(),
        state,
        verdict: verdict.map(str::to_owned),
    }
}

fn start(rig: &Rig, step: &StepRow) -> Result<StartAction, VivariumError> {
    start_in(rig, &run(), step)
}

fn start_in(rig: &Rig, run: &RunRow, step: &StepRow) -> Result<StartAction, VivariumError> {
    let def = load(FlowKind::Repair)?;
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run,
        def: &def,
    };
    RepairerStart.start(&cx, step)
}

fn job(action: StartAction) -> Box<crate::pump::jobs::JobSpec> {
    match action {
        StartAction::Job(spec) => spec,
        other => panic!("expected a job, got {other:?}"),
    }
}

#[test]
fn attempt_one_cuts_a_repair_branch_from_the_main_tip() -> R {
    let rig = rig(true, &record())?;
    main_tip(&rig.host, MAIN_A);
    let spec = job(start(&rig, &step("fix1", "t1", StepState::Pending, None))?);
    assert_eq!(spec.kind, "repairer");
    assert_eq!(spec.role, "repairer");
    assert_eq!(spec.task.as_deref(), Some("t1"));
    assert_eq!(spec.target.as_deref(), Some("default"));
    assert_eq!(spec.from_ref.as_deref(), Some(MAIN_A));
    assert_eq!(spec.branch.as_deref(), Some("vivi/repair/abcd1234-r1-a1"));
    assert_eq!(
        spec.publish_ref.as_deref(),
        Some("refs/heads/vivi/repair/abcd1234-r1-a1")
    );
    assert!(!spec.read_only);
    assert_eq!(spec.budget, 1800);
    assert!(spec.body.contains("Failing output:\nFAILED a::b"));
    assert!(spec.body.contains(MAIN_A));
    assert!(!spec.body.contains("Prior attempt"));
    let row = rig.store.need_branch(NEED)?.expect("need_branches row");
    assert_eq!(row.branch, "vivi/need/abcd1234");
    assert_eq!(row.base.as_deref(), Some(MAIN_A));
    assert_eq!(row.status, "live");
    assert_eq!((row.need.as_str(), row.run), (NEED, 1));
    assert_eq!(row.target, "default");
    Ok(())
}

#[test]
fn attempt_two_reads_a_new_tip_and_names_the_rung_verdict() -> R {
    let mut rig = rig(true, &record())?;
    let fix = rig.mail.send_step("mind", "mind", "fix1", "b", &[])?;
    let rung = rig.mail.send_step("mind", "mind", "rung1", "b", &[])?;
    rig.store
        .insert_step(NEED, &step("fix1", &fix, StepState::Done, Some("built")))?;
    rig.store
        .insert_step(NEED, &step("rung1", &rung, StepState::Done, Some("fail")))?;
    let facts = AttemptFacts {
        branch: repair::branch(NEED, 1, 1),
        commit: "c".repeat(40),
        base: MAIN_A.into(),
    };
    rig.mail.reply(
        &fix,
        &note_body(NoteWord::RepairAttempt, None, &repair::facts_text(&facts))?,
    )?;
    rig.mail.reply(
        &rung,
        &note_body(NoteWord::RepairRung, None, "failed on a::b::c_test")?,
    )?;
    main_tip(&rig.host, MAIN_B);
    let spec = job(start(&rig, &step("fix2", "t2", StepState::Pending, None))?);
    assert_eq!(spec.from_ref.as_deref(), Some(MAIN_B));
    assert_eq!(spec.branch.as_deref(), Some("vivi/repair/abcd1234-r1-a2"));
    assert!(spec.body.contains("Prior attempt"));
    assert!(
        spec.body.contains("- rung1: Done, verdict fail"),
        "{}",
        spec.body
    );
    assert!(spec.body.contains(&"c".repeat(40)));
    assert!(
        spec.body.contains("failed on a::b::c_test"),
        "{}",
        spec.body
    );
    let row = rig.store.need_branch(NEED)?.expect("row");
    assert_eq!(row.base.as_deref(), Some(MAIN_B));
    Ok(())
}

#[test]
fn attempts_keep_the_repair_cap_despite_thinking_overrides() -> R {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.budget = 222;
    cfg.dispatch.step_budgets.researcher = Some(90);
    cfg.dispatch.step_budgets.head_cto = Some(55);
    let mut rig = rig_with(cfg, true, &record())?;
    main_tip(&rig.host, MAIN_A);

    let fix1 = job(start(&rig, &step("fix1", "t1", StepState::Pending, None))?);

    // Repairer is not a thinking kind: every attempt keeps the explicit
    // repair cap, whatever the shared fallback and thinking overrides say.
    assert_eq!(fix1.budget, rig.ctx.cfg.repair_attempt_secs);
    assert_ne!(fix1.budget, rig.ctx.cfg.dispatch.budget);

    let fix = rig.mail.send_step("mind", "mind", "fix1", "b", &[])?;
    rig.store
        .insert_step(NEED, &step("fix1", &fix, StepState::Done, Some("built")))?;
    let facts = AttemptFacts {
        branch: repair::branch(NEED, 1, 1),
        commit: "c".repeat(40),
        base: MAIN_A.into(),
    };
    rig.mail.reply(
        &fix,
        &note_body(NoteWord::RepairAttempt, None, &repair::facts_text(&facts))?,
    )?;
    main_tip(&rig.host, MAIN_B);

    let fix2 = job(start(&rig, &step("fix2", "t2", StepState::Pending, None))?);

    assert_eq!(fix2.budget, rig.ctx.cfg.repair_attempt_secs);
    Ok(())
}

#[test]
fn repair_attempt_budget_defaults_to_1800() -> R {
    let cfg = pump_cfg("name = \"t\"\n[pump]\nbranch = \"next\"\n[pump.dispatch]\nbudget = 600\n");
    assert_eq!(cfg.repair_attempt_secs, 1800, "the repair key is omitted");
    assert_eq!(cfg.dispatch.budget, 600, "dispatch budget differs");
    let rig = rig_cfg(true, &record(), cfg)?;
    main_tip(&rig.host, MAIN_A);
    let fix1 = job(start(&rig, &step("fix1", "t1", StepState::Pending, None))?);
    let fix2 = job(start(&rig, &step("fix2", "t2", StepState::Pending, None))?);
    for spec in [&fix1, &fix2] {
        assert_eq!(spec.budget, 1800, "{}", spec.subject);
        assert!(
            spec.body
                .contains("Time cap: 1800 seconds for this attempt; no diff cap."),
            "{}",
            spec.body
        );
    }
    Ok(())
}

#[test]
fn repair_attempt_budget_uses_configuration() -> R {
    for (seconds, dispatch) in [(7200u64, 600u64), (7201, 900)] {
        let cfg = pump_cfg(&format!(
            "name = \"t\"\n[pump]\nbranch = \"next\"\nrepair_attempt_secs = {seconds}\n\
             [pump.dispatch]\nbudget = {dispatch}\n"
        ));
        assert_eq!(cfg.repair_attempt_secs, seconds);
        let rig = rig_cfg(true, &record(), cfg)?;
        main_tip(&rig.host, MAIN_A);
        let fix1 = job(start(&rig, &step("fix1", "t1", StepState::Pending, None))?);
        let fix2 = job(start(&rig, &step("fix2", "t2", StepState::Pending, None))?);
        let want = format!("Time cap: {seconds} seconds for this attempt; no diff cap.");
        for spec in [&fix1, &fix2] {
            assert_eq!(spec.budget, seconds, "{}", spec.subject);
            assert!(spec.body.contains(&want), "{}", spec.body);
        }
    }
    Ok(())
}

#[test]
fn no_repairer_role_is_an_error() -> R {
    let rig = rig(false, &record())?;
    main_tip(&rig.host, MAIN_A);
    let err = start(&rig, &step("fix1", "t1", StepState::Pending, None)).unwrap_err();
    assert!(err.to_string().contains("no repairer role"), "{err}");
    Ok(())
}

#[test]
fn a_record_without_a_target_is_an_error() -> R {
    let rig = rig(true, &targetless_record())?;
    main_tip(&rig.host, MAIN_A);
    let err = start(&rig, &step("fix1", "t1", StepState::Pending, None)).unwrap_err();
    assert!(err.to_string().contains("target"), "{err}");
    assert!(rig.store.need_branch(NEED)?.is_none());
    Ok(())
}

#[test]
fn a_node_without_an_attempt_is_an_error() -> R {
    let rig = rig(true, &record())?;
    let err = start(&rig, &step("unrepaired", "t1", StepState::Pending, None)).unwrap_err();
    assert!(matches!(err, VivariumError::Parse(_)), "{err}");
    Ok(())
}

#[test]
fn a_restarted_repair_run_publishes_to_fresh_refs() -> R {
    let mut rig = rig(true, &record())?;
    let old = start_run(&rig.ctx, &rig.store, &mut rig.mail, NEED, Some("fix1"))?;
    let rows = rig.store.steps_of(NEED, old.run)?;
    let fix1 = rows
        .iter()
        .find(|row| row.node == "fix1")
        .expect("fix1 row")
        .clone();
    let fix2 = rig.mail.send_step("mind", "mind", "fix2", "b", &[])?;
    rig.store.insert_step(
        NEED,
        &StepRow {
            run: old.run,
            node: "fix2".into(),
            task: fix2.clone(),
            state: StepState::Running,
            verdict: None,
        },
    )?;
    let mut old_refs = Vec::new();
    for (attempt, task) in [(1, &fix1.task), (2, &fix2)] {
        let branch = repair::branch(NEED, old.run, attempt);
        let facts = AttemptFacts {
            branch: branch.clone(),
            commit: "c".repeat(40),
            base: MAIN_A.into(),
        };
        rig.mail.reply(
            task,
            &note_body(NoteWord::RepairAttempt, None, &repair::facts_text(&facts))?,
        )?;
        old_refs.push(branch);
    }
    main_tip(&rig.host, MAIN_A);
    let first = job(start_in(&rig, &old, &fix1)?);
    assert_eq!(first.branch.as_deref(), Some(old_refs[0].as_str()));
    assert_eq!(rig.store.need_branch(NEED)?.expect("row").run, 1);

    let next = restart(&rig.ctx, &rig.store, &mut rig.mail, &old, "fix1")?;

    assert_eq!(next.run, 2);
    let new_fix1 = rig
        .store
        .steps_of(NEED, next.run)?
        .into_iter()
        .find(|row| row.node == "fix1")
        .expect("new fix1 row");
    main_tip(&rig.host, MAIN_B);
    let spec = job(start_in(&rig, &next, &new_fix1)?);
    let want = format!("vivi/repair/{NEED}-r2-a1");
    assert_eq!(spec.branch.as_deref(), Some(want.as_str()));
    assert_eq!(
        spec.publish_ref.as_deref(),
        Some(format!("refs/heads/{want}").as_str())
    );
    assert!(old_refs.iter().all(|old| *old != want));
    assert!(!spec.body.contains("Prior attempt"));
    let row = rig.store.need_branch(NEED)?.expect("row");
    assert_eq!(row.run, 2);
    assert_eq!(row.base.as_deref(), Some(MAIN_B));
    Ok(())
}

#[test]
fn attempt_two_brief_says_why_attempt_one_changed_nothing() -> R {
    let mut rig = rig(true, &record())?;
    let fix = rig.mail.send_step("mind", "mind", "fix1", "b", &[])?;
    rig.store.insert_step(
        NEED,
        &step("fix1", &fix, StepState::Done, Some("unchanged")),
    )?;
    rig.store.record_step_payload(&StepPayloadRow {
        need: NEED.into(),
        run: 1,
        node: "fix1".into(),
        class: "repairer".into(),
        task: fix.clone(),
        payload: serde_json::json!({
            "job": "j1",
            "reason": "expected a commit on X from base B; actual: no change",
        }),
    })?;
    main_tip(&rig.host, MAIN_B);
    let spec = job(start(&rig, &step("fix2", "t2", StepState::Pending, None))?);
    assert!(spec.body.contains("verdict unchanged"), "{}", spec.body);
    assert!(
        spec.body
            .contains("expected a commit on X from base B; actual: no change"),
        "{}",
        spec.body
    );
    assert!(spec.body.contains("(job j1)"), "{}", spec.body);
    assert!(
        spec.body.contains("Attempt 1 published nothing."),
        "{}",
        spec.body
    );
    Ok(())
}

/// A `prior_context` value in the shape `repair_collect::context` captures:
/// the original job and task, the model-authored summary and the failure
/// excerpt, each bounded on its own.
fn prior_context(summary: &str, failure: &str, source: &str, task: &str) -> serde_json::Value {
    serde_json::json!({
        "source_job": source,
        "task": task,
        "summary": {
            "authored_by": "model",
            "text": summary,
            "bytes": summary.len(),
            "truncated": summary.contains("[cut at 4096 bytes]"),
        },
        "failure": {
            "mode": "markers",
            "text": failure,
            "source_lines": 4,
            "kept_lines": 2,
            "omitted_lines": 2,
            "truncated": false,
        },
    })
}

/// Store a persisted step payload, the way the collector left it.
fn store_payload(rig: &Rig, run: u32, node: &str, task: &str, payload: serde_json::Value) -> R {
    rig.store.record_step_payload(&StepPayloadRow {
        need: NEED.into(),
        run,
        node: node.into(),
        class: "repairer".into(),
        task: task.into(),
        payload,
    })
}

#[test]
fn failed_fix_context_without_rung_renders_the_persisted_account() -> R {
    let mut rig = rig(true, &record())?;
    let fix = rig.mail.send_step("mind", "mind", "fix1", "b", &[])?;
    rig.store
        .insert_step(NEED, &step("fix1", &fix, StepState::Done, Some("failed")))?;
    let summary = "the flake bit again\n[cut at 4096 bytes]";
    store_payload(
        &rig,
        1,
        "fix1",
        &fix,
        serde_json::json!({
            "job": "j-orig",
            "reason": "expected a commit on X from base B; actual: no change",
            "prior_context": prior_context(summary, "error: boom\nthread panicked", "j-orig", &fix),
        }),
    )?;
    main_tip(&rig.host, MAIN_B);
    let spec = job(start(&rig, &step("fix2", "t2", StepState::Pending, None))?);
    let body = &spec.body;
    // The record's own tested commit and the fresh main stay separate.
    assert!(body.contains("Record tested tip: 0000"), "{body}");
    assert!(
        body.contains(&format!("Current main tip: {MAIN_B}")),
        "{body}"
    );
    // The persisted account names its source, its model-authored summary
    // (with its own cut intact) and its failure evidence.
    assert!(
        body.contains(&format!("original job j-orig on task {fix}")),
        "{body}"
    );
    assert!(body.contains("summary (model-authored):"), "{body}");
    assert!(body.contains("  the flake bit again\n"), "{body}");
    assert!(body.contains("  [cut at 4096 bytes]\n"), "{body}");
    assert!(body.contains("failure:\n  error: boom\n"), "{body}");
    assert!(body.contains("  thread panicked\n"), "{body}");
    // The stored reason and job still stand beside the account.
    assert!(
        body.contains("expected a commit on X from base B; actual: no change"),
        "{body}"
    );
    assert!(body.contains("(job j-orig)"), "{body}");
    assert!(!body.contains("Rung verdict"), "{body}");
    Ok(())
}

#[test]
fn prior_context_is_run_local() -> R {
    let mut rig = rig(true, &record())?;
    let stale = rig.mail.send_step("mind", "mind", "fix1", "b", &[])?;
    rig.store
        .insert_step(NEED, &step("fix1", &stale, StepState::Done, Some("failed")))?;
    store_payload(
        &rig,
        1,
        "fix1",
        &stale,
        serde_json::json!({
            "prior_context": prior_context(
                "stale run-one account",
                "stale failure",
                "j-stale",
                &stale,
            ),
        }),
    )?;
    // The current run has its own fix1 row, with no persisted payload: the
    // earlier run's account must not stand in for it.
    let current_fix = rig.mail.send_step("mind", "mind", "fix1", "b", &[])?;
    rig.store.insert_step(
        NEED,
        &StepRow {
            run: 2,
            node: "fix1".into(),
            task: current_fix,
            state: StepState::Done,
            verdict: Some("failed".into()),
        },
    )?;
    let current = RunRow { run: 2, ..run() };
    let fix2 = StepRow {
        run: 2,
        node: "fix2".into(),
        task: "t2".into(),
        state: StepState::Pending,
        verdict: None,
    };
    main_tip(&rig.host, MAIN_B);
    let spec = job(start_in(&rig, &current, &fix2)?);
    assert!(
        spec.body.contains("Repairer account: none (unavailable)"),
        "{}",
        spec.body
    );
    assert!(
        !spec.body.contains("stale run-one account"),
        "{}",
        spec.body
    );
    assert!(!spec.body.contains("j-stale"), "{}", spec.body);
    Ok(())
}

#[test]
fn the_newest_fix_row_and_newest_rung_supply_the_prior_attempt() -> R {
    let mut rig = rig(true, &record())?;
    let old_fix = rig.mail.send_step("mind", "mind", "fix1", "b", &[])?;
    let new_fix = rig.mail.send_step("mind", "mind", "fix1", "b", &[])?;
    rig.store.insert_step(
        NEED,
        &step("fix1", &old_fix, StepState::Done, Some("failed")),
    )?;
    rig.store.insert_step(
        NEED,
        &step("fix1", &new_fix, StepState::Done, Some("failed")),
    )?;
    store_payload(
        &rig,
        1,
        "fix1",
        &old_fix,
        serde_json::json!({
            "job": "j-old",
            "reason": "the stale reason",
            "prior_context": prior_context("stale account", "stale failure", "j-old", &old_fix),
        }),
    )?;
    store_payload(
        &rig,
        1,
        "fix1",
        &new_fix,
        serde_json::json!({
            "job": "j-new",
            "reason": "the newest reason",
            "prior_context": prior_context(
                "the newest account",
                "the newest failure",
                "j-new",
                &new_fix,
            ),
        }),
    )?;
    let facts = AttemptFacts {
        branch: repair::branch(NEED, 1, 1),
        commit: "d".repeat(40),
        base: MAIN_A.into(),
    };
    rig.mail.reply(
        &new_fix,
        &note_body(NoteWord::RepairAttempt, None, &repair::facts_text(&facts))?,
    )?;
    let old_rung = rig.mail.send_step("mind", "mind", "rung1", "b", &[])?;
    let new_rung = rig.mail.send_step("mind", "mind", "rung1", "b", &[])?;
    rig.store.insert_step(
        NEED,
        &step("rung1", &old_rung, StepState::Done, Some("fail")),
    )?;
    rig.store.insert_step(
        NEED,
        &step("rung1", &new_rung, StepState::Done, Some("fail")),
    )?;
    rig.mail.reply(
        &old_rung,
        &note_body(NoteWord::RepairRung, None, "the oldest rung verdict")?,
    )?;
    rig.mail.reply(
        &new_rung,
        &note_body(NoteWord::RepairRung, None, "the newest rung verdict")?,
    )?;
    main_tip(&rig.host, MAIN_B);
    let spec = job(start(&rig, &step("fix2", "t2", StepState::Pending, None))?);
    let body = &spec.body;
    assert!(body.contains("Record tested tip: 0000"), "{body}");
    assert!(
        body.contains(&format!("Current main tip: {MAIN_B}")),
        "{body}"
    );
    assert!(body.contains("original job j-new"), "{body}");
    assert!(body.contains("  the newest account\n"), "{body}");
    assert!(body.contains("  the newest failure\n"), "{body}");
    assert!(!body.contains("stale account"), "{body}");
    assert!(!body.contains("stale failure"), "{body}");
    // The newest fix row's facts, reason and the newest rung verdict remain.
    assert!(body.contains(&"d".repeat(40)), "{body}");
    assert!(body.contains("the newest reason"), "{body}");
    assert!(body.contains("(job j-new)"), "{body}");
    assert!(
        body.contains("Rung verdict: the newest rung verdict"),
        "{body}"
    );
    assert!(!body.contains("the oldest rung verdict"), "{body}");
    Ok(())
}
