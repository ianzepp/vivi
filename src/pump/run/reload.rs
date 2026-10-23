//! Live config reload between pump ticks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{actor_string, pump_config};
use crate::VivariumError;
use crate::mailspace::Mailspace;
use crate::pump::config::PumpConfig;
use crate::pump::host::Ctx;

/// Refresh the live mailspace config between ticks. The pump home and target
/// checkout paths stay fixed for the life of the lock; valid pool, routing,
/// dispatch, merge, gate, budget, and check changes apply to the next tick.
pub(in crate::pump) fn refresh_config(
    ctx: &mut Ctx,
    fingerprint: &mut Option<Vec<u8>>,
    last_error: &mut Option<String>,
) -> bool {
    let loaded = match load_runtime_config(&ctx.project) {
        Ok(loaded) => loaded,
        Err(error) => return report_config_error(ctx, last_error, error.to_string()),
    };
    if fingerprint.as_ref() == Some(&loaded.fingerprint) {
        *last_error = None;
        return false;
    }
    if !runtime_paths_match(ctx, &loaded.cfg) {
        return report_config_error(
            ctx,
            last_error,
            String::from("pump home or repository target paths changed; restart required"),
        );
    }
    let changed = fingerprint.is_some();
    ctx.shell_prefix.clone_from(&loaded.cfg.shell_prefix);
    ctx.cfg = Arc::new(loaded.cfg);
    ctx.routing = Arc::new(loaded.routing);
    ctx.pool_sources = Arc::new(loaded.pool_sources);
    ctx.actor = loaded.actor;
    ctx.mailspace = loaded.mailspace;
    *fingerprint = Some(loaded.fingerprint);
    *last_error = None;
    if changed {
        ctx.log("pump config reloaded");
    }
    changed
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

/// Reject live changes that would move the lock or an integration checkout.
fn runtime_paths_match(ctx: &Ctx, cfg: &PumpConfig) -> bool {
    if cfg.home_dir(&ctx.project) != ctx.home {
        return false;
    }
    runtime_repo_paths(&ctx.cfg, &ctx.project) == runtime_repo_paths(cfg, &ctx.project)
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

fn report_config_error(ctx: &Ctx, last_error: &mut Option<String>, error: String) -> bool {
    if last_error.as_deref() != Some(error.as_str()) {
        ctx.log(&format!("pump config reload skipped: {error}"));
    }
    *last_error = Some(error);
    false
}
