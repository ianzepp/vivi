use std::time::Duration;

use crate::local_pool_scale_command as scale;
use crate::local_pool_work_command as work;
use vivi::VivariumError;
use vivi::cli::{PoolCreateCommand, PoolDestroyCommand, PoolStatusCommand, PoolSubcommand};
use vivi::mailspace::Mailspace;
use vivi::pool::bridge;
use vivi::pool::github::{self, Secret};
use vivi::pool::local_branch;
use vivi::pool::local_git::Repo;
use vivi::pool::pools::{self, Need, PoolHandle, PoolSet};
use vivi::pool::porcelain;
use vivi::pool::progress::Progress;
use vivi::pool::provider::SlotLifetime;
use vivi::pool::requires;
use vivi::pool::run::{self, CreateOptions, CreateReport, DestroyTarget, SlotOutcome};
use vivi::pool::scale::no_resting_slots;
use vivi::pool::slot::SlotStatus;
use vivi::pool::work as engine;

pub(crate) fn handle_pool_command(command: &PoolSubcommand) -> Result<(), VivariumError> {
    match command {
        PoolSubcommand::Create(c) => create(c),
        PoolSubcommand::Status(c) => status(c),
        PoolSubcommand::Destroy(c) => destroy(c),
        PoolSubcommand::Scale(c) => scale::scale(c),
        PoolSubcommand::Dispatch(c) => work::dispatch(c),
        PoolSubcommand::Wait(c) => work::wait(c),
        PoolSubcommand::Reconcile(c) => work::reconcile(c),
        PoolSubcommand::Doctor(c) => crate::local_pool_doctor_command::doctor(c),
    }
}

fn create(command: &PoolCreateCommand) -> Result<(), VivariumError> {
    let progress = Progress::stderr(command.porcelain, "");
    let space = Mailspace::discover(command.project.as_deref())?;
    let handle = pools::open_one(&space, command.pool.as_deref())?;
    let options = CreateOptions {
        slots: command.slots,
        timeout: Duration::from_secs(command.timeout),
        ..CreateOptions::default()
    };
    let seed = SeedAt {
        base: command.base.as_deref(),
        porcelain: command.porcelain,
    };
    create_into(&handle, &options, &seed, &progress).map(drop)
}

/// How new slots are seeded and how their results are printed.
pub(crate) struct SeedAt<'a> {
    pub base: Option<&'a str>,
    pub porcelain: bool,
}

/// Make the pool's home, create and seed the missing slots up to
/// `options.slots`, and print one line per new slot. Shared by `create` and
/// `scale`; both need the pool's repository.
pub(crate) fn create_into(
    handle: &PoolHandle,
    options: &CreateOptions,
    seed: &SeedAt,
    progress: &Progress,
) -> Result<CreateReport, VivariumError> {
    let (id, config) = (&handle.id, &handle.config);
    let provider = handle.provider.as_ref();
    if provider.slot_lifetime() == SlotLifetime::PerDispatch {
        return Err(no_resting_slots(&id.pool, &config.provider));
    }
    let worktree = config.worktree_source();
    let access = match &worktree {
        Some(_) => None,
        None => Some(config.require_repo(&id.pool, "create slots")?),
    };
    progress.say(&format!("{}: preparing the pool's home", id.pool));
    let ready = run::prepare_home(provider, config)?;
    if let Some(made) = &ready.created {
        println!(
            "{}",
            if seed.porcelain {
                porcelain::home(&id.pool, made)
            } else {
                format!("{}  {made}", id.pool)
            }
        );
    }
    if let Some(warning) = run::env_warning(provider, &ready.missing_env) {
        eprintln!("{warning}");
    }
    let report = run::create_slots(provider, config, id, options, progress, || {
        let (token, base) = match (&worktree, &access) {
            (Some(path), _) => {
                let repo = Repo::new(path);
                progress.say(&format!(
                    "{}: resolving the base commit in {}",
                    id.pool,
                    path.display()
                ));
                let base = local_branch::base_commit(&repo, seed.base)?;
                (Secret::new(String::new()), base)
            }
            (None, Some(access)) => {
                progress.say(&format!(
                    "{}: getting a GitHub token and the base commit",
                    id.pool
                ));
                let token = github::mint_token(access)?;
                let base = github::resolve_base(&token, access.repo, seed.base)?;
                (token, base)
            }
            (None, None) => {
                return Err(VivariumError::Config(format!(
                    "[pools.{}] has no source to seed from",
                    id.pool
                )));
            }
        };
        progress.say(&format!("seeding at {}", &base[..12.min(base.len())]));
        Ok((token, base))
    })?;
    print_created(seed.porcelain, &id.pool, &report);
    let failed = report.created.iter().filter(|o| !o.seeded()).count();
    if failed > 0 {
        return Err(VivariumError::Message(format!(
            "{failed} of {} new slot(s) in pool {} did not seed cleanly",
            report.created.len(),
            id.pool
        )));
    }
    Ok(report)
}

/// The results of `create`: one line per new slot (and per problem met while
/// waiting, in porcelain), or the note that nothing was missing.
fn print_created(porcelain: bool, pool: &str, report: &CreateReport) {
    if report.created.is_empty() {
        if porcelain {
            println!("{}", porcelain::exists(pool, report.existing.len()));
        } else {
            println!(
                "pool {pool} already has {} slot(s); nothing to create",
                report.existing.len()
            );
        }
        return;
    }
    for outcome in &report.created {
        if porcelain {
            println!("{}", porcelain::created(outcome));
        } else {
            println!("{}", outcome_line(outcome));
        }
    }
    if porcelain {
        report
            .issues
            .iter()
            .for_each(|i| println!("{}", porcelain::issue(i)));
    }
}

fn outcome_line(outcome: &SlotOutcome) -> String {
    let note = if outcome.timed_out {
        "  TIMED OUT".to_string()
    } else {
        match outcome.exit_code {
            Some(requires::EXIT_CODE) => format!(
                "  exit {} (a `requires` command failed){}",
                requires::EXIT_CODE,
                reason_text(outcome)
            ),
            Some(code) if code != 0 => format!("  exit {code}{}", reason_text(outcome)),
            _ => String::new(),
        }
    };
    format!(
        "{}  {}  {}  {}  seed {}s{note}",
        outcome.pool, outcome.name, outcome.id, outcome.state, outcome.seconds
    )
}

fn reason_text(outcome: &SlotOutcome) -> String {
    outcome
        .error
        .as_ref()
        .map_or_else(String::new, |reason| format!(": {reason}"))
}

fn status(command: &PoolStatusCommand) -> Result<(), VivariumError> {
    let space = Mailspace::discover(command.project.as_deref())?;
    let handles = pools::open(&space, command.pool.as_deref(), Need::Any)?;
    let set = PoolSet::of(&handles, command.pool.is_none());
    let open = engine::open_dispatches(&space)?;
    let report = run::status_of(&set, &open, engine::epoch_secs());
    if command.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report.slots)
                .map_err(|e| VivariumError::Other(format!("failed to encode JSON: {e}")))?
        );
    } else if command.porcelain {
        report
            .slots
            .iter()
            .for_each(|s| println!("{}", porcelain::slot(s)));
    } else {
        for handle in &handles {
            if !report.pools.iter().any(|(pool, _)| *pool == handle.id.pool) {
                continue;
            }
            let slots: Vec<&SlotStatus> = report
                .slots
                .iter()
                .filter(|s| s.pool == handle.id.pool)
                .collect();
            print_slots(handle, &slots);
        }
    }
    match report.failures.is_empty() {
        true => Ok(()),
        false => {
            let said: Vec<String> = report
                .failures
                .iter()
                .map(|(pool, e)| format!("pool {pool}: {e}"))
                .collect();
            Err(VivariumError::Message(format!(
                "cannot list: {}",
                said.join("; ")
            )))
        }
    }
}

/// `running 95s of 1800s` for a slot with an open dispatch.
fn running_text(slot: &SlotStatus) -> String {
    let mode = if slot.read_only == Some(true) {
        "  read-only"
    } else {
        ""
    };
    match (slot.running_secs, slot.budget_secs) {
        (Some(ran), Some(budget)) => format!("  running {ran}s of {budget}s{mode}"),
        _ if slot.state == "busy" => "  no open dispatch".to_string(),
        _ => String::new(),
    }
}

fn print_slots(handle: &PoolHandle, slots: &[&SlotStatus]) {
    if handle.provider.slot_lifetime() == SlotLifetime::PerDispatch {
        println!(
            "pool {}  {}  {} pod(s) of at most {} (a pod exists only while a job runs)",
            handle.id.pool,
            handle.config.provider,
            slots.len(),
            handle.config.slots
        );
    } else {
        println!(
            "pool {}  {}  {} slot(s)",
            handle.id.pool,
            handle.config.provider,
            slots.len()
        );
    }
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
            "  {}  {}  {}  {}{task}{}  base {}{arch}  created {}",
            slot.pool,
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
    let space = Mailspace::discover(command.project.as_deref())?;
    let handle = pools::open_one(&space, command.pool.as_deref())?;
    if !command.yes {
        let what = if handle.provider.slot_lifetime() == SlotLifetime::PerDispatch {
            "terminates the pool's pods, including any job running on one (its result is lost)"
        } else {
            "removes machines and their seeded disks"
        };
        return Err(VivariumError::Message(format!(
            "destroy {what}; pass --yes to confirm"
        )));
    }
    let target = match &command.slot {
        Some(selector) => DestroyTarget::Slot(selector.clone()),
        None => DestroyTarget::All,
    };
    let destroyed = run::destroy_slots(handle.provider.as_ref(), &handle.id, &target)?;
    for slot in &destroyed {
        println!("destroyed {}  {}  {}", slot.pool, slot.name, slot.id);
    }
    println!(
        "{} slot(s) destroyed in pool {}",
        destroyed.len(),
        handle.id.pool
    );
    if matches!(target, DestroyTarget::All) && handle.config.bridge.enabled {
        revoke_bridge_keys(&space, &handle.id.pool);
    }
    Ok(())
}

/// Sweep a destroyed bridge pool's leftover keys: every live key labelled for
/// the pool ([`bridge::revoke_pool`], which says which key went, one line
/// each). Best effort: a revoke that cannot be done only warns, naming the
/// exact manual command; it never fails a destroy that already succeeded.
fn revoke_bridge_keys(space: &Mailspace, pool: &str) {
    if let Err(e) = bridge::revoke_pool(space, pool) {
        eprintln!(
            "warning: the bridge keys of pool {pool} could not be revoked: {e}; when the \
             server answers, list the keys and revoke every one labelled \
             pool-{pool}-<job> by hand: vivi sync api-key revoke <key_id>"
        );
    }
}
