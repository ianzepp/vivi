//! `vivi pool dispatch | wait | reconcile`: the command glue. The engines are
//! in [`vivi::pool::work`]; GitHub (token, default branch, base commit) is
//! reached from here only.

use std::time::Duration;

use vivi::VivariumError;
use vivi::cli::{PoolDispatchCommand, PoolReconcileCommand, PoolWaitCommand};
use vivi::mailspace::Mailspace;
use vivi::mailspace::PoolTask;
use vivi::pool::bridge;
use vivi::pool::collect::{ErrorWatch, Pacing};
use vivi::pool::config::RepoAccess;
use vivi::pool::dispatch::{self, DispatchFlags};
use vivi::pool::github::Secret;
use vivi::pool::local_branch;
use vivi::pool::local_git::Repo;
use vivi::pool::plain_git::{self, RepoSource};
use vivi::pool::pools::{self, PoolSet};
use vivi::pool::porcelain;
use vivi::pool::progress::{HEARTBEAT, Progress};
use vivi::pool::provider::PoolProvider;
use vivi::pool::provider::SlotLifetime;
use vivi::pool::reconcile_parallel;
use vivi::pool::role_dispatch::{self, RoleFacts};
use vivi::pool::slot::PoolId;
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

/// The active `project`-scope rules, read now as the dispatch's snapshot.
fn load_project_rules(space: &Mailspace) -> Result<String, VivariumError> {
    role_dispatch::project_rules_snapshot(space)
}

pub(crate) fn dispatch(command: &PoolDispatchCommand) -> Result<(), VivariumError> {
    let progress = Progress::stderr(command.porcelain, "");
    let space = Mailspace::discover(command.project.as_deref())?;
    let task = space.pool_task(&command.task)?;
    let pool_flag = command
        .pool
        .clone()
        .or_else(|| role_pool(&space, task.addressee.as_deref()));
    let handle = pools::open_one(&space, pool_flag.as_deref())?;
    let selected = resolve_target_config(&space, &handle.config, command.target.as_deref())?;
    let config = selected
        .as_ref()
        .map_or(&handle.config, |selected| &selected.config);
    let (id, provider) = (&handle.id, handle.provider.as_ref());
    let ready_timeout =
        dispatch::check_ready_timeout(provider.slot_lifetime(), command.ready_timeout)?;
    refuse_second_dispatch(&space, &task.handle)?;
    let job_id = fresh_job_id(&task.handle);
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
        job_id.clone(),
    )?;
    plan.target = selected.as_ref().map(|selected| selected.target.clone());
    if config.provider == "local" {
        plan.publish_remote = selected
            .as_ref()
            .and_then(|selected| selected.target.publish_remote.clone());
    }
    plan.extras = resolve_extras(
        &space,
        &handle.config,
        &id.pool,
        command,
        &flags,
        &plan,
        &progress,
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
    if handle.config.provider == "local" {
        vivi::pool::clean::guard_before_dispatch(&space, &id.pool)?;
    }
    let bridged = command.command.is_none() && config.bridge.enabled;
    mint_bridge_grant(bridged, &space, &id.pool, &job_id, &mut plan, &progress)?;
    claim_note(provider, &id.pool, &progress);
    let made = submit_job(
        &space,
        provider,
        id,
        config,
        &plan,
        &token,
        &start,
        &progress,
        ready_timeout,
    )?;
    note_last_branch(provider, &made.machine, &plan.refs.branch);
    note_uncollected(provider, config, ready_timeout, &made, &progress);
    if command.porcelain {
        println!("{}", porcelain::dispatched(&task.handle, &made));
    } else {
        println!(
            "{}",
            dispatched_text(&task.handle, &made, plan.bridge.as_ref(), &role_line)
        );
    }
    Ok(())
}

/// Select a job source while the handle/provider retains the shared pool identity.
fn resolve_target_config(
    space: &Mailspace,
    config: &PoolConfig,
    target: Option<&str>,
) -> Result<Option<dispatch::target::Resolved>, VivariumError> {
    let Some(target) = target else {
        return Ok(None);
    };
    let pump = space.config.pump.as_ref().ok_or_else(|| {
        VivariumError::Config("--target requires [pump] repository targets".into())
    })?;
    dispatch::target::resolve(config, pump, &space.root, target).map(Some)
}

/// The other targets checked out read-only beside the job's own
/// (`--also-target`), resolved before any slot is claimed. Only a read-only
/// job may carry them: the checkouts are scratch space nothing publishes.
fn resolve_extras(
    space: &Mailspace,
    config: &PoolConfig,
    pool: &str,
    command: &PoolDispatchCommand,
    flags: &DispatchFlags,
    plan: &job::JobPlan,
    progress: &Progress,
) -> Result<Vec<job::ExtraCheckout>, VivariumError> {
    if command.also_target.is_empty() {
        return Ok(Vec::new());
    }
    if !flags.read_only {
        return Err(VivariumError::Message(
            "--also-target needs a read-only job: the extra checkouts are never published".into(),
        ));
    }
    let pump = space.config.pump.as_ref().ok_or_else(|| {
        VivariumError::Config("--also-target requires [pump] repository targets".into())
    })?;
    vivi::pool::extras::resolve(
        pool,
        config,
        pump,
        &space.root,
        plan.target.as_ref(),
        &command.also_target,
        progress,
    )
}

/// A fresh job id for this dispatch: the task handle, the wall clock and the
/// process id.
fn fresh_job_id(handle: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    dispatch::new_job_id(handle, nanos, std::process::id())
}

/// Submit the job and revoke a minted bridge grant when the submission
/// fails: the key must not outlive a dispatch that never happened.
#[allow(clippy::too_many_arguments)]
fn submit_job(
    space: &Mailspace,
    provider: &dyn vivi::pool::PoolProvider,
    id: &vivi::pool::slot::PoolId,
    config: &PoolConfig,
    plan: &job::JobPlan,
    token: &Secret,
    start: &str,
    progress: &Progress,
    ready_timeout: Option<u64>,
) -> Result<vivi::pool::record::DispatchRecord, VivariumError> {
    let pacing = Pacing::default();
    let launch = dispatch::Launch {
        config,
        plan,
        token,
        start,
        progress,
        ready_timeout,
        reset_base: dispatch_reset_base(config, space, &plan.task),
    };
    work::submit(space, provider, id, &launch, &pacing)
        .inspect_err(|_| undo_bridge_grant(space, plan.bridge.as_ref()))
}

fn dispatch_reset_base(config: &PoolConfig, space: &Mailspace, task: &str) -> Option<String> {
    if config.provider != "local" {
        return None;
    }
    let st = vivi::pool::clean_pump::load_state(space);
    let unit = st
        .as_ref()
        .and_then(|s| vivi::pool::clean_pump::unit_for(s, Some(task), None));
    let chosen =
        vivi::pool::reset::choose_base(space, None, unit, vivi::pool::reset::ResetKind::Dispatch);
    Some(chosen.name)
}

/// Note, on a per-dispatch host, when an uncollected pod gives up its job.
fn note_uncollected(
    provider: &dyn vivi::pool::PoolProvider,
    config: &PoolConfig,
    ready_timeout: Option<u64>,
    made: &vivi::pool::record::DispatchRecord,
    progress: &Progress,
) {
    if provider.slot_lifetime() != SlotLifetime::PerDispatch {
        return;
    }
    let deadline = vivi::pool::runpod_open::dispatch_deadline(
        &config.runpod,
        ready_timeout,
        (made.at, made.budget),
    );
    progress.say(&vivi::pool::runpod_open::uncollected_note(made, deadline));
}

/// Record the job branch on a local slot so later clean can recycle it.
fn note_last_branch(provider: &dyn vivi::pool::PoolProvider, machine: &str, branch: &str) {
    if branch.is_empty() {
        return;
    }
    let Some(root) = provider.local_root() else {
        return;
    };
    let dir = root.join(machine);
    let _ignored =
        vivi::pool::clean_rule::write_mark(&dir, vivi::pool::clean_rule::MARK_LAST_BRANCH, branch);
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
/// prompt whose pool note follows the pool's bridge: with the bridge on it
/// names the server's vivi tools, without it the receipt ending. A role with
/// no charter, or no role, gets the pool rules followed by the project rules.
fn resolve_dispatch(
    command: &PoolDispatchCommand,
    config: &PoolConfig,
    pool: &str,
    space: &Mailspace,
    task: &PoolTask,
    pool_rules: String,
) -> Result<(DispatchFlags, String, String), VivariumError> {
    if command.command.is_some() {
        return resolve_command_dispatch(command, space, task, pool_rules);
    }
    if let Some(slug) = &command.model {
        if let Some(harness) = &config.harness {
            job::check_model(slug, &harness.models)?;
        }
    }
    // An addressee that is not a known local role is no role at all, not an
    // error: the dispatch then runs without one.
    let role = role_facts(space, task, command.role.as_deref());
    role_dispatch::validate_publish(
        role.as_ref(),
        command.publish_ref.as_deref(),
        command.publish_expect.as_deref(),
    )?;
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
    let rules = match role.as_ref() {
        Some(role) => role_dispatch::standing_prompt(
            &role.name,
            &role.charter,
            &pool_rules,
            &load_project_rules(space)?,
            config.bridge.enabled,
        ),
        None => role_dispatch::with_project_rules(&pool_rules, &load_project_rules(space)?),
    };
    let role_line = role_line(command, role.as_ref(), &resolved);
    let (need, kind) = dispatch_need_kind(command, space, task, role.as_ref())?;
    let flags = DispatchFlags {
        model: resolved.model,
        read_only: resolved.read_only,
        reasoning: resolved.reasoning,
        ..base_flags(command, need, kind)
    };
    Ok((flags, rules, role_line))
}

/// The flags every dispatch shares, as the command line gave them; the model,
/// the reasoning level and the read-only mode are the caller's to fill in.
fn base_flags(
    command: &PoolDispatchCommand,
    need: Option<String>,
    kind: Option<String>,
) -> DispatchFlags {
    DispatchFlags {
        model: String::new(),
        need,
        kind,
        from: command.from.clone(),
        branch: command.branch.clone(),
        publish_ref: command.publish_ref.clone(),
        publish_expect: command.publish_expect.clone(),
        publish_contains: command.publish_contains.clone(),
        delete_ref: command.delete_ref.clone(),
        delete_expect: command.delete_expect.clone(),
        with: command.with.clone(),
        budget: command.budget,
        allow_default_branch: command.allow_default_branch,
        read_only: command.read_only,
        reasoning: None,
        command: command.command.clone(),
    }
}

/// The flags of a no-model dispatch (`--command`): no role resolution, no
/// standing prompt and no model. `validate_publish` still runs so a
/// merge-role task can publish.
fn resolve_command_dispatch(
    command: &PoolDispatchCommand,
    space: &Mailspace,
    task: &PoolTask,
    pool_rules: String,
) -> Result<(DispatchFlags, String, String), VivariumError> {
    let role = role_facts(space, task, command.role.as_deref());
    role_dispatch::validate_publish(
        role.as_ref(),
        command.publish_ref.as_deref(),
        command.publish_expect.as_deref(),
    )?;
    let (need, kind) = dispatch_need_kind(command, space, task, role.as_ref())?;
    let flags = base_flags(command, need, kind);
    Ok((flags, pool_rules, String::new()))
}

/// Resolve the need and kind carried by one dispatch command.
fn dispatch_need_kind(
    command: &PoolDispatchCommand,
    space: &Mailspace,
    task: &PoolTask,
    role: Option<&RoleFacts>,
) -> Result<(Option<String>, Option<String>), VivariumError> {
    let need = command
        .need
        .clone()
        .filter(|need| !need.trim().is_empty())
        .or(source_need(space, &task.handle)?);
    let kind = command
        .kind
        .clone()
        .filter(|kind| !kind.trim().is_empty())
        .or_else(|| {
            role.and_then(|role| role.kind.clone())
                .filter(|kind| !kind.trim().is_empty())
        });
    Ok((need, kind))
}

/// Resolve the role a dispatch runs as: the `--role` name first (so a step
/// task addressed to the pump identity still runs as the role it names),
/// then the shipped Auditor charter named by an internal pump job when the
/// mailspace has no auditor identity, then the task addressee. A name that is
/// not a known local role is no role at all, never the addressee instead.
fn role_facts(space: &Mailspace, task: &PoolTask, named: Option<&str>) -> Option<RoleFacts> {
    let named = named.map(str::trim).filter(|name| !name.is_empty());
    let known = |name: &str| match space.role_view(name) {
        Ok(view) => Some(RoleFacts {
            name: view.name,
            kind: view.kind,
            band: view.model,
            charter: view.charter,
            read_only: view.read_only,
        }),
        Err(_) => None,
    };
    if let Some(facts) = named.and_then(known) {
        return Some(facts);
    }
    if task
        .body
        .lines()
        .any(|line| line.trim() == "pump-builtin-role: auditor")
    {
        return Some(RoleFacts::builtin_auditor());
    }
    if named.is_some() {
        return None;
    }
    task.addressee.as_deref().and_then(known)
}

fn source_need(space: &Mailspace, task: &str) -> Result<Option<String>, VivariumError> {
    let storage = space.storage()?;
    let id = storage.resolve_message_token(task)?;
    let metadata = storage.item_metadata(&id)?;
    if metadata.get("source_kind").map(String::as_str) == Some("need") {
        return Ok(metadata
            .get("source_handle")
            .filter(|handle| !handle.trim().is_empty())
            .cloned());
    }
    Ok(None)
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
        let why = role.read_only.map_or_else(
            || format!("role kind {}", role.kind.as_deref().unwrap_or("none")),
            |_| "role read_only".to_string(),
        );
        line.push_str(&format!(" read-only ({why})"));
    }
    line
}

/// Mint the job's bridge grant when the pool's bridge is enabled. Every
/// refusal check has passed by now, so the very next step is the claim; a
/// failure here refuses the dispatch before any slot is touched, and a
/// failure of the dispatch itself revokes the minted key (see
/// [`undo_bridge_grant`]). The grant rides in the plan, is delivered only
/// inside the job archive, and only its key id is ever named.
fn mint_bridge_grant(
    enabled: bool,
    space: &Mailspace,
    pool: &str,
    job: &str,
    plan: &mut job::JobPlan,
    progress: &Progress,
) -> Result<(), VivariumError> {
    if !enabled {
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

/// The human line of a dispatch, with the bridge key and the role suffix when
/// they apply.
fn dispatched_text(
    task: &str,
    made: &vivi::pool::record::DispatchRecord,
    bridge: Option<&bridge::Grant>,
    role: &str,
) -> String {
    format!(
        "dispatched {task}  {}  {}  job {}  model {}  from {} ({})  {}  budget {}s{}{}",
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
        },
        with_suffix(made),
    ) + &bridge_suffix(bridge)
        + role
        + &abandoned_suffix(made)
}

/// `  with ref@commit12 ...` when the job names extra refs.
fn with_suffix(made: &vivi::pool::record::DispatchRecord) -> String {
    if made.with.is_empty() {
        String::new()
    } else {
        format!("  with {}", made.with.join(" "))
    }
}

/// The addressee role's assigned pool, when the role exists and sets one.
fn role_pool(space: &Mailspace, addressee: Option<&str>) -> Option<String> {
    let name = addressee?;
    match space.role_view(name) {
        Ok(view) => view.pool,
        Err(_) => None,
    }
}

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

/// Where a job on a remote pool starts, from its GitHub repository or its
/// plain Git remote.
fn remote_start(
    config: &PoolConfig,
    pool: &str,
    plan: &mut job::JobPlan,
    flags: &dispatch::DispatchFlags,
    progress: &Progress,
) -> Result<(Secret, String), VivariumError> {
    match config.require_source(pool, "dispatch")? {
        RepoSource::Github(access) => github_start(access, pool, plan, flags, progress),
        RepoSource::Git(remote) => git_start(remote, plan, flags, progress),
    }
}

/// A plain Git remote: no token (the slot fetches and pushes with its own SSH
/// key), the push target checked against the remote's `HEAD`, and `from` and
/// every extra ref resolved with `git ls-remote`.
fn git_start(
    remote: &str,
    plan: &mut job::JobPlan,
    flags: &dispatch::DispatchFlags,
    progress: &Progress,
) -> Result<(Secret, String), VivariumError> {
    if !plan.read_only {
        progress.say("checking the push target");
        let default = plain_git::default_branch(remote)?;
        dispatch::check_push_target(&plan.refs.branch, &default, flags.allow_default_branch)?;
    }
    progress.say(&format!("resolving {} at {remote}", plan.refs.from));
    let start = plain_git::resolve_base(remote, Some(&plan.refs.from))?;
    plan.refs.from = if plan.target.is_some() {
        start.clone()
    } else {
        dispatch::full_from(&plan.refs.from, &start)
    };
    for with in &mut plan.with {
        progress.say(&format!("resolving {}", with.name));
        let commit = plain_git::resolve_base(remote, Some(&with.name))?;
        with.name = dispatch::full_from(&with.name, &commit);
        with.commit = commit;
    }
    Ok((Secret::new(String::new()), start))
}

/// A GitHub repository: a GitHub token, the push target checked against the
/// default branch, and `from` resolved to a commit, which replaces an
/// abbreviated `from` in the plan (a Git server cannot fetch one). A
/// read-only job gets a token that cannot push and has no push target.
fn github_start(
    access: RepoAccess<'_>,
    pool: &str,
    plan: &mut job::JobPlan,
    flags: &dispatch::DispatchFlags,
    progress: &Progress,
) -> Result<(Secret, String), VivariumError> {
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
    plan.refs.from = if plan.target.is_some() {
        start.clone()
    } else {
        dispatch::full_from(&plan.refs.from, &start)
    };
    resolve_with_github(&token, access.repo, plan, progress)?;
    Ok((token, start))
}

/// Resolve each extra ref of the plan against the remote repository, with the
/// same token, before any slot is claimed: an unknown ref is a refusal and
/// never a spent job. An abbreviated hash becomes the full commit, which the
/// job then carries in `with.list`.
fn resolve_with_github(
    token: &Secret,
    repo: &str,
    plan: &mut job::JobPlan,
    progress: &Progress,
) -> Result<(), VivariumError> {
    for with in &mut plan.with {
        progress.say(&format!("resolving {}", with.name));
        let commit = github::resolve_base(token, repo, Some(&with.name))?;
        with.name = dispatch::full_from(&with.name, &commit);
        with.commit = commit;
    }
    Ok(())
}

/// Resolve each extra ref of the plan in the source repository of a worktree
/// pool, before any slot is claimed. The refs live in the shared repository,
/// so nothing is fetched later, but the job records them in `with.list`.
fn resolve_with_local(
    path: &std::path::Path,
    plan: &mut job::JobPlan,
    progress: &Progress,
) -> Result<(), VivariumError> {
    let repo = Repo::new(path);
    for with in &mut plan.with {
        progress.say(&format!("resolving {} in {}", with.name, path.display()));
        let Some(commit) = repo.commit_of(&with.name) else {
            return Err(VivariumError::Message(format!(
                "cannot resolve with ref \"{}\" to a commit in {}",
                with.name,
                path.display()
            )));
        };
        with.name = dispatch::full_from(&with.name, &commit);
        with.commit = commit;
    }
    Ok(())
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
    resolve_with_local(path, plan, progress)?;
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
    resolve_with_local(path, plan, progress)?;
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
    let report = if set.all && set.members.len() > 1 {
        // Several pools: each one is reconciled on its own thread, so one
        // slow machine host does not hold up the others.
        let ids: Vec<_> = set.members.iter().map(|m| m.id).collect();
        let open =
            |space: &Mailspace, id: &PoolId| -> Result<Box<dyn PoolProvider>, VivariumError> {
                let mut one = pools::open_for_collect(space, Some(&id.pool))?;
                one.pop()
                    .map(|handle| handle.provider)
                    .ok_or_else(|| VivariumError::Config(format!("pool {} did not open", id.pool)))
            };
        reconcile_parallel::reconcile_pools(&space, &ids, true, &Pacing::default(), &open)?
    } else {
        work::reconcile(&space, &set, &Pacing::default())?
    };
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
    if !command.porcelain {
        report.revoked.iter().for_each(|r| eprintln!("{r}"));
    }
    report.error().map_or(Ok(()), Err)
}

#[cfg(test)]
#[path = "local_pool_work_command_test.rs"]
mod tests;
