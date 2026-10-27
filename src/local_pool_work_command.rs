//! `vivi pool dispatch | wait | reconcile`: the command glue. The engines are
//! in [`vivi::pool::work`]; GitHub (token, default branch, base commit) is
//! reached from here only.

use std::path::Path;
use std::time::Duration;

use vivi::VivariumError;
use vivi::cli::{PoolDispatchCommand, PoolReconcileCommand, PoolWaitCommand};
use vivi::mailspace::Mailspace;
use vivi::mailspace::PoolTask;
use vivi::pool::bands::live::{self, Admission};
use vivi::pool::bands::runtime::{Decision, Wait};
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
use vivi::pool::provider::{PoolProvider, SlotLifetime};
use vivi::pool::reconcile_parallel;
use vivi::pool::record::{self, DispatchRecord};
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

/// What the plan phase decided: everything that can refuse a dispatch for
/// good, plus a model job's live admission, held until the submission is done.
struct Planned {
    /// The model job's dispatch guard with its snapshot; released after the
    /// submission. A `--command` job has none.
    admission: Option<Admission>,
    selected: Option<dispatch::target::Resolved>,
    ready_timeout: Option<u64>,
    /// The flags with the selected pair, the rules and the human role line.
    prep: DispatchPrep,
    plan: job::JobPlan,
}

/// Decide the job without claiming a slot or touching a remote. `Ready`
/// carries the plan; `Waiting` names a temporary block and is a successful
/// answer, not an error. Every permanent check runs before either arm, so a
/// wait never hides a configuration fault.
fn plan_phase(
    command: &PoolDispatchCommand,
    space: &Mailspace,
    task: &PoolTask,
    handle: &pools::PoolHandle,
) -> Result<Decision<Planned>, VivariumError> {
    let selected = resolve_target_config(space, &handle.config, command.target.as_deref())?;
    let config = selected.as_ref().map_or(&handle.config, |s| &s.config);
    let pool = handle.id.pool.as_str();
    let ready_timeout =
        dispatch::check_ready_timeout(handle.provider.slot_lifetime(), command.ready_timeout)?;
    let rules_text = load_rules(config, space)?;
    let prep = resolve_dispatch(command, config, pool, space, task, rules_text)?;
    // The request-level checks plan_job runs, ahead of any availability
    // decision: a later wait can only ever be about capacity.
    let letter = (
        task.handle.as_str(),
        task.subject.as_str(),
        task.body.as_str(),
    );
    dispatch::validate_request(config, pool, letter, &prep.flags)?;
    let (prep, admission) = match admit(command, config, pool, space, task, handle, prep)? {
        Decision::Ready(admitted) => admitted,
        Decision::Waiting(wait) => return Ok(Decision::Waiting(wait)),
    };
    let job_id = fresh_job_id(&task.handle);
    let mut plan = dispatch::plan_job(
        config,
        pool,
        letter,
        &prep.flags,
        prep.rules.clone(),
        job_id,
    )?;
    plan.target = selected.as_ref().map(|selected| selected.target.clone());
    if config.provider == "local" {
        plan.publish_remote = selected
            .as_ref()
            .and_then(|s| s.target.publish_remote.clone());
    }
    Ok(Decision::Ready(Planned {
        admission,
        selected,
        ready_timeout,
        prep,
        plan,
    }))
}

/// What one dispatch request needs before its live admission: the flags with
/// the model pair and role line still unset, the standing rules, the role
/// facts, and the normalized intent a model job's admission selects for; a
/// `--command` job carries no intent and stays outside model admission.
struct DispatchPrep {
    intent: Option<role_dispatch::Intent>,
    flags: DispatchFlags,
    rules: String,
    role_line: String,
    role: Option<RoleFacts>,
}

/// Admit one prepared request: acquire the pool's dispatch guard, read its
/// snapshot, and select the model pair from it exactly once, on the shared
/// pool identity. The guard rides out in the answer and is released after
/// the submission; contention and exhaustion answer a typed wait with nothing
/// claimed, bridged or started. The same-task-open refusal repeats under the
/// guard, closing the race between the preflight check and the acquisition.
fn admit(
    command: &PoolDispatchCommand,
    config: &PoolConfig,
    pool: &str,
    space: &Mailspace,
    task: &PoolTask,
    handle: &pools::PoolHandle,
    mut prep: DispatchPrep,
) -> Result<Decision<(DispatchPrep, Option<Admission>)>, VivariumError> {
    let Some(intent) = &prep.intent else {
        return Ok(Decision::Ready((prep, None)));
    };
    let admitted = match live::acquire(
        space,
        handle.provider.as_ref(),
        &handle.id,
        work::epoch_secs(),
    )? {
        Decision::Ready(admitted) => admitted,
        Decision::Waiting(wait) => return Ok(Decision::Waiting(wait)),
    };
    refuse_second_dispatch(space, &task.handle)?;
    let resolved =
        match role_dispatch::resolve_available(config, pool, intent, &admitted.availability)? {
            Decision::Ready(resolved) => resolved,
            Decision::Waiting(wait) => return Ok(Decision::Waiting(wait)),
        };
    prep.role_line = role_line(command, prep.role.as_ref(), &resolved);
    prep.flags.model = resolved.model;
    prep.flags.reasoning = resolved.reasoning;
    Ok(Decision::Ready((prep, Some(admitted))))
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
    refuse_second_dispatch(&space, &task.handle)?;
    let planned = match plan_phase(command, &space, &task, &handle).inspect_err(|e| {
        if command.porcelain {
            println!("{}", porcelain::refused(&task.handle, &e.to_string()));
        }
    })? {
        Decision::Ready(planned) => planned,
        // A wait answers successfully: neither dispatched nor refused.
        Decision::Waiting(wait) => {
            println!(
                "{}",
                waiting_line(&task.handle, &handle.id.pool, &wait, command.porcelain)
            );
            return Ok(());
        }
    };
    let config = planned
        .selected
        .as_ref()
        .map_or(&handle.config, |s| &s.config);
    let (id, provider) = (&handle.id, handle.provider.as_ref());
    let mut plan = planned.plan;
    let flags = &planned.prep.flags;
    plan.extras = resolve_extras(
        &space,
        &handle.config,
        &id.pool,
        command,
        flags,
        &plan,
        &progress,
    )?;
    read_only_notes(flags.read_only, config, &task.body, &progress);
    let (token, start) = match config.worktree_source() {
        Some(path) => worktree_start(&path, &mut plan, flags, &progress)?,
        None => remote_start(config, &id.pool, &mut plan, flags, &progress)?,
    };
    warn_missing_env(provider, config);
    if handle.config.provider == "local" {
        vivi::pool::clean::guard_before_dispatch(&space, &id.pool)?;
    }
    let bridged = command.command.is_none() && config.bridge.enabled;
    mint_bridge_grant(bridged, &space, &id.pool, &mut plan, &progress)?;
    claim_note(provider, &id.pool, &progress);
    let made = submit_job(
        &space,
        provider,
        id,
        dispatch::Launch {
            config,
            plan: &plan,
            token: &token,
            start: &start,
            progress: &progress,
            ready_timeout: planned.ready_timeout,
            reset_base: dispatch_reset_base(config, &space),
        },
    )?;
    // The submission — and its rollback on failure — is done: release the
    // pool's dispatch guard; every error path above dropped it naturally.
    drop(planned.admission);
    note_last_branch(provider, &made.machine, &plan.refs.branch);
    note_uncollected(provider, config, planned.ready_timeout, &made, &progress);
    let line = if command.porcelain {
        porcelain::dispatched(&task.handle, &made)
    } else {
        dispatched_text(
            &task.handle,
            &made,
            plan.bridge.as_ref(),
            &planned.prep.role_line,
        )
    };
    println!("{line}");
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
/// job may carry them: the checkouts are never published.
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

/// A fresh job id: the task handle, the wall clock and the process id.
fn fresh_job_id(handle: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    dispatch::new_job_id(handle, nanos, std::process::id())
}

/// Submit the job and revoke a minted bridge grant when the submission
/// fails: the key must not outlive a dispatch that never happened.
fn submit_job(
    space: &Mailspace,
    provider: &dyn PoolProvider,
    id: &PoolId,
    launch: dispatch::Launch<'_>,
) -> Result<DispatchRecord, VivariumError> {
    work::submit(space, provider, id, &launch, &Pacing::default())
        .inspect_err(|_| undo_bridge_grant(space, launch.plan.bridge.as_ref()))
}

fn dispatch_reset_base(config: &PoolConfig, space: &Mailspace) -> Option<String> {
    if config.provider != "local" {
        return None;
    }
    Some(vivi::pool::reset::choose_base(space, None).name)
}

/// Note, on a per-dispatch host, when an uncollected pod gives up its job.
fn note_uncollected(
    provider: &dyn PoolProvider,
    config: &PoolConfig,
    ready_timeout: Option<u64>,
    made: &DispatchRecord,
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
fn note_last_branch(provider: &dyn PoolProvider, machine: &str, branch: &str) {
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
fn claim_note(provider: &dyn PoolProvider, pool: &str, progress: &Progress) {
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

/// The permanent outcome of one dispatch request: the role the task is
/// addressed to goes through [`role_dispatch::normalize`] against the pool's
/// band table before anything is claimed, and every candidate pair is checked
/// ([`validate_pairs`]). The rules gain the role's charter as a standing
/// prompt whose pool note follows the pool's bridge: with the bridge on it
/// names the server's vivi tools, without it the receipt ending; a role with
/// no charter, or no role, gets the pool rules then the project rules. No pair
/// is drawn here: the live admission ([`admit`]) selects exactly once from its
/// own snapshot, and `Waiting` is never an error here.
fn resolve_dispatch(
    command: &PoolDispatchCommand,
    config: &PoolConfig,
    pool: &str,
    space: &Mailspace,
    task: &PoolTask,
    pool_rules: String,
) -> Result<DispatchPrep, VivariumError> {
    if command.command.is_some() {
        return resolve_command_dispatch(command, space, task, pool_rules);
    }
    if let Some(slug) = &command.model {
        if let Some(harness) = &config.harness {
            job::check_model(slug, &harness.models)?;
        }
    }
    let role = dispatch_role(command, space, task)?;
    let intent = role_dispatch::normalize(
        config,
        pool,
        &role_dispatch::Input {
            model_flag: command.model.as_deref(),
            read_only_flag: command.read_only,
            role: role.as_ref(),
            body: &task.body,
        },
    )?;
    validate_pairs(config, pool, &intent)?;
    let rules = match role.as_ref() {
        Some(role) => role_dispatch::standing_prompt(
            &role.name,
            &role.charter,
            &pool_rules,
            &role_dispatch::project_rules_snapshot(space)?,
            config.bridge.enabled,
        ),
        None => role_dispatch::with_project_rules(
            &pool_rules,
            &role_dispatch::project_rules_snapshot(space)?,
        ),
    };
    let (need, kind) = dispatch_need_kind(command, space, task, role.as_ref())?;
    let flags = DispatchFlags {
        model: String::new(),
        read_only: intent.read_only,
        reasoning: None,
        ..base_flags(command, need, kind)
    };
    Ok(DispatchPrep {
        intent: Some(intent),
        flags,
        rules,
        role_line: String::new(),
        role,
    })
}

/// The permanent model-and-reasoning checks of one normalized intent, run
/// without a draw: a pin validates directly; a band draw validates every
/// candidate on its list, so a configuration fault is refused for the band
/// whatever a later draw would pick.
fn validate_pairs(
    config: &PoolConfig,
    pool: &str,
    intent: &role_dispatch::Intent,
) -> Result<(), VivariumError> {
    match &intent.selection {
        role_dispatch::Selection::Pin { model, reasoning } => {
            dispatch::validate_pair(config, pool, model, reasoning.as_deref())
        }
        role_dispatch::Selection::Draw { band } => config
            .bands
            .entries
            .get(&format!("P{band}"))
            .map_or(Ok(()), |choices| {
                choices.iter().try_for_each(|choice| {
                    dispatch::validate_pair(config, pool, &choice.model, Some(&choice.reasoning))
                })
            }),
    }
}

/// What a wait reports: the fixed porcelain line, or one concise human
/// sentence; a wait is neither `dispatched` nor `refused`.
fn waiting_line(task: &str, pool: &str, wait: &Wait, porcelain: bool) -> String {
    if porcelain {
        return porcelain::waiting(task, pool, wait);
    }
    let retry = wait
        .retry_at
        .map_or_else(String::new, |at| format!("; ask again after {at}"));
    format!("waiting {task} in {pool}: {}{retry}", wait.reason.word())
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

/// The role a dispatch runs as, with its publish flags checked against it. An
/// addressee that is not a known local role is no role at all, not an error:
/// the dispatch then runs without one.
fn dispatch_role(
    command: &PoolDispatchCommand,
    space: &Mailspace,
    task: &PoolTask,
) -> Result<Option<RoleFacts>, VivariumError> {
    let role = role_facts(space, task, command.role.as_deref());
    let mains = space.config.pump.as_ref().map_or_else(Vec::new, |pump| {
        pump.target_configs()
            .into_values()
            .map(|target| target.main)
            .collect()
    });
    role_dispatch::validate_publish(
        role.as_ref(),
        command.publish_ref.as_deref(),
        command.publish_expect.as_deref(),
        &mains,
    )?;
    Ok(role)
}

/// The prep of a no-model dispatch (`--command`): no role resolution, no
/// standing prompt and no model, and no live admission — the job runs outside
/// model selection. `validate_publish` still runs so a merge-role task can
/// publish.
fn resolve_command_dispatch(
    command: &PoolDispatchCommand,
    space: &Mailspace,
    task: &PoolTask,
    pool_rules: String,
) -> Result<DispatchPrep, VivariumError> {
    let role = dispatch_role(command, space, task)?;
    let (need, kind) = dispatch_need_kind(command, space, task, role.as_ref())?;
    let flags = base_flags(command, need, kind);
    Ok(DispatchPrep {
        intent: None,
        flags,
        rules: pool_rules,
        role_line: String::new(),
        role,
    })
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
/// then the shipped Auditor charter of an internal pump job, then the task
/// addressee. A name that is not a known local role is no role at all.
fn role_facts(space: &Mailspace, task: &PoolTask, named: Option<&str>) -> Option<RoleFacts> {
    let named = named.map(str::trim).filter(|name| !name.is_empty());
    let known = |name: &str| {
        space.role_view(name).ok().map(|view| RoleFacts {
            name: view.name,
            kind: view.kind,
            band: view.model,
            thinking: view.thinking,
            charter: view.charter,
            read_only: view.read_only,
        })
    };
    if let Some(facts) = named.and_then(known) {
        return Some(facts);
    }
    let builtin_auditor = vivi::letter::read::task_of(&task.handle, &task.body)
        .ok()
        .and_then(|envelope| envelope.front.job)
        .is_some_and(|job| job.builtin_role.as_deref() == Some("auditor"));
    if builtin_auditor {
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
/// failure of the dispatch itself revokes the minted key (see
/// [`undo_bridge_grant`]). The grant rides in the plan, is delivered only
/// inside the job archive, and only its key id is ever named.
fn mint_bridge_grant(
    enabled: bool,
    space: &Mailspace,
    pool: &str,
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
    let grant = bridge::mint_for_job(space, pool, &plan.job)?;
    progress.say(&format!("bridge key {} ({})", grant.key_id, grant.label));
    plan.bridge = Some(grant);
    Ok(())
}

/// Revoke a minted bridge grant after the dispatch failed — the claim, the
/// delivery, or the record: the key must not outlive the attempt.
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
    made: &DispatchRecord,
    bridge: Option<&bridge::Grant>,
    role: &str,
) -> String {
    let start = &made.start[..12.min(made.start.len())];
    let target = if made.read_only {
        "read-only".to_string()
    } else {
        format!("branch {}", made.branch)
    };
    let hardware = if made.hardware.is_empty() {
        String::new()
    } else {
        format!("  hardware {}", made.hardware)
    };
    let with = if made.with.is_empty() {
        String::new()
    } else {
        format!("  with {}", made.with.join(" "))
    };
    let bridge = bridge
        .map(|g| format!("  bridge key {} ({})", g.key_id, g.label))
        .unwrap_or_default();
    let abandoned = record::abandoned_text(made);
    let abandoned = if abandoned.is_empty() {
        abandoned
    } else {
        format!("  {abandoned}")
    };
    format!(
        "dispatched {task}  {}  {}  job {}  model {}  from {} ({start})  {target}  budget {}s{hardware}{}{}{role}{}",
        made.pool, made.slot, made.job, made.model, made.from, made.budget, with, bridge, abandoned,
    )
}

/// The addressee role's assigned pool, when the role exists and sets one.
fn role_pool(space: &Mailspace, addressee: Option<&str>) -> Option<String> {
    let name = addressee?;
    match space.role_view(name) {
        Ok(view) => view.pool,
        Err(_) => None,
    }
}

/// What a read-only dispatch tells the caller: that a `branch` front-matter key is
/// ignored (human output only), and what the provider cannot promise (always,
/// on standard error).
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
    let start = resolve_remote_refs(plan, progress, |name| {
        plain_git::resolve_base(remote, Some(name))
    })?;
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
    let start = resolve_remote_refs(plan, progress, |name| {
        github::resolve_base(&token, access.repo, Some(name))
    })?;
    Ok((token, start))
}

/// Resolve `from` and each extra ref of the plan against the remote with
/// `resolve`, before any slot is claimed: an unknown ref is a refusal and never
/// a spent job. An abbreviated hash becomes the full commit (a Git server
/// cannot fetch an abbreviated one), which the job carries in `with.list`.
/// Returns the commit `from` resolved to.
fn resolve_remote_refs(
    plan: &mut job::JobPlan,
    progress: &Progress,
    resolve: impl Fn(&str) -> Result<String, VivariumError>,
) -> Result<String, VivariumError> {
    let start = resolve(&plan.refs.from)?;
    plan.refs.from = if plan.target.is_some() {
        start.clone()
    } else {
        dispatch::full_from(&plan.refs.from, &start)
    };
    for with in &mut plan.with {
        progress.say(&format!("resolving {}", with.name));
        let commit = resolve(&with.name)?;
        with.name = dispatch::full_from(&with.name, &commit);
        with.commit = commit;
    }
    Ok(start)
}

/// Resolve each extra ref of the plan in the source repository of a worktree
/// pool, before any slot is claimed. The refs live in the shared repository,
/// so nothing is fetched later, but the job records them in `with.list`.
fn resolve_with_local(
    path: &Path,
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

/// Where a job on a worktree pool starts. A read-only job detaches at `from`
/// with no branch decision; otherwise the branch decision is made against the
/// local repository before any slot is claimed. There is no token.
fn worktree_start(
    path: &Path,
    plan: &mut job::JobPlan,
    flags: &dispatch::DispatchFlags,
    progress: &Progress,
) -> Result<(Secret, String), VivariumError> {
    let checkout = if plan.read_only {
        progress.say(&format!(
            "resolving {} in {}",
            plan.refs.from,
            path.display()
        ));
        local_branch::prepare_read_only(&Repo::new(path), &plan.refs.from)?
    } else {
        progress.say(&format!(
            "checking branch {} in {}",
            plan.refs.branch,
            path.display()
        ));
        local_branch::prepare(&Repo::new(path), &plan.refs, flags.allow_default_branch)?
    };
    let start = checkout.start.clone();
    plan.checkout = Some(checkout);
    resolve_with_local(path, plan, progress)?;
    Ok((Secret::new(String::new()), start))
}

/// Warn (never fail) when the host lacks harness environment the config names.
fn warn_missing_env(provider: &dyn PoolProvider, config: &PoolConfig) {
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
