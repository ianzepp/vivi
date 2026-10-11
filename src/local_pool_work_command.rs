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

pub(crate) fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub(crate) fn open_dispatches(space: &Mailspace) -> Result<Vec<OpenDispatch>, VivariumError> {
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
        allow_default_branch: command.allow_default_branch,
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
    let default = github::default_branch(&token, &config.repo)?;
    dispatch::check_push_target(&plan.refs.branch, &default, flags.allow_default_branch)?;
    warn_missing_secrets(&provider, &config, &config.app_name(&space.config.name));
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
        let freed = collect::release(&provider, &made.machine, &task.handle, &Pacing::default());
        if let Err(undo) = freed {
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

/// Warn (never fail) when the app lacks secrets the harness is configured for.
fn warn_missing_secrets(provider: &dyn vivi::pool::PoolProvider, config: &PoolConfig, app: &str) {
    match run::missing_secrets(provider, config) {
        Ok(missing) => run::secrets_warning(app, &missing)
            .iter()
            .for_each(|w| eprintln!("{w}")),
        Err(e) => eprintln!("warning: could not check the app's secrets: {e}"),
    }
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
    let mut watch = collect::ErrorWatch::default();
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
        watch.observe(&lines).iter().for_each(|n| eprintln!("{n}"));
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
    let pacing = Pacing::default();
    let lines = collect::run_round(&provider, &open, now, &pacing, &mut recorder(&space))?;
    lines.iter().for_each(print_line);
    let slots = run::pool_status(&provider, &pool)?;
    let mut failures = lines.iter().filter(|l| l.state == "error").count();
    for slot in collect::stale_claims(&slots, &open, now) {
        let task = slot.task.as_deref().unwrap_or_default();
        match collect::release(&provider, &slot.id, task, &pacing) {
            Ok(()) => println!(
                "{task}  stale-claim  {}  freed (no open dispatch)",
                slot.name
            ),
            Err(e) => {
                eprintln!("warning: could not free {}: {e}", slot.name);
                failures += 1;
            }
        }
    }
    failures += stop_orphans(&provider, &slots, &open, &pacing);
    if lines.is_empty() {
        println!("no open dispatches");
    }
    if failures > 0 {
        let first = lines.iter().find(|l| l.state == "error");
        let tail = first.map_or(String::new(), |l| format!(" (first: {})", l.detail));
        return Err(VivariumError::Message(format!(
            "{failures} problem(s) while reconciling{tail}"
        )));
    }
    Ok(())
}

/// Stop running slots that no task holds. Returns how many could not be stopped.
fn stop_orphans(
    provider: &dyn vivi::pool::PoolProvider,
    slots: &[vivi::pool::slot::SlotStatus],
    open: &[OpenDispatch],
    pacing: &Pacing,
) -> usize {
    let mut failed = 0;
    for slot in collect::orphan_runners(slots, open) {
        match provider
            .machine(&slot.id)
            .and_then(|m| collect::stop_unclaimed(provider, &m, pacing))
        {
            Ok(()) => println!(
                "-  orphan-stopped  {}  stopped (running with no task)",
                slot.name
            ),
            Err(e) => {
                eprintln!("warning: could not stop {}: {e}", slot.name);
                failed += 1;
            }
        }
    }
    failed
}
