//! `vivi pool dispatch | wait | reconcile`: the command glue. The engines are
//! in [`vivi::pool::work`]; GitHub (token, default branch, base commit) is
//! reached from here only.

use std::time::Duration;

use vivi::VivariumError;
use vivi::cli::{PoolDispatchCommand, PoolReconcileCommand, PoolWaitCommand};
use vivi::mailspace::Mailspace;
use vivi::pool::bridge;
use vivi::pool::collect::{ErrorWatch, Pacing};
use vivi::pool::dispatch::{self, DispatchFlags};
use vivi::pool::github::Secret;
use vivi::pool::local_branch;
use vivi::pool::local_git::Repo;
use vivi::pool::pools::{self, PoolSet};
use vivi::pool::porcelain;
use vivi::pool::progress::{HEARTBEAT, Progress};
use vivi::pool::provider::SlotLifetime;
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
    let ready_timeout =
        dispatch::check_ready_timeout(provider.slot_lifetime(), command.ready_timeout)?;
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
    let flags = dispatch_flags(command);
    let job_id = dispatch::new_job_id(&task.handle, nanos, std::process::id());
    let rules = load_rules(config, &space)?;
    let mut plan = dispatch::plan_job(
        config,
        &id.pool,
        (&task.handle, &task.subject, &task.body),
        &flags,
        rules,
        job_id.clone(),
    )?;
    read_only_notes(command, config, &task.body, &progress);
    let (token, start) = match (config.worktree_source(), flags.read_only) {
        (Some(path), true) => worktree_read_only_start(&path, &mut plan, &progress)?,
        (Some(path), false) => {
            worktree_start(&path, &mut plan, flags.allow_default_branch, &progress)?
        }
        (None, _) => remote_start(config, &id.pool, &plan, &flags, &progress)?,
    };
    warn_missing_env(provider, config);
    mint_bridge_grant(config, &space, &id.pool, &job_id, &mut plan, &progress)?;
    progress.say(&if provider.slot_lifetime() == SlotLifetime::PerDispatch {
        format!("{}: renting a pod and starting the job", id.pool)
    } else {
        format!("{}: claiming a free slot and starting the job", id.pool)
    });
    let pacing = Pacing::default();
    let launch = dispatch::Launch {
        config,
        plan: &plan,
        token: &token,
        start: &start,
        progress: &progress,
        ready_timeout,
    };
    let made = match work::submit(&space, provider, id, &launch, &pacing) {
        Ok(made) => made,
        Err(e) => {
            undo_bridge_grant(&space, plan.bridge.as_ref());
            return Err(e);
        }
    };
    if provider.slot_lifetime() == SlotLifetime::PerDispatch {
        let deadline = vivi::pool::runpod_open::dispatch_deadline(
            &config.runpod,
            ready_timeout,
            (made.at, made.budget),
        );
        progress.say(&vivi::pool::runpod_open::uncollected_note(&made, deadline));
    }
    if command.porcelain {
        println!("{}", porcelain::dispatched(&task.handle, &made));
    } else {
        println!(
            "{}",
            dispatched_text(&task.handle, &made, plan.bridge.as_ref())
        );
    }
    Ok(())
}

/// Mint the job's bridge grant when the pool's bridge is enabled. Every
/// refusal check has passed by now, so the very next step is the claim; a
/// failure here refuses the dispatch before any slot is touched, and a
/// failure of the dispatch itself revokes the minted key (see
/// [`undo_bridge_grant`]). The grant rides in the plan, is delivered only
/// inside the job archive, and only its key id is ever named.
fn mint_bridge_grant(
    config: &PoolConfig,
    space: &Mailspace,
    pool: &str,
    job: &str,
    plan: &mut job::JobPlan,
    progress: &Progress,
) -> Result<(), VivariumError> {
    if !config.bridge.enabled {
        return Ok(());
    }
    progress.say(&format!(
        "note: the bridge worker reads {} from the server; run `vivi sync --drain` before \
         dispatching so the task is there",
        plan.task
    ));
    let grant = bridge::mint_for_job(space, pool, job)?;
    progress.say(&format!("bridge key {} ({})", grant.key_id, grant.label));
    plan.bridge = Some(grant);
    Ok(())
}

/// Revoke a minted bridge grant after the dispatch failed — the claim, the
/// delivery, or the record: the key must not outlive the attempt. Only the
/// key id is named, never the key.
fn undo_bridge_grant(space: &Mailspace, grant: Option<&bridge::Grant>) {
    let Some(grant) = grant else {
        return;
    };
    match bridge::revoke_grant(space, grant) {
        Ok(()) => eprintln!("revoked bridge key {} ({})", grant.key_id, grant.label),
        Err(e) => eprintln!(
            "warning: the dispatch failed and bridge key {} ({}) could not be revoked: {e}",
            grant.key_id, grant.label
        ),
    }
}

fn dispatch_flags(command: &PoolDispatchCommand) -> DispatchFlags {
    DispatchFlags {
        model: command.model.clone(),
        from: command.from.clone(),
        branch: command.branch.clone(),
        budget: command.budget,
        allow_default_branch: command.allow_default_branch,
        read_only: command.read_only,
    }
}

/// The human line of a dispatch.
fn dispatched_text(
    task: &str,
    made: &vivi::pool::record::DispatchRecord,
    bridge: Option<&bridge::Grant>,
) -> String {
    format!(
        "dispatched {task}  {}  {}  job {}  model {}  from {} ({})  {}  budget {}s{}",
        made.pool,
        made.slot,
        made.job,
        made.model,
        made.from,
        &made.start[..12.min(made.start.len())],
        if made.read_only {
            "read-only".to_string()
        } else {
            format!("branch {}", made.branch)
        },
        made.budget,
        if made.hardware.is_empty() {
            String::new()
        } else {
            format!("  hardware {}", made.hardware)
        }
    ) + &bridge_suffix(bridge)
        + &abandoned_suffix(made)
}

/// `  bridge key <key_id> (<label>)` when the job carries a grant; the key
/// itself is never printed.
fn bridge_suffix(grant: Option<&bridge::Grant>) -> String {
    grant.map_or(String::new(), |g| {
        format!("  bridge key {} ({})", g.key_id, g.label)
    })
}

/// `  abandoned ...` when pods were given up on before the one that works.
fn abandoned_suffix(made: &vivi::pool::record::DispatchRecord) -> String {
    let text = vivi::pool::record::abandoned_text(made);
    if text.is_empty() {
        text
    } else {
        format!("  {text}")
    }
}

/// What a read-only dispatch tells the caller: that a `branch:` body line is
/// ignored (human output only), and what the provider cannot promise (always,
/// on standard error).
fn read_only_notes(
    command: &PoolDispatchCommand,
    config: &PoolConfig,
    body: &str,
    progress: &Progress,
) {
    if !command.read_only {
        return;
    }
    if let Some(branch) = job::body_refs(body).branch {
        progress.say(&format!(
            "note: the task body names branch {branch}; a read-only job has no branch, so it is ignored"
        ));
    }
    if let Some(warning) = dispatch::read_only_warning(config) {
        eprintln!("{warning}");
    }
}

/// Where a job on a remote pool starts: a GitHub token, the push target
/// checked against the default branch, and `from` resolved to a commit. A
/// read-only job gets a token that cannot push and has no push target.
fn remote_start(
    config: &PoolConfig,
    pool: &str,
    plan: &job::JobPlan,
    flags: &dispatch::DispatchFlags,
    progress: &Progress,
) -> Result<(Secret, String), VivariumError> {
    let access = config.require_repo(pool, "dispatch")?;
    progress.say(&format!("{pool}: getting a GitHub token"));
    let contents = if plan.read_only {
        github::Contents::Read
    } else {
        github::Contents::Write
    };
    let token = github::mint_token_with(&access, contents)?;
    if !plan.read_only {
        progress.say("checking the push target");
        let default = github::default_branch(&token, access.repo)?;
        dispatch::check_push_target(&plan.refs.branch, &default, flags.allow_default_branch)?;
    }
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

/// Where a read-only job on a worktree pool starts: detached at `from`, with
/// no branch decision at all.
fn worktree_read_only_start(
    path: &std::path::Path,
    plan: &mut job::JobPlan,
    progress: &Progress,
) -> Result<(Secret, String), VivariumError> {
    progress.say(&format!(
        "resolving {} in {}",
        plan.refs.from,
        path.display()
    ));
    let checkout = local_branch::prepare_read_only(&Repo::new(path), &plan.refs.from)?;
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
