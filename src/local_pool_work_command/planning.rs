//! Dispatch planning: everything one dispatch decides before a slot is
//! claimed. The engines are in [`vivi::pool::dispatch`] and
//! [`vivi::pool::work`]; this module only refuses or shapes a job. Split out
//! of [`super`] to keep the command glue under the source hygiene limits.

use vivi::VivariumError;
use vivi::cli::PoolDispatchCommand;
use vivi::mailspace::{Mailspace, PoolTask};
use vivi::pool::PoolConfig;
use vivi::pool::dispatch::{self, DispatchFlags};
use vivi::pool::job;
use vivi::pool::pools;
use vivi::pool::role_dispatch;

use super::{dispatch_need_kind, dispatch_role, role_line};

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

/// What the plan phase decided: everything that can refuse a dispatch for good.
pub(super) struct Planned {
    pub(super) selected: Option<dispatch::target::Resolved>,
    pub(super) ready_timeout: Option<u64>,
    pub(super) flags: DispatchFlags,
    pub(super) role_line: String,
    pub(super) plan: job::JobPlan,
    pub(super) job_id: String,
}

/// Decide the job without claiming a slot or touching a remote.
pub(super) fn plan_phase(
    command: &PoolDispatchCommand,
    space: &Mailspace,
    task: &PoolTask,
    handle: &pools::PoolHandle,
) -> Result<Planned, VivariumError> {
    let selected = resolve_target_config(space, &handle.config, command.target.as_deref())?;
    let config = selected
        .as_ref()
        .map_or(&handle.config, |selected| &selected.config);
    let ready_timeout =
        dispatch::check_ready_timeout(handle.provider.slot_lifetime(), command.ready_timeout)?;
    let job_id = fresh_job_id(&task.handle);
    let (flags, rules, role_line) = resolve_dispatch(
        command,
        config,
        &handle.id.pool,
        space,
        task,
        load_rules(config, space)?,
    )?;
    let mut plan = dispatch::plan_job(
        config,
        &handle.id.pool,
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
    Ok(Planned {
        selected,
        ready_timeout,
        flags,
        role_line,
        plan,
        job_id,
    })
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

/// A fresh job id for this dispatch: the task handle, the wall clock and the
/// process id.
fn fresh_job_id(handle: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    dispatch::new_job_id(handle, nanos, std::process::id())
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
    let role = dispatch_role(command, space, task)?;
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
            &role_dispatch::project_rules_snapshot(space)?,
            config.bridge.enabled,
        ),
        None => role_dispatch::with_project_rules(
            &pool_rules,
            &role_dispatch::project_rules_snapshot(space)?,
        ),
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
    let role = dispatch_role(command, space, task)?;
    let (need, kind) = dispatch_need_kind(command, space, task, role.as_ref())?;
    let flags = base_flags(command, need, kind);
    Ok((flags, pool_rules, String::new()))
}
