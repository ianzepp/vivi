//! `vivi pool list | show | add | set | unset | remove | bands`.
#![allow(clippy::missing_errors_doc)]

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use vivi::VivariumError;
use vivi::cli::{
    PoolAddCommand, PoolBandsCommand, PoolConfigSetCommand, PoolListCommand, PoolRemoveCommand,
    PoolShowCommand, PoolUnsetCommand,
};
use vivi::mailspace::Mailspace;
use vivi::mailspace::config_edit::{self, GuardedWrite, PoolAdd};
use vivi::pool::PoolConfig;
use vivi::pool::config::check_pool_name;
use vivi::pool::work::open_dispatches;

pub(crate) fn list(command: &PoolListCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let rows: Vec<PoolRow> = mailspace
        .config
        .pools
        .iter()
        .map(|(name, pool)| row(name, pool, None))
        .collect();
    if command.json {
        return print_json(&rows);
    }
    for entry in &rows {
        if command.porcelain {
            println!("pool {} {} {}", entry.name, entry.provider, entry.slots);
        } else {
            println!(
                "{}  provider={}  slots={}",
                entry.name, entry.provider, entry.slots
            );
        }
    }
    Ok(())
}

pub(crate) fn show(command: &PoolShowCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let pool = mailspace
        .config
        .pools
        .get(&command.name)
        .ok_or_else(|| VivariumError::Message(format!("unknown pool '{}'", command.name)))?;
    let free = command.resolved.then(|| live_free(&command.name));
    let entry = row(&command.name, pool, free);
    if command.json {
        return print_json(&entry);
    }
    if command.porcelain {
        println!(
            "pool {} {} {} {}",
            entry.name,
            entry.provider,
            entry.slots,
            entry.free.map_or_else(|| "-".into(), |n| n.to_string())
        );
        return Ok(());
    }
    print_show(&entry, pool);
    Ok(())
}

fn print_show(entry: &PoolRow, pool: &PoolConfig) {
    println!(
        "{}  provider={}  slots={}",
        entry.name, entry.provider, entry.slots
    );
    if let Some(free) = entry.free {
        println!("  free: {free}/{}", entry.slots);
    }
    if let Some(path) = &pool.local.worktree_of {
        println!("  worktree_of: {path}");
    }
    if let Some(harness) = &pool.harness {
        println!("  models: {}", harness.models.join(", "));
    }
    if !pool.caps.is_empty() {
        println!("  caps: {:?}", pool.caps);
    }
    for (key, choices) in &entry.bands {
        let shown: Vec<String> = choices
            .iter()
            .map(|c| format!("{} ({})", c.model, c.reasoning))
            .collect();
        println!("  {key}: {}", shown.join(", "));
    }
}

pub(crate) fn add(command: &PoolAddCommand) -> Result<(), VivariumError> {
    check_pool_name(&command.name)?;
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let write = config_edit::guarded(&mailspace.root, |doc| {
        config_edit::upsert_pool(
            doc,
            &PoolAdd {
                name: command.name.clone(),
                provider: command.provider.clone(),
                slots: command.slots,
                worktree_of: command.worktree_of.clone(),
                repo: command.repo.clone(),
                image: command.image.clone(),
                root: command.root.clone(),
            },
        )
    })?;
    println!(
        "added pool {} (provider {} slots {})",
        command.name, command.provider, command.slots
    );
    print_notes(&write);
    Ok(())
}

pub(crate) fn set(command: &PoolConfigSetCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let write = config_edit::guarded(&mailspace.root, |doc| {
        config_edit::set_pool_key(doc, &command.name, &command.key, &command.value)
    })?;
    println!(
        "set pools.{} {} = {}",
        command.name, command.key, command.value
    );
    print_notes(&write);
    Ok(())
}

pub(crate) fn unset(command: &PoolUnsetCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let write = config_edit::guarded(&mailspace.root, |doc| {
        config_edit::unset_pool_key(doc, &command.name, &command.key)
    })?;
    println!("unset pools.{} {}", command.name, command.key);
    print_notes(&write);
    Ok(())
}

pub(crate) fn remove(command: &PoolRemoveCommand) -> Result<(), VivariumError> {
    if !command.yes {
        return Err(VivariumError::Message("pool remove requires --yes".into()));
    }
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    if mailspace.config.pools.get(&command.name).is_none() {
        return Err(VivariumError::Message(format!(
            "unknown pool '{}'",
            command.name
        )));
    }
    refuse_busy(&mailspace, &command.name, command.force)?;
    let write = config_edit::guarded(&mailspace.root, |doc| {
        config_edit::remove_pool(doc, &command.name)
    })?;
    println!("removed pool {}", command.name);
    print_notes(&write);
    Ok(())
}

/// Print a guarded write's backup and still-invalid lines on stderr.
fn print_notes(write: &GuardedWrite) {
    for note in write.notes() {
        eprintln!("{note}");
    }
}

fn refuse_busy(mailspace: &Mailspace, name: &str, force: bool) -> Result<(), VivariumError> {
    let roles: Vec<_> = mailspace
        .config
        .identities
        .iter()
        .filter(|role| role.pool.as_deref() == Some(name))
        .map(|role| role.name.as_str())
        .collect();
    let open = open_dispatches(mailspace)?
        .into_iter()
        .filter(|job| job.record.pool == name)
        .count();
    if (roles.is_empty() && open == 0) || force {
        return Ok(());
    }
    Err(VivariumError::Message(format!(
        "pool '{name}' is still referenced by roles [{}] and {open} open dispatch(es); pass --force to remove it",
        roles.join(", ")
    )))
}

pub(crate) fn bands(command: &PoolBandsCommand) -> Result<(), VivariumError> {
    match command {
        PoolBandsCommand::Set {
            pool,
            band,
            model,
            reasoning,
            project,
        } => bands_set(project.as_deref(), pool, band, model, reasoning.as_deref()),
        PoolBandsCommand::Unset {
            pool,
            band,
            project,
        } => bands_unset(project.as_deref(), pool, band),
    }
}

fn bands_set(
    project: Option<&Path>,
    pool: &str,
    band: &str,
    model: &str,
    reasoning: Option<&str>,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let effort = reasoning.unwrap_or("xhigh");
    let write = config_edit::guarded(&mailspace.root, |doc| {
        config_edit::set_band(doc, pool, band, model, effort)
    })?;
    println!("set pools.{pool}.bands.{band} {model} {effort}");
    print_notes(&write);
    Ok(())
}

fn bands_unset(project: Option<&Path>, pool: &str, band: &str) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let write = config_edit::guarded(&mailspace.root, |doc| {
        config_edit::unset_band(doc, pool, band)
    })?;
    println!("unset pools.{pool}.bands.{band}");
    print_notes(&write);
    Ok(())
}

#[derive(Serialize)]
struct PoolRow {
    name: String,
    provider: String,
    slots: u32,
    free: Option<u32>,
    bands: BTreeMap<String, Vec<BandRow>>,
    harness_models: Vec<String>,
}

#[derive(Serialize)]
struct BandRow {
    model: String,
    reasoning: String,
}

fn row(name: &str, pool: &PoolConfig, free: Option<u32>) -> PoolRow {
    PoolRow {
        name: name.to_string(),
        provider: pool.provider.clone(),
        slots: pool.slots,
        free,
        bands: pool
            .bands
            .entries
            .iter()
            .map(|(key, choices)| {
                let rows = choices
                    .iter()
                    .map(|c| BandRow {
                        model: c.model.clone(),
                        reasoning: c.reasoning.clone(),
                    })
                    .collect();
                (key.clone(), rows)
            })
            .collect(),
        harness_models: pool
            .harness
            .as_ref()
            .map_or_else(Vec::new, |h| h.models.clone()),
    }
}

fn live_free(name: &str) -> u32 {
    let out = std::process::Command::new(std::env::current_exe().unwrap_or_else(|_| "vivi".into()))
        .args(["pool", "status", "--pool", name, "--porcelain"])
        .output();
    let Ok(out) = out else {
        return 0;
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            tokens.len() >= 5 && tokens[0] == "slot" && tokens[4] == "free"
        })
        .count() as u32
}

fn print_json<T: Serialize>(value: &T) -> Result<(), VivariumError> {
    println!(
        "{}",
        serde_json::to_string_pretty(value)
            .map_err(|e| VivariumError::Other(format!("failed to encode JSON: {e}")))?
    );
    Ok(())
}
