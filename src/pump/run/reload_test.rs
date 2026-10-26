//! Tests for [`super::refresh_config`]: targets added live are cloned by the
//! startup code, and a moved or removed target is refused with one exception
//! while the rest of the reload applies.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use tempfile::tempdir;

use super::refresh_config;
use crate::mailspace::Mailspace;
use crate::pump::flow::def::NodeClass;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::step_inputs;
use crate::pump::host::{Ctx, FakeHost};
use crate::pump::run::actor_string;

const BASE_TOML: &str = concat!(
    "name = \"pump\"\n\n",
    "[[identities]]\nname = \"pump-planner\"\nkind = \"planner\"\npool = \"here\"\nmodel = \"P2\"\n\n",
    "[[identities]]\nname = \"merge\"\nkind = \"merge\"\npool = \"here\"\nmodel = \"P2\"\n\n",
    "[[identities]]\nname = \"auditor\"\nkind = \"auditor\"\npool = \"here\"\nmodel = \"P2\"\n\n",
    "[pools.here]\nprovider = \"local\"\nslots = 1\n\n",
    "[pools.here.harness]\nname = \"fake\"\nrun = \"fake --model {model} @{brief}\"\nmodels = [\"m/ok\"]\n\n",
    "[pools.here.bands]\nP2 = [{ model = \"m/ok\", reasoning = \"high\" }]\n\n",
    "[pump]\ntick = 1\n",
    "committer_name = \"Pump\"\ncommitter_email = \"pump@example.com\"\n\n",
    "[pump.dispatch]\nlanes = [{ model = \"m\", cap = CAP }]\n\n",
    "[[identities]]\nname = \"operator\"\n\n",
    "[[identities]]\nname = \"hand\"\nkind = \"hand\"\npool = \"here\"\n\n",
    "[[identities]]\nname = \"mind\"\n\n",
);

/// The mailspace file: `cap` on the lane plus the given target tables.
fn toml(cap: u32, targets: &str) -> String {
    format!("{}{targets}", BASE_TOML.replace("CAP", &cap.to_string()))
}

fn write_toml(project: &Path, text: &str) {
    fs::write(project.join(".vivi").join("mailspace.toml"), text).unwrap();
}

/// A git-repo-looking target source directory under the project.
fn source(project: &Path, name: &str) {
    fs::create_dir_all(project.join(name)).unwrap();
}

/// A running context for the mailspace file already written under `project`.
fn running(project: &Path) -> (Ctx, Arc<FakeHost>) {
    let mailspace = Mailspace::discover(Some(project)).unwrap();
    let cfg = mailspace.config.pump.clone().unwrap();
    let host = FakeHost::new();
    let pool_sources = cfg.pool_sources(&mailspace.config, project);
    let ctx = Ctx::new(
        cfg,
        host.clone(),
        project.to_path_buf(),
        String::from("vivi"),
    )
    .with_routing(crate::routing::from_mailspace(&mailspace.config))
    .with_actor(actor_string(&mailspace))
    .with_mailspace(mailspace.mailspace_id().to_string())
    .with_pool_sources(pool_sources);
    (ctx, host)
}

/// The mailspace file with an explicit `[pump.dispatch]` fallback budget and
/// the given `[pump.dispatch.step_budgets]` lines.
fn budgeted_toml(cap: u32, budget: u64, step_budgets: &str, targets: &str) -> String {
    let lanes = format!("lanes = [{{ model = \"m\", cap = {cap} }}]");
    toml(cap, targets).replacen(
        &format!("[pump.dispatch]\n{lanes}"),
        &format!(
            "[pump.dispatch]\nbudget = {budget}\n{lanes}\n\n[pump.dispatch.step_budgets]\n{step_budgets}",
        ),
        1,
    )
}

/// A live build run with one planner step to build role jobs from.
fn run_row() -> (RunRow, StepRow) {
    (
        RunRow {
            need: String::from("n1"),
            run: 1,
            flow: String::from("build"),
            flow_version: crate::pump::flow::index::FLOW_VERSION_NUMBER,
            kind: String::from("build"),
            entry: String::from("plan"),
            state: String::from(crate::pump::flow::index::run_state::LIVE),
        },
        StepRow {
            run: 1,
            node: String::from("plan"),
            task: String::from("t1"),
            state: StepState::Waiting,
            verdict: None,
        },
    )
}

fn exceptions(ctx: &Ctx) -> Vec<String> {
    fs::read_to_string(ctx.home.join("exceptions.txt"))
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains("config-reload-refused"))
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_target_added_live_is_cloned_and_listed_at_the_next_refresh() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".vivi")).unwrap();
    source(dir.path(), "alpha");
    source(dir.path(), "beta");
    let alpha = "[pump.targets.alpha]\npath = \"alpha\"\n\n";
    write_toml(dir.path(), &toml(2, alpha));
    let (mut ctx, host) = running(dir.path());
    let mut fingerprint = None;
    let mut last_error = None;
    assert!(!refresh_config(&mut ctx, &mut fingerprint, &mut last_error));

    let both = format!("{alpha}[pump.targets.beta]\npath = \"beta\"\n\n");
    write_toml(dir.path(), &toml(7, &both));
    assert!(refresh_config(&mut ctx, &mut fingerprint, &mut last_error));

    assert_eq!(ctx.cfg.dispatch.lanes[0].cap, 7);
    assert_eq!(step_inputs::targets(&ctx), ["alpha", "beta"]);
    let beta_repo = ctx.cfg.target_repo_dir(&ctx.project, "beta").unwrap();
    assert!(
        host.commands().iter().any(|command| {
            command.contains("git clone") && command.contains(&beta_repo.display().to_string())
        }),
        "{:?}",
        host.commands()
    );
    assert!(last_error.is_none());
    assert!(exceptions(&ctx).is_empty());
}

#[test]
fn a_moved_target_keeps_its_checkout_and_raises_one_exception() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".vivi")).unwrap();
    source(dir.path(), "alpha");
    let alpha = "[pump.targets.alpha]\npath = \"alpha\"\n\n";
    write_toml(dir.path(), &toml(2, alpha));
    let (mut ctx, _host) = running(dir.path());
    let mut fingerprint = None;
    let mut last_error = None;
    refresh_config(&mut ctx, &mut fingerprint, &mut last_error);
    let old_repo = ctx.cfg.target_repo_dir(&ctx.project, "alpha").unwrap();

    let moved = "[pump.targets.alpha]\npath = \"alpha\"\nintegration_repo = \"elsewhere\"\n\n";
    write_toml(dir.path(), &toml(7, moved));
    assert!(refresh_config(&mut ctx, &mut fingerprint, &mut last_error));
    assert_eq!(ctx.cfg.dispatch.lanes[0].cap, 7);
    assert_eq!(
        ctx.cfg.target_repo_dir(&ctx.project, "alpha").unwrap(),
        old_repo
    );
    assert_eq!(exceptions(&ctx).len(), 1, "{:?}", exceptions(&ctx));
    assert!(
        exceptions(&ctx)[0].contains("target alpha was moved"),
        "{:?}",
        exceptions(&ctx)
    );

    // The same file again, then another edit that still carries the move:
    // neither raises a second exception.
    assert!(!refresh_config(&mut ctx, &mut fingerprint, &mut last_error));
    write_toml(dir.path(), &toml(9, moved));
    assert!(refresh_config(&mut ctx, &mut fingerprint, &mut last_error));
    assert_eq!(ctx.cfg.dispatch.lanes[0].cap, 9);
    assert_eq!(exceptions(&ctx).len(), 1);
}

#[test]
fn a_removed_target_stays_running_and_a_failed_clone_drops_only_the_new_target() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".vivi")).unwrap();
    source(dir.path(), "alpha");
    source(dir.path(), "beta");
    let alpha = "[pump.targets.alpha]\npath = \"alpha\"\n\n";
    write_toml(dir.path(), &toml(2, alpha));
    let (mut ctx, host) = running(dir.path());
    let mut fingerprint = None;
    let mut last_error = None;
    refresh_config(&mut ctx, &mut fingerprint, &mut last_error);

    host.reply("git clone", 128, "fatal: no such remote");
    let beta_only = "[pump.targets.beta]\npath = \"beta\"\n\n";
    write_toml(dir.path(), &toml(7, beta_only));
    assert!(refresh_config(&mut ctx, &mut fingerprint, &mut last_error));
    assert_eq!(ctx.cfg.dispatch.lanes[0].cap, 7);
    assert_eq!(step_inputs::targets(&ctx), ["alpha"]);
    let lines = exceptions(&ctx);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("target alpha was removed"), "{lines:?}");
    assert!(lines[0].contains("target beta clone failed"), "{lines:?}");
}

#[test]
fn a_reloaded_budget_reaches_new_jobs_and_leaves_queued_ones_alone() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".vivi")).unwrap();
    write_toml(dir.path(), &budgeted_toml(2, 421, "planner = 97", ""));
    let (mut ctx, _host) = running(dir.path());
    let mut fingerprint = None;
    let mut last_error = None;
    refresh_config(&mut ctx, &mut fingerprint, &mut last_error);
    assert_eq!(ctx.cfg.dispatch.envelope_fix_budget(), 300);

    // The planner job is built through the real read-only role-job path and
    // captures the startup override.
    let (run, step) = run_row();
    let queued = step_inputs::read_only_job(
        &ctx,
        &run,
        &step,
        &NodeClass::Planner,
        "pump-planner",
        "brief one",
    )
    .unwrap();
    assert_eq!(queued.budget, 97);

    write_toml(dir.path(), &budgeted_toml(2, 233, "planner = 151", ""));
    assert!(refresh_config(&mut ctx, &mut fingerprint, &mut last_error));
    assert!(last_error.is_none());
    assert!(exceptions(&ctx).is_empty());
    assert_eq!(ctx.cfg.dispatch.budget, 233);
    assert_eq!(ctx.cfg.dispatch.step_budgets.planner, Some(151));

    let fresh = step_inputs::read_only_job(
        &ctx,
        &run,
        &step,
        &NodeClass::Planner,
        "pump-planner",
        "brief one",
    )
    .unwrap();
    assert_eq!(fresh.budget, 151);
    // An omitted thinking-kind override follows the reloaded fallback.
    let judge =
        step_inputs::read_only_job(&ctx, &run, &step, &NodeClass::Judge, "auditor", "brief two")
            .unwrap();
    assert_eq!(judge.budget, 233);
    assert_eq!(ctx.cfg.dispatch.envelope_fix_budget(), 300);

    // The queued job keeps its original budget and execution inputs.
    assert_eq!(queued.budget, 97);
    assert_eq!(queued.id, "step-t1-a1");
    assert_eq!(queued.kind, "planner");
    assert_eq!(queued.role, "pump-planner");
    assert_eq!(queued.task.as_deref(), Some("t1"));
    assert_eq!(queued.body, "brief one");
    assert!(queued.read_only);
    assert_eq!(queued.from_ref.as_deref(), Some(ctx.cfg.main.as_str()));
    assert_eq!(queued.extra_targets, step_inputs::targets(&ctx));
}

#[test]
fn a_changed_pump_home_keeps_the_running_home_and_still_applies_the_rest() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".vivi")).unwrap();
    write_toml(dir.path(), &toml(2, ""));
    let (mut ctx, _host) = running(dir.path());
    let home = ctx.home.clone();
    let mut fingerprint = None;
    let mut last_error = None;
    refresh_config(&mut ctx, &mut fingerprint, &mut last_error);

    let text = toml(7, "").replace("[pump]\n", "[pump]\nhome = \"other-home\"\n");
    write_toml(dir.path(), &text);
    assert!(refresh_config(&mut ctx, &mut fingerprint, &mut last_error));
    assert_eq!(ctx.cfg.dispatch.lanes[0].cap, 7);
    assert_eq!(ctx.cfg.home_dir(&ctx.project), home);
    assert_eq!(exceptions(&ctx).len(), 1);
}
