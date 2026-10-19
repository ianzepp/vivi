use std::time::Duration;

use crate::local_pool_scale_command as scale;
use crate::local_pool_work_command as work;
use vivi::VivariumError;
use vivi::cli::{
    PoolCleanCommand, PoolCreateCommand, PoolDestroyCommand, PoolResetCommand, PoolStatusCommand,
    PoolSubcommand,
};
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
        PoolSubcommand::Clean(c) => clean(c),
        PoolSubcommand::Reset(c) => reset(c),
        PoolSubcommand::List(c) => crate::local_pool_config_command::list(c),
        PoolSubcommand::Show(c) => crate::local_pool_config_command::show(c),
        PoolSubcommand::Add(c) => crate::local_pool_config_command::add(c),
        PoolSubcommand::Set(c) => crate::local_pool_config_command::set(c),
        PoolSubcommand::Unset(c) => crate::local_pool_config_command::unset(c),
        PoolSubcommand::Remove(c) => crate::local_pool_config_command::remove(c),
        PoolSubcommand::Bands { command } => crate::local_pool_config_command::bands(command),
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
        None if config.repo.is_some() => Some(config.require_repo(&id.pool, "create slots")?),
        None => None,
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
                if seed.base.is_some() {
                    return Err(VivariumError::Config(
                        "--base requires a repository source".into(),
                    ));
                }
                (Secret::new(String::new()), String::new())
            }
        };
        progress.say(&format!("seeding at {}", &base[..12.min(base.len())]));
        Ok((token, base))
    })?;
    let _ignored = vivi::pool::clean::record_pool_baselines(handle);
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

fn status_base_text(slot: &SlotStatus) -> String {
    match (slot.base_ref.as_deref(), slot.base.as_deref()) {
        (Some(name), Some(sha)) => format!("{name}@{}", &sha[..12.min(sha.len())]),
        (_, Some(sha)) => sha[..12.min(sha.len())].to_string(),
        (Some(name), None) => name.to_string(),
        _ => "unknown".into(),
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
            status_base_text(slot),
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

/// Reclaim free-slot build output and recycle work lanes whose units are
/// merged into the merge lane (not main).
fn clean(command: &PoolCleanCommand) -> Result<(), VivariumError> {
    let report = if let Some(request) = command.after_merged_request.as_deref() {
        vivi::pool::clean::after_merged_request(request, command.project.as_deref())?
    } else {
        let space = Mailspace::discover(command.project.as_deref())?;
        vivi::pool::clean::clean_mailspace(
            &space,
            &vivi::pool::clean::CleanOptions {
                dry_run: command.dry_run,
                pool: command.pool.clone(),
                base: command.base.clone(),
            },
        )?
    };
    print_clean(command.porcelain, command.dry_run, &report);
    match &report.refused {
        Some(msg) => Err(VivariumError::Message(msg.clone())),
        None => Ok(()),
    }
}

fn reset(command: &PoolResetCommand) -> Result<(), VivariumError> {
    if command.slot.is_none() && command.pool.is_none() {
        return Err(VivariumError::Message(
            "pool reset needs a slot or --pool".into(),
        ));
    }
    let space = Mailspace::discover(command.project.as_deref())?;
    let handle = pools::open_one(&space, command.pool.as_deref())?;
    let chosen = vivi::pool::reset::choose_base(
        &space,
        command.base.as_deref(),
        None,
        None,
        vivi::pool::reset::ResetKind::Recycle,
    );
    let machines = handle.provider.list()?;
    let slots = vivi::pool::slot::pool_slots(&handle.id, &machines);
    let targets: Vec<vivi::pool::slot::SlotStatus> = match &command.slot {
        Some(sel) => {
            let found = vivi::pool::slot::find_slot(&slots, sel).ok_or_else(|| {
                VivariumError::Message(format!("no slot {sel} in pool {}", handle.id.pool))
            })?;
            vec![found.clone()]
        }
        None => slots.into_iter().filter(|s| s.state == "free").collect(),
    };
    let mut failed = 0;
    for slot in &targets {
        if !print_reset(&handle, slot, &chosen, command.porcelain) {
            failed += 1;
        }
    }
    if failed > 0 {
        return Err(VivariumError::Message(format!(
            "{failed} slot reset(s) failed"
        )));
    }
    Ok(())
}

fn print_reset(
    handle: &PoolHandle,
    slot: &SlotStatus,
    chosen: &vivi::pool::reset::ChosenBase,
    porcelain: bool,
) -> bool {
    if slot.state != "free" {
        let reason = format!("slot is {}", slot.state);
        say_reset_skip(handle, slot, &chosen.name, &reason, porcelain);
        return false;
    }
    match vivi::pool::reset::reset_slot(
        &handle.config,
        handle.provider.as_ref(),
        &slot.id,
        &chosen.name,
        chosen.why,
    ) {
        Ok(out) => {
            say_reset_ok(handle, slot, &out, porcelain);
            true
        }
        Err(e) => {
            say_reset_skip(handle, slot, &chosen.name, &e.to_string(), porcelain);
            false
        }
    }
}

fn say_reset_ok(
    handle: &PoolHandle,
    slot: &SlotStatus,
    out: &vivi::pool::reset::ResetOutcome,
    porcelain: bool,
) {
    if porcelain {
        println!(
            "{}",
            porcelain::reset_line(&handle.id.pool, &slot.name, out)
        );
        return;
    }
    println!(
        "pool {}  {}  reset  origin/{}  {}  ({})",
        handle.id.pool,
        slot.name,
        out.base_ref,
        &out.sha[..12.min(out.sha.len())],
        out.why
    );
}

fn say_reset_skip(
    handle: &PoolHandle,
    slot: &SlotStatus,
    base: &str,
    reason: &str,
    porcelain: bool,
) {
    if porcelain {
        println!(
            "{}",
            porcelain::reset_skip(&handle.id.pool, &slot.name, base, reason)
        );
        return;
    }
    eprintln!("WARN pool {}  {}  {}", handle.id.pool, slot.name, reason);
}

fn print_clean(porcelain: bool, dry_run: bool, report: &vivi::pool::clean::CleanReport) {
    if porcelain {
        for action in &report.actions {
            println!("{}", porcelain::cleaned(dry_run, action));
        }
        for skip in &report.skips {
            println!("{}", porcelain::clean_skip(skip));
        }
        if let Some(free) = report.free_bytes {
            println!("{}", porcelain::clean_disk(free));
        }
        println!(
            "{}",
            porcelain::clean_summary(report.actions.len(), report.skips.len(), report.bytes())
        );
        return;
    }
    print_clean_human(dry_run, report);
}

fn print_clean_human(dry_run: bool, report: &vivi::pool::clean::CleanReport) {
    let verb = if dry_run {
        "would reclaim"
    } else {
        "reclaimed"
    };
    if report.actions.is_empty() && report.skips.is_empty() {
        println!("nothing to reclaim");
    }
    for action in &report.actions {
        println!(
            "pool {}  {}  {verb}  {}  {}  {}  ({})",
            action.pool,
            action.slot,
            action.kind.word(),
            vivi::pool::clean::rule::format_bytes(action.bytes),
            action.path,
            action.note
        );
    }
    for skip in &report.skips {
        println!(
            "pool {}  {}  kept  {}  {}",
            skip.pool,
            skip.slot,
            skip.kind.word(),
            skip.reason
        );
    }
    if let Some(free) = report.free_bytes {
        println!(
            "volume {} free (reclaim under {} GiB, refuse under {} GiB)",
            vivi::pool::clean::rule::format_bytes(free),
            vivi::pool::clean::rule::DISK_RECLAIM_GIB,
            vivi::pool::clean::rule::DISK_REFUSE_GIB
        );
    }
    println!(
        "{verb} {} in {} action(s); skipped {}",
        vivi::pool::clean::rule::format_bytes(report.bytes()),
        report.actions.len(),
        report.skips.len()
    );
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
