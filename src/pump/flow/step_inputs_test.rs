//! Tests for the shared step-brief readers.

use tempfile::{TempDir, tempdir};

use super::{
    POOL_RESULT_MARKER, done_step, make_read_only, need_target, need_thread, predecessor_step,
    read_only_job, role_job, step_replies, targets,
};
use crate::error::VivariumError;
use crate::letter::fixture::need_body_with;
use crate::letter::work::NeedFront;
use crate::pump::config::{PumpConfig, PumpTarget};
use crate::pump::envelope::roles::RoleKind;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::fake_mail::{FakeMail, FakeTask};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::{MailWriter, TaskState, TaskStatus};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const NEED: &str = "n1";

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    host: std::sync::Arc<FakeHost>,
    store: PumpStore,
    run: RunRow,
}

fn rig() -> Result<Rig, VivariumError> {
    rig_with(PumpConfig::default())
}

fn rig_with(cfg: PumpConfig) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), cfg);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let run = RunRow {
        need: NEED.into(),
        run: 1,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    };
    store.insert_run(&run)?;
    Ok(Rig {
        _dir: dir,
        ctx,
        host,
        store,
        run,
    })
}

fn row(node: &str, task: &str, state: StepState) -> StepRow {
    StepRow {
        run: 1,
        node: node.into(),
        task: task.into(),
        state,
        verdict: None,
    }
}

fn result(job: &str, word: &str, message: &str) -> String {
    format!("{POOL_RESULT_MARKER}{job}: {word}\npool p  slot 1  model m\n\n--- receipt\n{message}")
}

#[test]
fn step_replies_are_newest_first_and_reduced_to_the_agent_message() -> R {
    let mut mail = FakeMail::new("pump");
    let task = mail.send_step("pump", "pump", "plan", "body", &[])?;
    mail.reply(&task, &result("j1", "finished", "first answer"))?;
    mail.reply(&task, "unrelated note")?;
    mail.reply(&task, &result("j2", "finished", "second answer"))?;

    let replies = step_replies(&mail, &task)?;

    assert_eq!(replies, vec!["second answer", "first answer"]);
    Ok(())
}

#[test]
fn need_thread_fails_on_a_nonzero_exit_naming_the_need() -> R {
    let rig = rig()?;
    rig.host.reply("need show", 1, "boom");

    let err = need_thread(&rig.ctx, NEED).unwrap_err();

    assert!(err.to_string().contains(NEED), "{err}");
    Ok(())
}

#[test]
fn need_thread_keeps_a_short_thread_and_cuts_a_long_one() -> R {
    let rig = rig()?;
    rig.host.reply("need show", 0, "short thread");
    assert_eq!(need_thread(&rig.ctx, NEED)?, "short thread");

    rig.host.reply("need show", 0, &"x".repeat(40_001));
    let cut = need_thread(&rig.ctx, NEED)?;
    assert_eq!(cut, format!("{}\n[thread cut]", "x".repeat(40_000)));
    Ok(())
}

/// Store `body` verbatim as the letter under `handle`: for bodies the
/// store_need fixture cannot express (malformed TOML, legacy prose).
fn stored_body(mail: &mut FakeMail, handle: &str, body: &str) {
    mail.tasks.insert(
        handle.to_string(),
        FakeTask {
            from: "pump".to_string(),
            to: "hand".to_string(),
            subject: "original need".to_string(),
            body: body.to_string(),
            depends_on: Vec::new(),
            replies: Vec::new(),
            state: TaskState {
                status: TaskStatus::Open,
                closer: None,
                verdict: None,
            },
            branch: None,
            commit: None,
        },
    );
}

#[test]
fn need_target_reads_the_typed_target_of_a_targeted_need() -> R {
    let mut mail = FakeMail::new("pump");
    mail.store_need(NEED, Some("vivi"));

    assert_eq!(need_target(&mail, NEED)?, Some("vivi".to_string()));
    Ok(())
}

#[test]
fn need_target_is_none_for_a_valid_untargeted_need() -> R {
    let mut mail = FakeMail::new("pump");
    mail.store_need(NEED, None);

    assert_eq!(need_target(&mail, NEED)?, None);
    Ok(())
}

#[test]
fn need_target_is_an_error_when_the_stored_body_is_missing() -> R {
    let mail = FakeMail::new("pump");

    let err = need_target(&mail, NEED).unwrap_err();

    assert!(matches!(err, VivariumError::Message(_)), "{err}");
    Ok(())
}

#[test]
fn need_target_is_an_error_on_malformed_toml() -> R {
    let mut mail = FakeMail::new("pump");
    stored_body(&mut mail, NEED, "+++\ndone_when = [\n+++\nprose");

    let err = need_target(&mail, NEED).unwrap_err();

    assert!(matches!(err, VivariumError::Message(_)), "{err}");
    Ok(())
}

#[test]
fn need_target_is_an_error_on_invalid_typed_need_front_matter() -> R {
    let mut mail = FakeMail::new("pump");
    let blank = need_body_with(&NeedFront::new(vec!["  ".to_string()]), "prose");
    stored_body(&mut mail, NEED, &blank);

    let err = need_target(&mail, NEED).unwrap_err();

    assert!(matches!(err, VivariumError::Message(_)), "{err}");
    Ok(())
}

#[test]
fn need_target_is_an_error_on_a_legacy_body_without_front_matter() -> R {
    let mut mail = FakeMail::new("pump");
    stored_body(&mut mail, NEED, "plain prose, no front matter");

    let err = need_target(&mail, NEED).unwrap_err();

    assert!(matches!(err, VivariumError::Message(_)), "{err}");
    Ok(())
}

#[test]
fn targets_lists_the_configured_target_ids() -> R {
    let rig = rig()?;
    assert_eq!(targets(&rig.ctx), vec![String::from("default")]);
    Ok(())
}

#[test]
fn done_step_is_the_last_done_row_of_the_node() -> R {
    let rig = rig()?;
    rig.store
        .insert_step(NEED, &row("plan", "t1", StepState::Done))?;
    rig.store
        .insert_step(NEED, &row("plan", "t2", StepState::Done))?;
    rig.store
        .insert_step(NEED, &row("plan", "t3", StepState::Running))?;

    let found = done_step(&rig.store, &rig.run, "plan")?.unwrap();

    assert_eq!(found.task, "t2");
    assert!(done_step(&rig.store, &rig.run, "slice")?.is_none());
    Ok(())
}

#[test]
fn predecessor_step_follows_the_decision_node_to_the_slicer() -> R {
    let rig = rig()?;
    let def = load(FlowKind::Build)?;
    rig.store
        .insert_step(NEED, &row("slice", "ts", StepState::Done))?;
    rig.store
        .insert_step(NEED, &row("rework1", "tr", StepState::Done))?;

    let first = predecessor_step(&def, &rig.store, &rig.run, "build1", &NodeClass::Slicer)?;
    let second = predecessor_step(&def, &rig.store, &rig.run, "build2", &NodeClass::Slicer)?;
    let wrong_class = predecessor_step(&def, &rig.store, &rig.run, "build1", &NodeClass::Planner)?;

    assert_eq!(first.unwrap().node, "slice");
    assert_eq!(second.unwrap().node, "rework1");
    assert!(wrong_class.is_none());
    Ok(())
}

#[test]
fn read_only_job_refuses_a_missing_role() -> R {
    let rig = rig()?;
    let step = row("plan", "t1", StepState::Running);

    let err = read_only_job(
        &rig.ctx,
        &rig.run,
        &step,
        &NodeClass::Planner,
        "nobody",
        "brief",
    )
    .unwrap_err();

    assert!(matches!(err, VivariumError::Config(_)));
    assert!(err.to_string().contains("nobody"), "{err}");
    Ok(())
}

#[test]
fn read_only_job_sets_read_only_from_ref_and_all_targets() -> R {
    let rig = rig()?;
    let step = row("plan", "t1", StepState::Running);

    let spec = read_only_job(
        &rig.ctx,
        &rig.run,
        &step,
        &NodeClass::Planner,
        "pump-planner",
        "the brief",
    )?;

    assert!(spec.read_only);
    assert_eq!(spec.from_ref.as_deref(), Some(rig.ctx.cfg.main.as_str()));
    assert_eq!(spec.extra_targets, vec![String::from("default")]);
    assert_eq!(spec.role, "pump-planner");
    assert_eq!(spec.task.as_deref(), Some("t1"));
    assert!(spec.target.is_none());
    Ok(())
}

#[test]
fn make_read_only_sets_read_only_from_ref_and_all_targets() -> R {
    let rig = rig()?;
    let step = row("plan", "t1", StepState::Running);
    let mut spec = role_job(
        &rig.ctx,
        &rig.run,
        &step,
        "flow-x",
        RoleKind::Planner,
        "pump-planner",
        "the brief",
    );
    assert!(!spec.read_only);
    assert!(spec.from_ref.is_none());

    make_read_only(&rig.ctx, &mut spec)?;

    assert!(spec.read_only);
    assert_eq!(spec.from_ref.as_deref(), Some(rig.ctx.cfg.main.as_str()));
    assert_eq!(spec.extra_targets, targets(&rig.ctx));
    Ok(())
}

#[test]
fn need_target_reads_the_original_typed_need() -> R {
    let mut mail = FakeMail::new("pump");
    mail.store_need("n-targeted", Some("vivi"));
    mail.store_need("n-plain", None);
    stored_body(&mut mail, "n-broken", "+++\ndone_when = [\n+++\nprose");

    assert_eq!(need_target(&mail, "n-targeted")?, Some("vivi".to_string()));
    assert_eq!(need_target(&mail, "n-plain")?, None);
    assert!(matches!(
        need_target(&mail, "n-missing").unwrap_err(),
        VivariumError::Message(_)
    ));
    assert!(matches!(
        need_target(&mail, "n-broken").unwrap_err(),
        VivariumError::Message(_)
    ));
    Ok(())
}

#[test]
fn make_read_only_starts_from_an_explicit_target_over_the_primary() -> R {
    let rig = rig_with(target_config(&[
        ("default", "vivi/main-default"),
        ("alpha", "vivi/main-alpha"),
    ]))?;
    let mut spec = spec_for(&rig);
    spec.target = Some(String::from("alpha"));

    make_read_only(&rig.ctx, &mut spec)?;

    assert!(spec.read_only);
    assert_eq!(spec.from_ref.as_deref(), Some("vivi/main-alpha"));
    assert_eq!(spec.target.as_deref(), Some("alpha"));
    assert_eq!(
        spec.extra_targets,
        vec!["alpha".to_string(), "default".to_string()]
    );
    Ok(())
}

#[test]
fn make_read_only_falls_back_to_the_sole_target() -> R {
    let rig = rig_with(target_config(&[("alpha", "vivi/main-alpha")]))?;
    let mut spec = spec_for(&rig);

    make_read_only(&rig.ctx, &mut spec)?;

    assert!(spec.read_only);
    assert_eq!(spec.from_ref.as_deref(), Some("vivi/main-alpha"));
    assert_eq!(spec.target.as_deref(), Some("alpha"));
    Ok(())
}

#[test]
fn make_read_only_falls_back_to_the_explicit_default() -> R {
    let rig = rig_with(target_config(&[
        ("beta", "vivi/main-beta"),
        ("default", "vivi/main-default"),
    ]))?;
    let mut spec = spec_for(&rig);

    make_read_only(&rig.ctx, &mut spec)?;

    assert!(spec.read_only);
    assert_eq!(spec.from_ref.as_deref(), Some("vivi/main-default"));
    assert_eq!(spec.target.as_deref(), Some("default"));
    Ok(())
}

#[test]
fn make_read_only_resolves_the_legacy_default_without_a_target() -> R {
    let rig = rig()?;
    let mut spec = spec_for(&rig);

    make_read_only(&rig.ctx, &mut spec)?;

    assert!(spec.read_only);
    assert_eq!(spec.target, None, "a legacy configuration leaves it absent");
    assert_eq!(spec.from_ref.as_deref(), Some(rig.ctx.cfg.main.as_str()));
    assert_eq!(spec.extra_targets, vec![String::from("default")]);
    Ok(())
}

#[test]
fn make_read_only_refuses_an_unknown_target_without_changes() -> R {
    let rig = rig_with(target_config(&[("alpha", "vivi/main-alpha")]))?;
    let mut spec = spec_for(&rig);
    spec.target = Some(String::from("nope"));

    let err = make_read_only(&rig.ctx, &mut spec).unwrap_err();

    assert!(matches!(err, VivariumError::Config(_)), "{err}");
    assert!(!spec.read_only, "a failed resolution leaves it unchanged");
    assert!(spec.from_ref.is_none());
    assert!(spec.extra_targets.is_empty());
    assert_eq!(spec.target.as_deref(), Some("nope"));
    Ok(())
}

#[test]
fn make_read_only_refuses_an_untargeted_ambiguous_configuration() -> R {
    let rig = rig_with(target_config(&[
        ("alpha", "vivi/main-alpha"),
        ("beta", "vivi/main-beta"),
    ]))?;
    let mut spec = spec_for(&rig);

    let err = make_read_only(&rig.ctx, &mut spec).unwrap_err();

    assert!(matches!(err, VivariumError::Config(_)), "{err}");
    assert!(!spec.read_only, "a failed resolution leaves it unchanged");
    assert!(spec.from_ref.is_none());
    assert!(spec.extra_targets.is_empty());
    assert_eq!(spec.target, None);
    Ok(())
}

/// Explicit repository targets, each carrying its own literal main.
fn target_config(targets: &[(&str, &str)]) -> PumpConfig {
    let mut cfg = PumpConfig::default();
    cfg.targets = targets
        .iter()
        .map(|(id, main)| {
            (
                (*id).to_string(),
                PumpTarget {
                    main: (*main).to_string(),
                    ..PumpTarget::default()
                },
            )
        })
        .collect();
    cfg
}

/// The plain role job the read-only builders start from.
fn spec_for(rig: &Rig) -> crate::pump::jobs::JobSpec {
    let step = row("plan", "t1", StepState::Running);
    role_job(
        &rig.ctx,
        &rig.run,
        &step,
        "flow-x",
        RoleKind::Planner,
        "pump-planner",
        "the brief",
    )
}

#[test]
fn role_job_resolves_the_budget_from_the_execution_kind_not_the_label() -> R {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.step_budgets.planner = Some(90);
    cfg.dispatch.step_budgets.judge = Some(45);
    let rig = rig_with(cfg)?;
    let step = row("plan", "t1", StepState::Running);

    // A configured thinking-step override applies under any durable label.
    let planner = role_job(
        &rig.ctx,
        &rig.run,
        &step,
        "custom",
        RoleKind::Planner,
        "planner",
        "b",
    );
    assert_eq!(planner.budget, 90);
    let relabeled = role_job(
        &rig.ctx,
        &rig.run,
        &step,
        "other",
        RoleKind::Planner,
        "planner",
        "b",
    );
    assert_eq!(relabeled.budget, 90);

    // The judge budgets as a judge even though its seat is the auditor role.
    let judge = role_job(
        &rig.ctx,
        &rig.run,
        &step,
        "flow-judge",
        RoleKind::Judge,
        "auditor",
        "b",
    );
    assert_eq!(judge.budget, 45);

    // A kind with no step key falls back to the shared budget; the label and
    // seat never change that.
    let auditor = role_job(
        &rig.ctx,
        &rig.run,
        &step,
        "custom",
        RoleKind::Auditor,
        "auditor",
        "b",
    );
    assert_eq!(auditor.budget, rig.ctx.cfg.dispatch.budget);
    let fallback = role_job(
        &rig.ctx,
        &rig.run,
        &step,
        "flow-judge",
        RoleKind::Merge,
        "merge",
        "b",
    );
    assert_eq!(fallback.budget, rig.ctx.cfg.dispatch.budget);
    Ok(())
}

#[test]
fn read_only_job_resolves_the_budget_from_the_node_class() -> R {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.step_budgets.planner = Some(90);
    cfg.dispatch.budget = 777;
    let rig = rig_with(cfg)?;
    let step = row("admit1", "t1", StepState::Running);

    let planner = read_only_job(
        &rig.ctx,
        &rig.run,
        &step,
        &NodeClass::Planner,
        "pump-planner",
        "b",
    )?;
    assert_eq!(planner.budget, 90);
    // The class-based job still carries the read-only checkout context.
    assert!(planner.read_only);
    assert_eq!(planner.from_ref.as_deref(), Some(rig.ctx.cfg.main.as_str()));
    assert_eq!(planner.extra_targets, targets(&rig.ctx));
    // A role class with no thinking key falls back to the changed shared
    // budget, not the built-in default.
    let merge = read_only_job(
        &rig.ctx,
        &rig.run,
        &step,
        &NodeClass::Merge,
        "pump-planner",
        "b",
    )?;
    assert_eq!(merge.budget, 777);
    assert_eq!(merge.budget, rig.ctx.cfg.dispatch.budget);

    // A class that is not a role class is refused, not defaulted.
    let err = read_only_job(
        &rig.ctx,
        &rig.run,
        &step,
        &NodeClass::Decision,
        "pump-planner",
        "b",
    )
    .unwrap_err();
    assert!(matches!(err, VivariumError::Config(_)), "{err}");
    Ok(())
}
