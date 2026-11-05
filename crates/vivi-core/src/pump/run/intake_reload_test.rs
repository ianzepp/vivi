//! Intake configuration reloads are live, while removed targets stay until restart.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use tempfile::tempdir;

use super::refresh_config;
use crate::mailspace::Mailspace;
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
    "[pump]\ntick = 1\ncommitter_name = \"Pump\"\ncommitter_email = \"pump@example.com\"\n\n",
    "[pump.dispatch]\nlanes = [{ model = \"m\", cap = 2 }]\n\n",
    "[[identities]]\nname = \"operator\"\n\n",
    "[[identities]]\nname = \"hand\"\nkind = \"hand\"\npool = \"here\"\n\n",
    "[[identities]]\nname = \"mind\"\n\n",
);

fn with_targets(targets: &str) -> String {
    format!("{BASE_TOML}{targets}")
}

fn write_toml(project: &Path, text: &str) {
    fs::write(project.join(".vivi/mailspace.toml"), text).unwrap();
}

fn running(project: &Path) -> (Ctx, Arc<FakeHost>) {
    let mailspace = Mailspace::discover(Some(project)).unwrap();
    let cfg = mailspace.config.pump.clone().unwrap();
    let host = FakeHost::new();
    let ctx = Ctx::new(
        cfg.clone(),
        host.clone(),
        project.to_path_buf(),
        String::from("vivi"),
    )
    .with_routing(crate::routing::from_mailspace(&mailspace.config))
    .with_actor(actor_string(&mailspace))
    .with_mailspace(mailspace.mailspace_id().to_string())
    .with_pool_sources(cfg.pool_sources(&mailspace.config, project));
    (ctx, host)
}

fn exceptions(ctx: &Ctx) -> Vec<String> {
    fs::read_to_string(ctx.home.join("exceptions.txt"))
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains("config-reload-refused"))
        .map(str::to_owned)
        .collect()
}

fn target(id: &str, branch: &str, rung: &str) -> String {
    format!(
        "[pump.targets.{id}]\npath = \"{id}\"\nintake = [\"{branch}\"]\nintake_rung = \"{rung}\"\n\n"
    )
}

fn setup(project: &Path, targets: &str) {
    fs::create_dir_all(project.join(".vivi")).unwrap();
    for id in ["alpha", "beta"] {
        fs::create_dir_all(project.join(id)).unwrap();
    }
    write_toml(project, &with_targets(targets));
}

#[test]
fn intake_lists_and_selector_reload_per_target_and_invalid_edit_keeps_previous_config() {
    let dir = tempdir().unwrap();
    let original = format!(
        "{}{}",
        target("alpha", "peer/one", "task"),
        target("beta", "peer/stay", "need")
    );
    setup(dir.path(), &original);
    let (mut ctx, _) = running(dir.path());
    let mut fingerprint = None;
    let mut last_error = None;
    refresh_config(&mut ctx, &mut fingerprint, &mut last_error);
    assert_eq!(ctx.cfg.targets["alpha"].intake, ["peer/one"]);
    assert_eq!(ctx.cfg.targets["alpha"].intake_rung, "task");
    assert_eq!(ctx.cfg.targets["beta"].intake, ["peer/stay"]);
    assert_eq!(ctx.cfg.targets["beta"].intake_rung, "need");

    let edited = format!(
        "{}{}",
        target("alpha", "peer/two", "main"),
        target("beta", "peer/stay", "need")
    );
    write_toml(dir.path(), &with_targets(&edited));
    assert!(refresh_config(&mut ctx, &mut fingerprint, &mut last_error));
    assert_eq!(ctx.cfg.targets["alpha"].intake, ["peer/two"]);
    assert_eq!(ctx.cfg.targets["alpha"].intake_rung, "main");
    assert_eq!(ctx.cfg.targets["beta"].intake, ["peer/stay"]);
    assert_eq!(ctx.cfg.targets["beta"].intake_rung, "need");

    let invalid = format!(
        "{}{}",
        target("alpha", "peer/bad", "invalid"),
        target("beta", "peer/stay", "need")
    );
    write_toml(dir.path(), &with_targets(&invalid));
    assert!(!refresh_config(&mut ctx, &mut fingerprint, &mut last_error));
    assert_eq!(ctx.cfg.targets["alpha"].intake, ["peer/two"]);
    assert_eq!(ctx.cfg.targets["alpha"].intake_rung, "main");
    assert_eq!(ctx.cfg.targets["beta"].intake, ["peer/stay"]);
}

#[test]
fn removing_an_intake_branch_from_running_target_applies_without_retiring_target() {
    let dir = tempdir().unwrap();
    let original = format!(
        "{}{}",
        target("alpha", "peer/one", "task"),
        target("beta", "peer/stay", "need")
    );
    setup(dir.path(), &original);
    let (mut ctx, _) = running(dir.path());
    let mut fingerprint = None;
    let mut last_error = None;
    refresh_config(&mut ctx, &mut fingerprint, &mut last_error);

    let removed = format!(
        "[pump.targets.alpha]\npath = \"alpha\"\nintake = []\nintake_rung = \"task\"\n\n{}",
        target("beta", "peer/stay", "need")
    );
    write_toml(dir.path(), &with_targets(&removed));
    assert!(refresh_config(&mut ctx, &mut fingerprint, &mut last_error));
    assert!(ctx.cfg.targets["alpha"].intake.is_empty());
    assert_eq!(ctx.cfg.targets["beta"].intake, ["peer/stay"]);
    assert_eq!(step_inputs::targets(&ctx), ["alpha", "beta"]);
    assert!(
        exceptions(&ctx).is_empty(),
        "branch removal is not target removal"
    );
}

#[test]
fn removing_running_target_is_refused_until_restart() {
    let dir = tempdir().unwrap();
    let alpha = target("alpha", "peer/one", "task");
    setup(dir.path(), &alpha);
    let (mut ctx, _) = running(dir.path());
    let mut fingerprint = None;
    let mut last_error = None;
    refresh_config(&mut ctx, &mut fingerprint, &mut last_error);

    write_toml(dir.path(), &with_targets(""));
    assert!(refresh_config(&mut ctx, &mut fingerprint, &mut last_error));
    assert!(ctx.cfg.targets.contains_key("alpha"));
    let lines = exceptions(&ctx);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("config-reload-refused"));
    assert!(lines[0].contains("target alpha was removed"));
}
