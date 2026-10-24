//! Live config reload between pump ticks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{actor_string, ensure_target_repo, pump_config};
use crate::VivariumError;
use crate::mailspace::Mailspace;
use crate::pump::config::PumpConfig;
use crate::pump::host::Ctx;

/// Refresh the live mailspace config between ticks. Valid pool, routing,
/// dispatch, merge, budget, check, and new-target changes apply to the
/// next tick. The pump home and the integration checkout of a running target
/// stay fixed for the life of the lock: a change to them keeps the running
/// value, the rest of the reload still applies, and one
/// `config-reload-refused` exception names what needs a restart.
pub(in crate::pump) fn refresh_config(
    ctx: &mut Ctx,
    fingerprint: &mut Option<Vec<u8>>,
    last_error: &mut Option<String>,
) -> bool {
    let mut loaded = match load_runtime_config(&ctx.project) {
        Ok(loaded) => loaded,
        Err(error) => return report_config_error(ctx, last_error, error.to_string()),
    };
    if fingerprint.as_ref() == Some(&loaded.fingerprint) {
        return false;
    }
    let mut reasons = keep_running_paths(ctx, &mut loaded.cfg);
    if !running_paths_kept(ctx, &loaded.cfg) {
        return report_config_error(
            ctx,
            last_error,
            String::from("pump home or repository target paths changed; restart required"),
        );
    }
    clone_new_targets(ctx, &mut loaded.cfg, &mut reasons);
    let changed = fingerprint.is_some();
    ctx.shell_prefix.clone_from(&loaded.cfg.shell_prefix);
    ctx.cfg = Arc::new(loaded.cfg);
    ctx.routing = Arc::new(loaded.routing);
    ctx.pool_sources = Arc::new(loaded.pool_sources);
    ctx.actor = loaded.actor;
    ctx.mailspace = loaded.mailspace;
    *fingerprint = Some(loaded.fingerprint);
    if reasons.is_empty() {
        *last_error = None;
    } else {
        raise_once(
            ctx,
            last_error,
            reasons.join("; "),
            "pump config reload kept",
        );
    }
    if changed {
        ctx.log("pump config reloaded");
    }
    changed
}

/// Put the running pump home and the running checkout of every moved or
/// removed target back into `cfg`; one reason per kept value.
fn keep_running_paths(ctx: &Ctx, cfg: &mut PumpConfig) -> Vec<String> {
    let mut reasons = Vec::new();
    if cfg.home_dir(&ctx.project) != ctx.home {
        cfg.home.clone_from(&ctx.cfg.home);
        reasons.push(String::from(
            "pump home changed; restart the pump to apply it",
        ));
    }
    if ctx.cfg.targets.is_empty() || cfg.targets.is_empty() {
        return reasons;
    }
    let new_paths = runtime_repo_paths(cfg, &ctx.project);
    for (id, path) in runtime_repo_paths(&ctx.cfg, &ctx.project) {
        let verb = match new_paths.get(&id) {
            Some(new) if *new == path => continue,
            Some(_) => "moved",
            None => "removed",
        };
        if let Some(target) = ctx.cfg.targets.get(&id) {
            cfg.targets.insert(id.clone(), target.clone());
        }
        reasons.push(format!(
            "target {id} was {verb}; restart the pump to apply it"
        ));
    }
    reasons
}

/// Every running target keeps its running integration checkout in `cfg`.
fn running_paths_kept(ctx: &Ctx, cfg: &PumpConfig) -> bool {
    let new_paths = runtime_repo_paths(cfg, &ctx.project);
    runtime_repo_paths(&ctx.cfg, &ctx.project)
        .iter()
        .all(|(id, path)| new_paths.get(id) == Some(path))
}

/// Make the integration checkout of each target only the new config names,
/// with the startup code. A target whose clone fails is dropped from `cfg`.
fn clone_new_targets(ctx: &Ctx, cfg: &mut PumpConfig, reasons: &mut Vec<String>) {
    let known = runtime_repo_paths(&ctx.cfg, &ctx.project);
    let mut probe = ctx.clone();
    probe.cfg = Arc::new(cfg.clone());
    for (id, target) in cfg.target_configs() {
        if known.contains_key(&id) {
            continue;
        }
        if let Err(error) = ensure_target_repo(&probe, &id, target) {
            cfg.targets.remove(&id);
            reasons.push(format!("target {id} clone failed: {error}"));
        }
    }
}

/// Read and validate one complete runtime config snapshot.
fn load_runtime_config(project: &Path) -> Result<LoadedRuntimeConfig, VivariumError> {
    let mailspace = Mailspace::discover(Some(project))?;
    let cfg = pump_config(&mailspace)?;
    let issues = crate::pump::readiness::assess(&mailspace);
    crate::routing::require_ok(&issues)?;
    if let Some(summary) = crate::routing::not_ready_summary(&issues) {
        return Err(VivariumError::Config(summary));
    }
    let fingerprint = serde_json::to_vec(&mailspace.config)
        .map_err(|error| VivariumError::Message(error.to_string()))?;
    Ok(LoadedRuntimeConfig {
        pool_sources: cfg.pool_sources(&mailspace.config, project),
        routing: crate::routing::from_mailspace(&mailspace.config),
        actor: actor_string(&mailspace),
        mailspace: mailspace.mailspace_id().to_string(),
        cfg,
        fingerprint,
    })
}

struct LoadedRuntimeConfig {
    cfg: PumpConfig,
    routing: crate::routing::RoleRouting,
    pool_sources: BTreeMap<String, PathBuf>,
    actor: String,
    mailspace: String,
    fingerprint: Vec<u8>,
}

fn runtime_repo_paths(cfg: &PumpConfig, project: &Path) -> BTreeMap<String, PathBuf> {
    cfg.target_configs()
        .keys()
        .filter_map(|id| {
            cfg.target_repo_dir(project, id)
                .map(|path| (id.clone(), path))
        })
        .collect()
}

/// A reload that was skipped whole: log and raise it once per distinct text.
fn report_config_error(ctx: &Ctx, last_error: &mut Option<String>, error: String) -> bool {
    raise_once(ctx, last_error, error, "pump config reload skipped");
    false
}

/// Log `what: text` and raise the `config-reload-refused` exception, once per
/// distinct `text`; `last_error` remembers the text last raised.
fn raise_once(ctx: &Ctx, last_error: &mut Option<String>, text: String, what: &str) {
    if last_error.as_deref() != Some(text.as_str()) {
        ctx.log(&format!("{what}: {text}"));
        ctx.exception("config-reload-refused", "pump-config", &text);
    }
    *last_error = Some(text);
}

#[cfg(test)]
#[path = "reload_test.rs"]
mod reload_test;
