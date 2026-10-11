//! `vivi pool scale`: the command glue. The plan is [`vivi::pool::scale`].

use std::time::Duration;

use crate::local_pool_command::{SeedAt, create_into};
use vivi::VivariumError;
use vivi::cli::PoolScaleCommand;
use vivi::mailspace::Mailspace;
use vivi::pool::pools;
use vivi::pool::porcelain;
use vivi::pool::progress::Progress;
use vivi::pool::run::{self, CreateOptions};
use vivi::pool::scale;
use vivi::pool::slot::SlotStatus;

pub(crate) fn scale(command: &PoolScaleCommand) -> Result<(), VivariumError> {
    let progress = Progress::stderr(command.porcelain, "");
    let space = Mailspace::discover(command.project.as_deref())?;
    let handle = pools::open_one(&space, command.pool.as_deref())?;
    let (id, config) = (&handle.id, &handle.config);
    let provider = handle.provider.as_ref();
    scale::check_target(command.target, config.slots)?;
    progress.say(&format!("{}: looking at the pool's slots", id.pool));
    let slots = run::pool_status(provider, id)?;
    let plan = scale::plan(&slots, command.target, config.slots)?;
    if plan.create > 0 {
        config.require_repo(&id.pool, "create slots")?;
    }
    if !plan.destroy.is_empty() && !command.yes {
        return Err(VivariumError::Message(format!(
            "scaling pool {} to {} would destroy {} free slot(s) ({}) and their seeded disks; pass --yes to confirm",
            id.pool,
            command.target,
            plan.destroy.len(),
            names(&plan.destroy)
        )));
    }
    let mut now = plan.have;
    if plan.create > 0 {
        let options = CreateOptions {
            slots: Some(command.target),
            timeout: Duration::from_secs(command.timeout),
            ..CreateOptions::default()
        };
        let seed = SeedAt {
            base: command.base.as_deref(),
            porcelain: command.porcelain,
        };
        now += create_into(&handle, &options, &seed, &progress)?
            .created
            .len();
    }
    for slot in &plan.destroy {
        progress.say(&format!("{}: destroying {}", id.pool, slot.name));
        provider.destroy(&slot.id)?;
        now -= 1;
        println!("{}", destroyed_line(command.porcelain, slot));
    }
    for slot in &plan.skipped {
        println!("{}", skipped_line(command.porcelain, slot));
    }
    println!("{}", summary_line(&id.pool, command, now));
    Ok(())
}

fn names(slots: &[SlotStatus]) -> String {
    let list: Vec<&str> = slots.iter().map(|s| s.name.as_str()).collect();
    list.join(", ")
}

fn destroyed_line(porcelain: bool, slot: &SlotStatus) -> String {
    if porcelain {
        porcelain::destroyed(slot)
    } else {
        format!("destroyed {}  {}  {}", slot.pool, slot.name, slot.id)
    }
}

fn skipped_line(porcelain: bool, slot: &SlotStatus) -> String {
    if porcelain {
        return porcelain::skipped(slot);
    }
    let task = slot
        .task
        .as_deref()
        .map_or(String::new(), |t| format!(" (task {t})"));
    format!(
        "skipped {}  {}  {}{task}: only a free slot is destroyed",
        slot.pool, slot.name, slot.state
    )
}

fn summary_line(pool: &str, command: &PoolScaleCommand, now: usize) -> String {
    if command.porcelain {
        return porcelain::scaled(pool, command.target, now);
    }
    format!("pool {pool}: {now} slot(s), asked for {}", command.target)
}
