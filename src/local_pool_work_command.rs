//! `vivi pool dispatch | wait | reconcile`: the command glue. The engines are
//! in [`vivi::pool::work`]; GitHub (token, default branch, base commit) is
//! reached from here only.

use std::time::Duration;

use vivi::VivariumError;
use vivi::cli::{PoolDispatchCommand, PoolReconcileCommand, PoolWaitCommand};
use vivi::mailspace::Mailspace;
use vivi::pool::collect::{ErrorWatch, Pacing};
use vivi::pool::dispatch::{self, DispatchFlags};
use vivi::pool::github::Secret;
use vivi::pool::local_branch;
use vivi::pool::local_git::Repo;
use vivi::pool::pools::{self, PoolSet};
use vivi::pool::porcelain;
use vivi::pool::progress::{HEARTBEAT, Progress};
use vivi::pool::work::{self, WaitOptions, Waited};
use vivi::pool::{PoolConfig, github, job, run};

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
    let progress = Progress::stderr(command.porcelain, "");
    let space = Mailspace::discover(command.project.as_deref())?;
    let handle = pools::open_one(&space, command.pool.as_deref())?;
    let (config, id, provider) = (&handle.config, &handle.id, handle.provider.as_ref());
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
    let rules = load_rules(config, &space)?;
    let mut plan = dispatch::plan_job(
        config,
        &id.pool,
        (&task.handle, &task.subject, &task.body),
        &flags,
        rules,
        job_id,
    )?;
    let (token, start) = match config.worktree_source() {
        Some(path) => worktree_start(&path, &mut plan, flags.allow_default_branch, &progress)?,
        None => remote_start(config, &id.pool, &plan, &flags, &progress)?,
    };
    warn_missing_env(provider, config);
    progress.say(&format!(
        "{}: claiming a free slot and starting the job",
        id.pool
    ));
    let made = work::submit(
        &space,
        provider,
        id,
        &plan,
        &token,
        &start,
        &Pacing::default(),
    )?;
    if command.porcelain {
        println!("{}", porcelain::dispatched(&task.handle, &made));
        return Ok(());
    }
    println!(
        "dispatched {}  {}  {}  job {}  model {}  from {} ({})  branch {}  budget {}s",
        task.handle,
        made.pool,
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

/// Where a job on a remote pool starts: a GitHub token, the push target
/// checked against the default branch, and `from` resolved to a commit.
fn remote_start(
    config: &PoolConfig,
    pool: &str,
    plan: &job::JobPlan,
    flags: &dispatch::DispatchFlags,
    progress: &Progress,
) -> Result<(Secret, String), VivariumError> {
    let access = config.require_repo(pool, "dispatch")?;
    progress.say(&format!("{pool}: getting a GitHub token"));
    let token = github::mint_token(&access)?;
    progress.say("checking the push target");
    let default = github::default_branch(&token, access.repo)?;
    dispatch::check_push_target(&plan.refs.branch, &default, flags.allow_default_branch)?;
    progress.say(&format!("resolving {}", plan.refs.from));
    let start = github::resolve_base(&token, access.repo, Some(&plan.refs.from))?;
    Ok((token, start))
}

/// Where a job on a worktree pool starts: the branch decision, made against
/// the local repository before any slot is claimed. There is no token.
fn worktree_start(
    path: &std::path::Path,
    plan: &mut job::JobPlan,
    allow_default: bool,
    progress: &Progress,
) -> Result<(Secret, String), VivariumError> {
    progress.say(&format!(
        "checking branch {} in {}",
        plan.refs.branch,
        path.display()
    ));
    let checkout = local_branch::prepare(&Repo::new(path), &plan.refs, allow_default)?;
    let start = checkout.start.clone();
    plan.checkout = Some(checkout);
    Ok((Secret::new(String::new()), start))
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
    let progress = Progress::stderr(command.porcelain, "");
    let space = Mailspace::discover(command.project.as_deref())?;
    let handles = pools::open_for_collect(&space, command.pool.as_deref())?;
    let set = PoolSet::of(&handles, command.pool.is_none());
    let wanted = command
        .tasks
        .iter()
        .map(|t| space.pool_handle(t))
        .collect::<Result<Vec<_>, _>>()?;
    let options = WaitOptions {
        limit: Duration::from_secs(command.timeout),
        poll: WAIT_POLL,
        pacing: Pacing::default(),
        heartbeat: HEARTBEAT,
        cancelled: &|| false,
    };
    let mut watch = ErrorWatch::default();
    let waited = work::wait(&space, &set, &wanted, &options, &mut watch, &mut |n| {
        progress.say(n);
    });
    if command.porcelain {
        watch
            .issues()
            .iter()
            .for_each(|i| println!("{}", porcelain::issue(i)));
    }
    match waited? {
        Waited::NothingOpen if command.porcelain => println!("{}", porcelain::nothing_open()),
        Waited::NothingOpen => println!("nothing left to wait for (already collected)"),
        Waited::Settled(lines) => lines.iter().for_each(|l| {
            if command.porcelain {
                println!("{}", porcelain::collected(l));
            } else {
                println!("{}", work::line_text(l));
            }
        }),
    }
    Ok(())
}

pub(crate) fn reconcile(command: &PoolReconcileCommand) -> Result<(), VivariumError> {
    let progress = Progress::stderr(command.porcelain, "");
    let space = Mailspace::discover(command.project.as_deref())?;
    let handles = pools::open_for_collect(&space, command.pool.as_deref())?;
    let set = PoolSet::of(&handles, command.pool.is_none());
    progress.say("polling the open dispatches and freeing stale slots");
    let report = work::reconcile(&space, &set, &Pacing::default())?;
    if command.porcelain {
        report
            .items
            .iter()
            .for_each(|l| println!("{}", porcelain::reconciled(l)));
        if report.idle {
            println!("{}", porcelain::nothing_open());
        }
    } else {
        report.out.iter().for_each(|l| println!("{l}"));
    }
    report.warnings.iter().for_each(|w| eprintln!("{w}"));
    report.error().map_or(Ok(()), Err)
}
