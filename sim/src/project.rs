//! A throwaway project: a real local mailspace with a ready pump config, and
//! the pump context built from it the way `vivi pump run` builds its own.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use tempfile::TempDir;
use vivi_core::mailspace::{Mailspace, RoleUpdate};
use vivi_core::pump::host::Ctx;
use vivi_core::pump::run::actor_string;

use crate::host::SimHost;
use crate::scenario::Scenario;

pub struct Project {
    /// Held so the directory lives as long as the run.
    pub _dir: TempDir,
    pub ctx: Ctx,
    pub host: Arc<SimHost>,
}

/// The roles a ready pump needs, with their kinds. All route to `worker-1`.
const ROLES: [(&str, &str); 5] = [
    ("hand", "hand"),
    ("merge", "merge"),
    ("pump-planner", "planner"),
    ("auditor", "auditor"),
    ("mind", "mind"),
];

/// Build the project for `scenario`.
pub fn build(scenario: &Scenario) -> Result<Project, String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    write_config(dir.path(), scenario).map_err(|e| e.to_string())?;
    let mailspace = Mailspace::open_root(dir.path().to_path_buf()).map_err(|e| e.to_string())?;
    let mut cfg = mailspace.config.pump.clone().ok_or("no [pump] table")?;
    // As `vivi pump run` does: a mailspace with no [sync] table has nothing to pull.
    cfg.board.pull &= mailspace.config.sync.is_some();
    let sources = cfg.pool_sources(&mailspace.config, dir.path());
    let host = Arc::new(SimHost::new());
    let ctx = Ctx::new(
        cfg,
        host.clone(),
        dir.path().to_path_buf(),
        String::from("vivi"),
    )
    .with_routing(vivi_core::routing::from_mailspace(&mailspace.config))
    .with_actor(actor_string(&mailspace))
    .with_mailspace(mailspace.mailspace_id().to_string())
    .with_pool_sources(sources);
    fs::create_dir_all(&ctx.repo).map_err(|e| e.to_string())?;
    Ok(Project {
        _dir: dir,
        ctx,
        host,
    })
}

/// Write a ready `mailspace.toml`: the starter roles and pump table, one pool
/// per simulated worker, every role on `worker-1`, a committer, and the tick.
fn write_config(root: &Path, scenario: &Scenario) -> Result<(), vivi_error::VivariumError> {
    let mut space = Mailspace::init(Some(root))?;
    space.ensure_operator_role()?;
    space.ensure_merge_role()?;
    space.ensure_pump_planner_role()?;
    space.ensure_starter_pump()?;
    let path = space.config_path();
    let mut text = fs::read_to_string(&path)?;
    for n in 1..=scenario.world.workers {
        write!(
            text,
            "\n[pools.worker-{n}]\nprovider = \"local\"\nslots = {slots}\n\n\
             [pools.worker-{n}.harness]\nname = \"sim\"\nrun = \"sim --model {{model}} @{{brief}}\"\n\
             models = [\"sim-model\"]\n\n[pools.worker-{n}.bands]\n\
             P2 = [{{ model = \"sim-model\", reasoning = \"low\" }}]\n",
            slots = scenario.world.slots,
        )
        .expect("writing to a String");
    }
    let text = text
        .replace("committer_name = \"\"", "committer_name = \"Sim\"")
        .replace(
            "committer_email = \"\"",
            "committer_email = \"sim@example.com\"",
        );
    fs::write(&path, set_tick(&text, scenario.run.tick_secs))?;
    let mut space = Mailspace::open_root(root.to_path_buf())?;
    for (name, kind) in ROLES {
        let update = RoleUpdate {
            kind: Some(Some(kind.to_owned())),
            pool: Some(Some(String::from("worker-1"))),
            model: Some(Some(String::from("P2"))),
            ..RoleUpdate::default()
        };
        if space.role_view(name).is_ok() {
            space.set_role(name, update)?;
        } else {
            space.add_role(name, update)?;
        }
    }
    Ok(())
}

/// `text` with the `[pump]` table's `tick` set to `secs`.
fn set_tick(text: &str, secs: u64) -> String {
    let mut out = String::new();
    let mut in_pump = false;
    let mut done = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if in_pump && !done {
                writeln!(out, "tick = {secs}").expect("writing to a String");
                done = true;
            }
            in_pump = trimmed == "[pump]";
        } else if in_pump && trimmed.starts_with("tick =") {
            writeln!(out, "tick = {secs}").expect("writing to a String");
            done = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if in_pump && !done {
        writeln!(out, "tick = {secs}").expect("writing to a String");
    }
    out
}
