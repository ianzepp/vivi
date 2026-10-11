use std::time::Duration;

use crate::local_pool_work_command as work;
use vivi::VivariumError;
use vivi::cli::{PoolCreateCommand, PoolDestroyCommand, PoolStatusCommand, PoolSubcommand};
use vivi::mailspace::Mailspace;
use vivi::pool::github;
use vivi::pool::run::{self, CreateOptions, DestroyTarget, SlotOutcome};
use vivi::pool::slot::SlotStatus;
use vivi::pool::work as engine;
use vivi::pool::{PoolConfig, PoolProvider, open_provider};

pub(crate) fn handle_pool_command(command: &PoolSubcommand) -> Result<(), VivariumError> {
    match command {
        PoolSubcommand::Create(c) => create(c),
        PoolSubcommand::Status(c) => status(c),
        PoolSubcommand::Destroy(c) => destroy(c),
        PoolSubcommand::Dispatch(c) => work::dispatch(c),
        PoolSubcommand::Wait(c) => work::wait(c),
        PoolSubcommand::Reconcile(c) => work::reconcile(c),
        PoolSubcommand::Doctor(c) => crate::local_pool_doctor_command::doctor(c),
    }
}

/// Load, validate, and name the pool: `(config, pool name, provider)`.
fn open_pool(
    project: Option<&std::path::Path>,
) -> Result<(PoolConfig, String, Box<dyn PoolProvider>), VivariumError> {
    let (_, config, pool, provider) = open_pool_space(project)?;
    Ok((config, pool, provider))
}

/// [`open_pool`] plus the mailspace the pool is configured in.
pub(crate) fn open_pool_space(
    project: Option<&std::path::Path>,
) -> Result<(Mailspace, PoolConfig, String, Box<dyn PoolProvider>), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let Some(config) = mailspace.config.pool.clone() else {
        return Err(VivariumError::Config(
            "no [pool] table in .vivi/mailspace.toml".into(),
        ));
    };
    config.validate(&mailspace.config.name)?;
    let pool = config.pool_name(&mailspace.config.name);
    let provider = open_provider(&config, &mailspace.config.name);
    Ok((mailspace, config, pool, provider))
}

fn create(command: &PoolCreateCommand) -> Result<(), VivariumError> {
    let (_, config, pool, provider) = open_pool_space(command.project.as_deref())?;
    let provider = provider.as_ref();
    let ready = run::prepare_home(provider, &config)?;
    if let Some(made) = &ready.created {
        println!("{made}");
    }
    if let Some(warning) = run::env_warning(provider, &ready.missing_env) {
        eprintln!("{warning}");
    }
    let options = CreateOptions {
        slots: command.slots,
        timeout: Duration::from_secs(command.timeout),
        ..CreateOptions::default()
    };
    let report = run::create_slots(provider, &config, &pool, &options, || {
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
    let (space, _, pool, provider) = open_pool_space(command.project.as_deref())?;
    let mut slots = run::pool_status(provider.as_ref(), &pool)?;
    run::annotate_running(
        &mut slots,
        &engine::open_dispatches(&space)?,
        engine::epoch_secs(),
    );
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

/// `running 95s of 1800s` for a slot with an open dispatch.
fn running_text(slot: &SlotStatus) -> String {
    match (slot.running_secs, slot.budget_secs) {
        (Some(ran), Some(budget)) => format!("  running {ran}s of {budget}s"),
        _ if slot.state == "busy" => "  no open dispatch".to_string(),
        _ => String::new(),
    }
}

fn print_slots(pool: &str, slots: &[SlotStatus]) {
    println!("pool {pool}  {} slot(s)", slots.len());
    for slot in slots {
        let task = slot
            .task
            .as_deref()
            .map_or(String::new(), |t| format!("  task {t}"));
        let arch = slot
            .arch
            .as_deref()
            .map_or(String::new(), |a| format!("  arch {a}"));
        println!(
            "  {}  {}  {}{task}{}  base {}{arch}  created {}",
            slot.name,
            slot.id,
            slot.state,
            running_text(slot),
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
    let destroyed = run::destroy_slots(provider.as_ref(), &pool, &target)?;
    for slot in &destroyed {
        println!("destroyed {}  {}", slot.name, slot.id);
    }
    println!("{} slot(s) destroyed", destroyed.len());
    Ok(())
}
