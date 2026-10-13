//! `vivi pool dispatch | wait | reconcile`: the command glue. The engines are
//! in [`vivi::pool::work`]; GitHub (token, default branch, base commit) is
//! reached from here only.

use std::time::Duration;

use vivi::VivariumError;
use vivi::cli::{PoolDispatchCommand, PoolReconcileCommand, PoolWaitCommand};
use vivi::mailspace::Mailspace;
use vivi::mailspace::PoolTask;
use vivi::pool::collect::{ErrorWatch, Pacing};
use vivi::pool::dispatch::{self, DispatchFlags};
use vivi::pool::github::Secret;
use vivi::pool::local_branch;
use vivi::pool::local_git::Repo;
use vivi::pool::pools::{self, PoolSet};
use vivi::pool::porcelain;
use vivi::pool::progress::{HEARTBEAT, Progress};
use vivi::pool::provider::SlotLifetime;
use vivi::pool::role_dispatch::{self, RoleFacts};
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
    refuse_second_dispatch(&space, &task.handle)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let job_id = dispatch::new_job_id(&task.handle, nanos, std::process::id());
    let (flags, rules, role_line) = resolve_dispatch(
        command,
        config,
        &id.pool,
        &space,
        &task,
        load_rules(config, &space)?,
    )?;
    let mut plan = dispatch::plan_job(
        config,
        &id.pool,
        (&task.handle, &task.subject, &task.body),
        &flags,
        rules,
        job_id,
    )?;
    read_only_notes(flags.read_only, config, &task.body, &progress);
    let (token, start) = match (config.worktree_source(), flags.read_only) {
        (Some(path), true) => worktree_read_only_start(&path, &mut plan, &progress)?,
        (Some(path), false) => {
            worktree_start(&path, &mut plan, flags.allow_default_branch, &progress)?
        }
        (None, _) => remote_start(config, &id.pool, &mut plan, &flags, &progress)?,
    };
    warn_missing_env(provider, config);
    claim_note(provider, &id.pool, &progress);
    let pacing = Pacing::default();
    let launch = dispatch::Launch {
        config,
        plan: &plan,
        token: &token,
        start: &start,
        progress: &progress,
        ready_timeout,
    };
    let made = work::submit(&space, provider, id, &launch, &pacing)?;
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
        println!("{}", dispatched_text(&task.handle, &made, &role_line));
    }
    Ok(())
}

/// What the dispatch says it is about to do to the pool: rent a pod (slots
/// made per dispatch) or claim a free resting slot.
fn claim_note(provider: &dyn vivi::pool::PoolProvider, pool: &str, progress: &Progress) {
    progress.say(&if provider.slot_lifetime() == SlotLifetime::PerDispatch {
        format!("{pool}: renting a pod and starting the job")
    } else {
        format!("{pool}: claiming a free slot and starting the job")
    });
}

/// Refuse a second dispatch of a task that already has one open.
fn refuse_second_dispatch(space: &Mailspace, handle: &str) -> Result<(), VivariumError> {
    if work::open_dispatches(space)?
        .iter()
        .any(|d| d.task == handle)
    {
        return Err(VivariumError::Message(format!(
            "task {handle} already has an open dispatch; run `vivi pool wait` or `reconcile` first"
        )));
    }
    Ok(())
}

/// The flags, rules text and human suffix of one dispatch, with the role the
/// task is addressed to folded in before anything is claimed: the role's
/// band, kind and charter and the operator's flags go through
/// `role_dispatch::resolve` against the pool's band table, so every refusal
/// there costs nothing. The rules gain the role's charter as a standing
/// prompt; a role with no charter leaves the pool rules unchanged.
fn resolve_dispatch(
    command: &PoolDispatchCommand,
    config: &PoolConfig,
    pool: &str,
    space: &Mailspace,
    task: &PoolTask,
    pool_rules: String,
) -> Result<(DispatchFlags, String, String), VivariumError> {
    if let Some(slug) = &command.model {
        if let Some(harness) = &config.harness {
            job::check_model(slug, &harness.models)?;
        }
    }
    // An addressee that is not a known local role is no role at all, not an
    // error: the dispatch then runs without one.
    let role = task
        .addressee
        .as_deref()
        .and_then(|name| match space.role_view(name) {
            Ok(view) => Some(RoleFacts {
                name: view.name,
                kind: view.kind,
                band: view.model,
                charter: view.charter,
            }),
            Err(_) => None,
        });
    let resolved = role_dispatch::resolve(
        config,
        pool,
        &role_dispatch::Input {
            model_flag: command.model.as_deref(),
            read_only_flag: command.read_only,
            role: role.as_ref(),
            body: &task.body,
        },
    )?;
    let rules = role.as_ref().map_or(pool_rules.clone(), |role| {
        role_dispatch::standing_prompt(&role.name, &role.charter, &pool_rules, false)
    });
    let role_line = role_line(command, role.as_ref(), &resolved);
    let flags = DispatchFlags {
        model: resolved.model,
        from: command.from.clone(),
        branch: command.branch.clone(),
        budget: command.budget,
        allow_default_branch: command.allow_default_branch,
        read_only: resolved.read_only,
        reasoning: resolved.reasoning,
    };
    Ok((flags, rules, role_line))
}

/// The `  role ...` suffix of the human dispatch line: the role, the band
/// that chose the model (`-` when none did), the model and its reasoning,
/// and a note when the role's kind, not the flag, forced read-only. Empty
/// without a role.
fn role_line(
    command: &PoolDispatchCommand,
    role: Option<&RoleFacts>,
    resolved: &role_dispatch::Resolved,
) -> String {
    let Some(role) = role else {
        return String::new();
    };
    let band = resolved
        .band
        .map_or_else(|| "-".to_string(), |b| format!("P{b}"));
    let mut line = format!("  role {} band {band} -> {}", role.name, resolved.model);
    if let Some(reasoning) = &resolved.reasoning {
        line.push(' ');
        line.push_str(reasoning);
    }
    if resolved.read_only && !command.read_only {
        line.push_str(&format!(
            " read-only (role kind {})",
            role.kind.as_deref().unwrap_or("none")
        ));
    }
    line
}

/// The human line of a dispatch, with the role suffix when a role was used.
fn dispatched_text(task: &str, made: &vivi::pool::record::DispatchRecord, role: &str) -> String {
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
    ) + role
        + &abandoned_suffix(made)
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
/// on standard error). A read-only mode a role kind forced gets the same
/// notes as one the flag forced.
fn read_only_notes(read_only: bool, config: &PoolConfig, body: &str, progress: &Progress) {
    if !read_only {
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
/// checked against the default branch, and `from` resolved to a commit, which
/// replaces an abbreviated `from` in the plan (a Git server cannot fetch one).
/// A read-only job gets a token that cannot push and has no push target.
fn remote_start(
    config: &PoolConfig,
    pool: &str,
    plan: &mut job::JobPlan,
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
    plan.refs.from = dispatch::full_from(&plan.refs.from, &start);
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
