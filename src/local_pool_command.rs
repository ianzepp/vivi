use std::time::Duration;

use crate::local_pool_work_command as work;
use vivi::VivariumError;
use vivi::cli::{PoolCreateCommand, PoolDestroyCommand, PoolStatusCommand, PoolSubcommand};
use vivi::mailspace::Mailspace;
use vivi::pool::PoolConfig;
use vivi::pool::fly::FlyctlProvider;
use vivi::pool::github;
use vivi::pool::run::{self, CreateOptions, DestroyTarget, SlotOutcome};
use vivi::pool::slot::SlotStatus;

pub(crate) fn handle_pool_command(command: &PoolSubcommand) -> Result<(), VivariumError> {
    match command {
        PoolSubcommand::Create(c) => create(c),
        PoolSubcommand::Status(c) => status(c),
        PoolSubcommand::Destroy(c) => destroy(c),
        PoolSubcommand::Dispatch(c) => work::dispatch(c),
        PoolSubcommand::Wait(c) => work::wait(c),
        PoolSubcommand::Reconcile(c) => work::reconcile(c),
    }
}

/// Load, validate, and name the pool: `(config, pool name, provider)`.
fn open_pool(
    project: Option<&std::path::Path>,
) -> Result<(PoolConfig, String, FlyctlProvider), VivariumError> {
    let (_, config, pool, provider) = open_pool_space(project)?;
    Ok((config, pool, provider))
}

/// [`open_pool`] plus the mailspace the pool is configured in.
pub(crate) fn open_pool_space(
    project: Option<&std::path::Path>,
) -> Result<(Mailspace, PoolConfig, String, FlyctlProvider), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let Some(config) = mailspace.config.pool.clone() else {
        return Err(VivariumError::Config(
            "no [pool] table in .vivi/mailspace.toml".into(),
        ));
    };
    config.validate(&mailspace.config.name)?;
    let pool = config.pool_name(&mailspace.config.name);
    let provider = FlyctlProvider::new(&config.app);
    Ok((mailspace, config, pool, provider))
}

fn create(command: &PoolCreateCommand) -> Result<(), VivariumError> {
    let (config, pool, provider) = open_pool(command.project.as_deref())?;
    let options = CreateOptions {
        slots: command.slots,
        timeout: Duration::from_secs(command.timeout),
        ..CreateOptions::default()
    };
    let report = run::create_slots(&provider, &config, &pool, &options, || {
        let token = github::mint_token(&config)?;
        let base = github::resolve_base(&token, &config.repo, command.base.as_deref())?;
        println!("seeding at {}", &base[..12.min(base.len())]);
        Ok((token, base))
    })?;
    if report.created.is_empty() {
        println!(
            "pool {pool} already has {} slot(s); nothing to create",
            report.existing.len()
        );
        return Ok(());
    }
    for outcome in &report.created {
        println!("{}", outcome_line(outcome));
    }
    let failed = report.created.iter().filter(|o| !o.seeded()).count();
    if failed > 0 {
        return Err(VivariumError::Message(format!(
            "{failed} of {} new slot(s) did not seed cleanly",
            report.created.len()
        )));
    }
    Ok(())
}

fn outcome_line(outcome: &SlotOutcome) -> String {
    let note = if outcome.timed_out {
        "  TIMED OUT".to_string()
    } else {
        match outcome.exit_code {
            Some(code) if code != 0 => format!("  exit {code}"),
            _ => String::new(),
        }
    };
    format!(
        "{}  {}  {}  seed {}s{note}",
        outcome.name, outcome.id, outcome.state, outcome.seconds
    )
}

fn status(command: &PoolStatusCommand) -> Result<(), VivariumError> {
    let (_, pool, provider) = open_pool(command.project.as_deref())?;
    let slots = run::pool_status(&provider, &pool)?;
    if command.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&slots)
                .map_err(|e| VivariumError::Other(format!("failed to encode JSON: {e}")))?
        );
        return Ok(());
    }
    print_slots(&pool, &slots);
    Ok(())
}

fn print_slots(pool: &str, slots: &[SlotStatus]) {
    println!("pool {pool}  {} slot(s)", slots.len());
    for slot in slots {
        let task = slot
            .task
            .as_deref()
            .map_or(String::new(), |t| format!("  task {t}"));
        println!(
            "  {}  {}  {}{task}  base {}  created {}",
            slot.name,
            slot.id,
            slot.state,
            slot.base
                .as_deref()
                .map_or("unknown", |b| &b[..12.min(b.len())]),
            slot.created_at
        );
    }
}

fn destroy(command: &PoolDestroyCommand) -> Result<(), VivariumError> {
    if !command.yes {
        return Err(VivariumError::Message(
            "destroy removes machines and their seeded disks; pass --yes to confirm".into(),
        ));
    }
    let (_, pool, provider) = open_pool(command.project.as_deref())?;
    let target = match &command.slot {
        Some(selector) => DestroyTarget::Slot(selector.clone()),
        None => DestroyTarget::All,
    };
    let destroyed = run::destroy_slots(&provider, &pool, &target)?;
    for slot in &destroyed {
        println!("destroyed {}  {}", slot.name, slot.id);
    }
    println!("{} slot(s) destroyed", destroyed.len());
    Ok(())
}
