//! `vivi pool dispatch | wait | reconcile`: hand tasks to slots and collect
//! what they produce. The task is the unit: a dispatch and its result are
//! recorded on it, and the task itself is never closed here.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use vivi::VivariumError;
use vivi::cli::{PoolDispatchCommand, PoolReconcileCommand, PoolWaitCommand};
use vivi::mailspace::Mailspace;
use vivi::pool::collect::{self, Outcome, Pacing, RoundLine};
use vivi::pool::dispatch::{self, DispatchFlags};
use vivi::pool::record::{self, OpenDispatch};
use vivi::pool::{PoolConfig, github, job, run};

use crate::local_pool_command::open_pool_space;

const WAIT_POLL: Duration = Duration::from_secs(8);

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn open_dispatches(space: &Mailspace) -> Result<Vec<OpenDispatch>, VivariumError> {
    Ok(record::fold_open(&space.pool_events()?))
}

/// The standing rules to deliver: the configured file, else the embedded text.
fn load_rules(config: &PoolConfig, space: &Mailspace) -> Result<String, VivariumError> {
    let path = config
        .harness
        .as_ref()
        .and_then(|h| h.rules_path(&space.root));
    match path {
        Some(path) => std::fs::read_to_string(&path).map_err(|e| {
            VivariumError::Config(format!("cannot read rules file {}: {e}", path.display()))
        }),
        None => Ok(job::DEFAULT_RULES.to_string()),
    }
}

pub(crate) fn dispatch(command: &PoolDispatchCommand) -> Result<(), VivariumError> {
    let (space, config, pool, provider) = open_pool_space(command.project.as_deref())?;
    let task = space.pool_task(&command.task)?;
    if open_dispatches(&space)?
        .iter()
        .any(|d| d.task == task.handle)
    {
        return Err(VivariumError::Message(format!(
            "task {} already has an open dispatch; run `vivi pool wait` or `reconcile` first",
            task.handle
        )));
    }
    let now = epoch_secs();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let flags = DispatchFlags {
        model: command.model.clone(),
        from: command.from.clone(),
        branch: command.branch.clone(),
        budget: command.budget,
    };
    let job_id = dispatch::new_job_id(&task.handle, nanos, std::process::id());
    let rules = load_rules(&config, &space)?;
    let plan = dispatch::plan_job(
        &config,
        (&task.handle, &task.subject, &task.body),
        &flags,
        rules,
        job_id,
    )?;
    let token = github::mint_token(&config)?;
    let start = github::resolve_base(&token, &config.repo, Some(&plan.refs.from))?;
    let made = dispatch::dispatch(
        &provider,
        &pool,
        &plan,
        &token,
        &start,
        now,
        &Pacing::default(),
    )?;
    let note = record::dispatch_note(&made);
    let text = record::dispatch_text(&made);
    if let Err(e) = space.record_pool_note(&task.handle, "pool_dispatched", &note, &text) {
        if let Err(undo) = collect::release(&provider, &made.machine, &task.handle) {
            eprintln!(
                "warning: could not stop slot {} after the failed record: {undo}",
                made.slot
            );
        }
        return Err(e);
    }
    println!(
        "dispatched {}  {}  job {}  model {}  from {} ({})  branch {}  budget {}s",
        task.handle,
        made.slot,
        made.job,
        made.model,
        made.from,
        &made.start[..12.min(made.start.len())],
        made.branch,
        made.budget
    );
    Ok(())
}

/// Record a settled dispatch on its task, unless another process already did.
fn recorder<'a>(
    space: &'a Mailspace,
) -> impl FnMut(&OpenDispatch, &Outcome) -> Result<bool, VivariumError> + 'a {
    move |dispatch, outcome| {
        let still_open = open_dispatches(space)?
            .iter()
            .any(|o| o.task == dispatch.task && o.record.job == dispatch.record.job);
        if !still_open {
            return Ok(false);
        }
        space.record_pool_note(
            &dispatch.task,
            "pool_collected",
            &record::collected_note(dispatch, outcome),
            &record::collected_text(dispatch, outcome),
        )?;
        Ok(true)
    }
}

fn print_line(line: &RoundLine) {
    println!(
        "{}  {}  {}  {}",
        line.task, line.state, line.slot, line.detail
    );
}

pub(crate) fn wait(command: &PoolWaitCommand) -> Result<(), VivariumError> {
    let (space, _, _, provider) = open_pool_space(command.project.as_deref())?;
    let wanted = command
        .tasks
        .iter()
        .map(|t| space.pool_handle(t))
        .collect::<Result<Vec<_>, _>>()?;
    check_wanted(&space, &wanted)?;
    let started = Instant::now();
    let limit = Duration::from_secs(command.timeout);
    let mut last_error = None;
    loop {
        let open = wanted_open(&space, &wanted)?;
        if open.is_empty() {
            println!("nothing left to wait for (already collected)");
            return Ok(());
        }
        let lines = collect::run_round(
            &provider,
            &open,
            epoch_secs(),
            &Pacing::default(),
            &mut recorder(&space),
        )?;
        let settled: Vec<&RoundLine> = lines.iter().filter(|l| l.settled()).collect();
        if !settled.is_empty() {
            settled.into_iter().for_each(print_line);
            return Ok(());
        }
        last_error = lines
            .into_iter()
            .find(|l| l.state == "error")
            .or(last_error);
        if started.elapsed() >= limit {
            return Err(timeout_error(command.timeout, last_error.as_ref()));
        }
        std::thread::sleep(WAIT_POLL);
    }
}

fn timeout_error(seconds: u64, last_error: Option<&RoundLine>) -> VivariumError {
    let tail = last_error.map_or(String::new(), |l| {
        format!(" (last error on {}: {})", l.task, l.detail)
    });
    VivariumError::Message(format!("nothing finished within {seconds}s{tail}"))
}

fn wanted_open(space: &Mailspace, wanted: &[String]) -> Result<Vec<OpenDispatch>, VivariumError> {
    Ok(open_dispatches(space)?
        .into_iter()
        .filter(|d| wanted.is_empty() || wanted.contains(&d.task))
        .collect())
}

/// Named tasks must have an open dispatch now; with none named, something
/// must be open.
fn check_wanted(space: &Mailspace, wanted: &[String]) -> Result<(), VivariumError> {
    let open = open_dispatches(space)?;
    if wanted.is_empty() && open.is_empty() {
        return Err(VivariumError::Message(
            "no open dispatches to wait for".into(),
        ));
    }
    let missing: Vec<&String> = wanted
        .iter()
        .filter(|w| !open.iter().any(|d| &d.task == *w))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let names = missing
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Err(VivariumError::Message(format!(
        "no open dispatch for: {names}"
    )))
}

pub(crate) fn reconcile(command: &PoolReconcileCommand) -> Result<(), VivariumError> {
    let (space, _, pool, provider) = open_pool_space(command.project.as_deref())?;
    let open = open_dispatches(&space)?;
    let now = epoch_secs();
    let lines = collect::run_round(
        &provider,
        &open,
        now,
        &Pacing::default(),
        &mut recorder(&space),
    )?;
    lines.iter().for_each(print_line);
    let slots = run::pool_status(&provider, &pool)?;
    for slot in collect::stale_claims(&slots, &open, now) {
        let task = slot.task.as_deref().unwrap_or_default();
        collect::release(&provider, &slot.id, task)?;
        println!(
            "{task}  stale-claim  {}  freed (no open dispatch)",
            slot.name
        );
    }
    if lines.is_empty() {
        println!("no open dispatches");
    }
    let errors = lines.iter().filter(|l| l.state == "error").count();
    if errors > 0 {
        return Err(VivariumError::Message(format!(
            "{errors} dispatch(es) could not be polled"
        )));
    }
    Ok(())
}
