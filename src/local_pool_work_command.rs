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
/// good, plus the live admission a `Ready` model job holds — guard and
/// snapshot — until its job is recorded. `None` for a `--command` job, which
/// runs outside model admission.
struct Planned {
    selected: Option<dispatch::target::Resolved>,
    ready_timeout: Option<u64>,
    flags: DispatchFlags,
    role_line: String,
    plan: job::JobPlan,
    job_id: String,
    admission: Option<Admission>,
}

/// The permanent half of one dispatch request: everything that can refuse it
/// before any availability question is asked, with the model selection left
/// open. The pair is chosen later, from a live snapshot under the pool's
/// dispatch guard.
struct Prepared {
    /// The request's flags, the model-and-reasoning pair still empty and the
    /// read-only mode already resolved from the role.
    flags: DispatchFlags,
    /// The standing rules text the job delivers.
    rules: String,
    /// The normalized model intent, when this is a model job; `None` for a
    /// `--command` job.
    intent: Option<role_dispatch::Intent>,
    /// The role the job serves, when it has one: the human line names it
    /// once the pair is resolved.
    role: Option<RoleFacts>,
}

/// Decide the job without claiming a slot or touching a remote. `Ready`
/// carries the plan and, for a model job, the live admission whose guard
/// stays held until the job is recorded; `Waiting` names a temporary block
/// and is a successful answer, not an error. Every permanent check runs
/// before either arm, so a wait never hides a configuration fault: the
/// intent is normalized and pair-checked and the request itself validated,
/// and only then is live admission acquired ([`live::acquire`], on the
/// pool's own identity) and the model-and-reasoning pair selected exactly
/// once from its snapshot ([`role_dispatch::resolve_available`]) — still
/// before any claim, bridge grant, worktree start or remote start.
fn plan_phase(
    command: &PoolDispatchCommand,
    space: &Mailspace,
    task: &PoolTask,
    handle: &pools::PoolHandle,
) -> Result<Decision<Planned>, VivariumError> {
    let selected = resolve_target_config(space, &handle.config, command.target.as_deref())?;
    let config = selected
        .as_ref()
        .map_or(&handle.config, |selected| &selected.config);
    let pool = handle.id.pool.as_str();
    let ready_timeout =
        dispatch::check_ready_timeout(handle.provider.slot_lifetime(), command.ready_timeout)?;
    let job_id = fresh_job_id(&task.handle);
    let rules_text = load_rules(config, space)?;
    let prepared = resolve_dispatch(command, config, pool, space, task, rules_text)?;
    // The request-level checks plan_job runs, ahead of any availability
    // decision: a later wait can only ever be about capacity.
    let letter = (
        task.handle.as_str(),
        task.subject.as_str(),
        task.body.as_str(),
    );
    dispatch::validate_request(config, pool, letter, &prepared.flags)?;
    let mut flags = prepared.flags;
    let (admission, role_line) = match prepared.intent {
        // A --command job runs outside model admission: no guard, no
        // snapshot, no wait.
        None => (None, String::new()),
        Some(intent) => {
            let (admission, resolved) =
                match admit_and_select(space, task, handle, config, pool, &intent)? {
                    Decision::Ready(found) => found,
                    Decision::Waiting(wait) => return Ok(Decision::Waiting(wait)),
                };
            let line = role_line(command, prepared.role.as_ref(), &resolved);
            flags = carry_resolved(flags, &resolved);
            (Some(admission), line)
        }
    };
    let mut plan =
        dispatch::plan_job(config, pool, letter, &flags, prepared.rules, job_id.clone())?;
    plan.target = selected.as_ref().map(|selected| selected.target.clone());
    if config.provider == "local" {
        plan.publish_remote = selected
            .as_ref()
            .and_then(|selected| selected.target.publish_remote.clone());
    }
    Ok(Decision::Ready(Planned {
        selected,
        ready_timeout,
        flags,
        role_line,
        plan,
        job_id,
        admission,
    }))
}

/// Acquire the pool's live admission and select the model-and-reasoning
/// pair from its snapshot, exactly once ([`role_dispatch::resolve_available`]
/// over the [`live::acquire`] evidence). Contention (`DispatchBusy`), a
/// failed census (`BandUnavailable`) and an exhausted band or pin all answer
/// a typed wait with the guard released on the way out; only a `Ready`
/// selection keeps the admission — guard and snapshot — alive, for the
/// caller to hold until the job is recorded. The same-task-open check, run
/// before planning without the guard, is repeated here: holding the guard
/// closes the very race that check exists for.
fn admit_and_select(
    space: &Mailspace,
    task: &PoolTask,
    handle: &pools::PoolHandle,
    config: &PoolConfig,
    pool: &str,
    intent: &role_dispatch::Intent,
) -> Result<Decision<(Admission, role_dispatch::Resolved)>, VivariumError> {
    let admission = match live::acquire(
        space,
        handle.provider.as_ref(),
        &handle.id,
        work::epoch_secs(),
    )? {
        Decision::Ready(admission) => admission,
        Decision::Waiting(wait) => return Ok(Decision::Waiting(wait)),
    };
    refuse_second_dispatch(space, &task.handle)?;
    let resolved =
        match role_dispatch::resolve_available(config, pool, intent, &admission.availability)? {
            Decision::Ready(resolved) => resolved,
            Decision::Waiting(wait) => return Ok(Decision::Waiting(wait)),
        };
    Ok(Decision::Ready((admission, resolved)))
}

/// Carry the selected model-and-reasoning pair — and the read-only mode it
/// repeats — into the request's flags.
fn carry_resolved(mut flags: DispatchFlags, resolved: &role_dispatch::Resolved) -> DispatchFlags {
    flags.model = resolved.model.clone();
    flags.reasoning = resolved.reasoning.clone();
    flags.read_only = resolved.read_only;
    flags
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
    let Planned {
        selected,
        ready_timeout,
        flags,
        role_line,
        mut plan,
        job_id,
        admission,
    } = match plan_phase(command, &space, &task, &handle).inspect_err(|e| {
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
    let config = selected
        .as_ref()
        .map_or(&handle.config, |selected| &selected.config);
    let (id, provider) = (&handle.id, handle.provider.as_ref());
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
    // The job is recorded (or the failure has already undone its grant), so
    // the pool's dispatch guard can go: a later dispatch of this pool is
    // judged by the record, not the lock. Every error path before this
    // releases the guard the same way, by dropping the admission.
    drop(admission);
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
    provider: &dyn PoolProvider,
    id: &PoolId,
    config: &PoolConfig,
    plan: &job::JobPlan,
    token: &Secret,
    start: &str,
    progress: &Progress,
    ready_timeout: Option<u64>,
) -> Result<DispatchRecord, VivariumError> {
    let pacing = Pacing::default();
    let launch = dispatch::Launch {
        config,
        plan,
        token,
        start,
        progress,
        ready_timeout,
        reset_base: dispatch_reset_base(config, space),
    };
    work::submit(space, provider, id, &launch, &pacing)
        .inspect_err(|_| undo_bridge_grant(space, plan.bridge.as_ref()))
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

/// The permanent half of one dispatch: the flags, rules and normalized model
/// intent that every availability question presupposes. A `--command` job
/// skips model admission entirely: no standing prompt, no model and no
/// intent ([`resolve_command_dispatch`]). A model job keeps its `--model`
/// pre-check ([`job::check_model`], whose refusal wording the CLI pins),
/// normalizes the intent once ([`role_dispatch::normalize`]) and checks
/// every candidate pair ([`validate_pairs`]). The selection itself no longer
/// happens here: [`role_dispatch::resolve_available`] runs later, under the
/// pool's dispatch guard, so a wait can only ever be about capacity — and
/// the guard outlives this function, riding in the [`Prepared`] intent.
fn resolve_dispatch(
    command: &PoolDispatchCommand,
    config: &PoolConfig,
    pool: &str,
    space: &Mailspace,
    task: &PoolTask,
    pool_rules: String,
) -> Result<Prepared, VivariumError> {
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
    Ok(Prepared {
        flags,
        rules,
        intent: Some(intent),
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
/// sentence. A wait is a successful answer, neither `dispatched` nor
/// `refused`.
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
    role_dispatch::validate_publish(
        role.as_ref(),
        command.publish_ref.as_deref(),
        command.publish_expect.as_deref(),
        &configured_mains(space),
    )?;
    Ok(role)
}

/// The main branch name of every configured pump target; empty with no
/// `[pump]` table.
fn configured_mains(space: &Mailspace) -> Vec<String> {
    space.config.pump.as_ref().map_or_else(Vec::new, |pump| {
        pump.target_configs()
            .into_values()
            .map(|target| target.main)
            .collect()
    })
}

/// The permanent half of a no-model dispatch (`--command`): no role
/// resolution beyond the publish checks, no standing prompt, no model and no
/// model admission — the job takes no dispatch guard and draws from no
/// snapshot, so pool contention and caps cannot make it wait.
/// `validate_publish` still runs so a merge-role task can publish.
fn resolve_command_dispatch(
    command: &PoolDispatchCommand,
    space: &Mailspace,
    task: &PoolTask,
    pool_rules: String,
) -> Result<Prepared, VivariumError> {
    let role = dispatch_role(command, space, task)?;
    let (need, kind) = dispatch_need_kind(command, space, task, role.as_ref())?;
    Ok(Prepared {
        flags: base_flags(command, need, kind),
        rules: pool_rules,
        intent: None,
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
            thinking: view.thinking,
            charter: view.charter,
            read_only: view.read_only,
        }),
        Err(_) => None,
    };
    if let Some(facts) = named.and_then(known) {
        return Some(facts);
    }
    let builtin_auditor = match vivi::letter::read::task_of(&task.handle, &task.body) {
        Ok(envelope) => envelope
            .front
            .job
            .is_some_and(|job| job.builtin_role.as_deref() == Some("auditor")),
        Err(_) => false,
    };
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
    format!(
        "dispatched {task}  {}  {}  job {}  model {}  from {} ({start})  {target}  budget {}s{hardware}{}{}{role}{}",
        made.pool,
        made.slot,
        made.job,
        made.model,
        made.from,
        made.budget,
        with_suffix(made),
        bridge_suffix(bridge),
        abandoned_suffix(made),
    )
}

/// `  with ref@commit12 ...` when the job names extra refs.
fn with_suffix(made: &DispatchRecord) -> String {
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
fn abandoned_suffix(made: &DispatchRecord) -> String {
    let text = record::abandoned_text(made);
    if text.is_empty() {
        text
    } else {
        format!("  {text}")
    }
}

/// What a read-only dispatch tells the caller: that a `branch` front-matter key is
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

/// Where a job on a worktree pool starts: the branch decision, made against
/// the local repository before any slot is claimed. There is no token.
fn worktree_start(
    path: &Path,
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
    path: &Path,
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
