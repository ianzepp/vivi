//! `vivi pool dispatch | wait | reconcile`: the command glue. The engines are
//! in [`vivi::pool::work`]; GitHub (token, default branch, base commit) is
//! reached from here only.

use std::time::Duration;

use vivi::VivariumError;
use vivi::cli::{PoolDispatchCommand, PoolReconcileCommand, PoolWaitCommand};
use vivi::mailspace::Mailspace;
use vivi::pool::collect::Pacing;
use vivi::pool::dispatch::{self, DispatchFlags};
use vivi::pool::work::{self, WaitOptions, Waited};
use vivi::pool::{PoolConfig, github, job, run};

use crate::local_pool_command::open_pool_space;

const WAIT_POLL: Duration = Duration::from_secs(8);

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
    if work::open_dispatches(&space)?
        .iter()
        .any(|d| d.task == task.handle)
    {
        return Err(VivariumError::Message(format!(
            "task {} already has an open dispatch; run `vivi pool wait` or `reconcile` first",
            task.handle
        )));
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
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
    warn_missing_env(provider.as_ref(), &config);
    let start = github::resolve_base(&token, &config.repo, Some(&plan.refs.from))?;
    let made = work::submit(
        &space,
        provider.as_ref(),
        &pool,
        &plan,
        &token,
        &start,
        &Pacing::default(),
    )?;
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

/// Warn (never fail) when the host lacks harness environment the config names.
fn warn_missing_env(provider: &dyn vivi::pool::PoolProvider, config: &PoolConfig) {
    match run::missing_env_at_dispatch(provider, config) {
        Ok(missing) => run::env_warning(provider, &missing)
            .iter()
            .for_each(|w| eprintln!("{w}")),
        Err(e) => eprintln!("warning: could not check the harness environment: {e}"),
    }
}

pub(crate) fn wait(command: &PoolWaitCommand) -> Result<(), VivariumError> {
    let (space, _, _, provider) = open_pool_space(command.project.as_deref())?;
    let wanted = command
        .tasks
        .iter()
        .map(|t| space.pool_handle(t))
        .collect::<Result<Vec<_>, _>>()?;
    let options = WaitOptions {
        limit: Duration::from_secs(command.timeout),
        poll: WAIT_POLL,
        pacing: Pacing::default(),
        cancelled: &|| false,
    };
    let waited = work::wait(&space, provider.as_ref(), &wanted, &options, &mut |n| {
        eprintln!("{n}");
    })?;
    match waited {
        Waited::NothingOpen => println!("nothing left to wait for (already collected)"),
        Waited::Settled(lines) => lines
            .iter()
            .for_each(|l| println!("{}", work::line_text(l))),
    }
    Ok(())
}

pub(crate) fn reconcile(command: &PoolReconcileCommand) -> Result<(), VivariumError> {
    let (space, _, pool, provider) = open_pool_space(command.project.as_deref())?;
    let report = work::reconcile(&space, provider.as_ref(), &pool, &Pacing::default())?;
    report.out.iter().for_each(|l| println!("{l}"));
    report.warnings.iter().for_each(|w| eprintln!("{w}"));
    report.error().map_or(Ok(()), Err)
}
