//! Host tools for `POST /v1/mcp`. They act on the host rather than on one
//! grant, and only a host principal may call them. `create_project` makes a
//! project, `lane_create` adds a lane to one and `delete_project` removes
//! one.
//!
//! `create_project` calls [`vivi::project_create::create`] (the project's
//! lanes directory with its first target's bare repository, the project
//! directory with an initialised mailspace, the first target's checkout and
//! entry, the host registry entry) with the roots from [`HostConfig`], then
//! reconciles the host registry now so the new project is served without
//! waiting for the registry watcher, and mints an operator server key granted
//! on it. After the rung seeding, a registered project's pump control mode is
//! set to running through the same control state `pump_control start` writes
//! (`pump:false` leaves the mode alone) and the answer reports the inspected
//! `pump_mode`. The answer's `mcp_path` is `/v1/mcp`. `delete_project` calls
//! [`vivi::project_create::delete::delete`] and reconciles the same way, so
//! the server stops serving the project at once.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use vivi::project_create::{ProjectCreate, ProjectCreated, ProjectDelete};
use vivi::pump::control::{PumpMode, inspect, project_home, set_project_mode};

use crate::AppState;
use crate::grants::{GrantLevel, GrantRecord};
use crate::hosted;
use crate::projects::ProjectMeta;

pub(crate) mod lane;
mod rungs;
mod source;

use self::rungs::Rungs;
use super::tool;
use super::tools::{ToolError, opt_str, req_str};

/// Environment variable names for the host's project layout.
pub const ENV_PROJECTS_ROOT: &str = "HOST_PROJECTS_ROOT";
pub const ENV_LANES_ROOT: &str = "HOST_LANES_ROOT";
pub const ENV_GIT_URL_PREFIX: &str = "HOST_GIT_URL_PREFIX";
pub const ENV_DEFAULT_POOL: &str = "HOST_DEFAULT_POOL";

/// Where the host tools put things. A field left unset makes a tool answer
/// with a configuration error naming every unset variable.
#[derive(Clone, Debug, Default)]
pub struct HostConfig {
    pub projects_root: Option<PathBuf>,
    pub lanes_root: Option<PathBuf>,
    pub git_url_prefix: Option<String>,
    pub default_pool: Option<String>,
}

impl HostConfig {
    /// Read the `HOST_*` variables; a blank value counts as unset.
    #[must_use]
    pub fn from_env() -> Self {
        let get = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        Self {
            projects_root: get(ENV_PROJECTS_ROOT).map(PathBuf::from),
            lanes_root: get(ENV_LANES_ROOT).map(PathBuf::from),
            git_url_prefix: get(ENV_GIT_URL_PREFIX),
            default_pool: get(ENV_DEFAULT_POOL).map(|v| v.trim().to_owned()),
        }
    }
}

pub(super) fn tools_list() -> Value {
    json!({ "tools": [
        tool("server_version", "This server's own name and version (the crate version compiled in). Read-only; host principal only.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        tool("project_list", "The hosted projects this server serves from the host registry: one {project_id, name?, mode} each. The name is the project root's directory name, else the mailspace config name; mode is hosted, or archived for a retired project whose metadata records its archive. Read-only; host principal only.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        tool("create_project",
        "Create a project on this host: its lanes directory <HOST_LANES_ROOT>/<name> with the \
         first target's bare git repository <target>.git, a project directory with an \
         initialised mailspace and starter pump config, the first target's checkout and \
         [pump.targets.<target>] entry, and a host registry entry; seed the given rung commands \
         into [pump.rungs]; then set a registered project's pump control mode to running \
         (the same control state pump_control start writes; with pump:false the mode is left \
         alone), serve it and mint an operator API key. Returns project_root, \
         lanes_dir, target, git_url (<HOST_GIT_URL_PREFIX><name>/<target>.git), repo_path, \
         checkout_path, mailspace_id, initial_commit, readiness (what the pump still needs), \
         project_id, mcp_path, operator_key (shown once; store it), pump_mode (the inspected \
         control state) and, when the mode could not be written or read, pump_error. \
         Refuses a name that already exists, and a non-GitHub source.",
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Project name: one safe path segment" },
                "target": { "type": "string", "description": "The first target's id (lowercase letters, digits and hyphens); defaults to the project name" },
                "branch": { "type": "string", "default": "main", "description": "The first target's default branch, also its main" },
                "pool": { "type": "string", "description": "Pool whose remote_url is set to the git URL; overrides the host default" },
                "initial_commit": { "type": "boolean", "default": false, "description": "Create an empty first commit on the branch" },
                "source": { "type": "string", "description": "Fill the first target from this Git address (every branch and tag), fetched by this host; branch must be one of its branches. Excludes initial_commit. Must match ^https://github\\.com/[A-Za-z0-9._-]+/[A-Za-z0-9._-]+(\\.git)?$; local paths, file://, ssh://, other hosts, user info, query and fragment are refused" },
                "pump": { "type": "boolean", "default": true, "description": "Register the project for the pump supervisor" },
                "serve": { "type": "boolean", "default": true, "description": "Register the project for this server to host" },
                "rungs": {
                    "type": "object",
                    "properties": {
                        "task": { "type": "string" },
                        "need": { "type": "string" },
                        "main": { "type": "string" },
                        "release": { "type": "string" },
                        "timeout": { "type": "integer", "minimum": 1 },
                        "pools": { "type": "array", "items": { "type": "string" } }
                    },
                    "additionalProperties": false,
                    "description": "Rung commands, timeout and pools written to [pump.rungs] after the project is made"
                }
            },
            "required": ["name"],
            "additionalProperties": false
        })),
        tool(
            "lane_create",
            "Add a lane to a project on this host: the bare repository \
             <HOST_LANES_ROOT>/<name>/<target>.git, its checkout <project>/<target>, and \
             [pump.targets.<target>] through the guarded config write (a running pump refuses; \
             pause it first). Returns target_id, repo_path, checkout_path, git_url, \
             initial_commit and the project's pump readiness. A second add of the same target is \
             refused. Host principal only.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Existing project directory name under HOST_PROJECTS_ROOT" },
                    "target": { "type": "string", "description": "Target id (lowercase letters, digits and hyphens): [pump.targets.<target>]" },
                    "git_url_prefix": { "type": "string", "description": "Lane URL is <prefix><name>/<target>.git (SSH or file://). Defaults to HOST_GIT_URL_PREFIX" },
                    "seed": { "type": "boolean", "default": false, "description": "Create one empty seed commit on the lane's main branch" },
                    "source": { "type": "string", "description": "Fill the lane from this Git address (every branch and tag), fetched by this host; it needs a main branch. Excludes seed. Must match ^https://github\\.com/[A-Za-z0-9._-]+/[A-Za-z0-9._-]+(\\.git)?$; local paths, file://, ssh://, other hosts, user info, query and fragment are refused" }
                },
                "required": ["name", "target"],
                "additionalProperties": false
            }),
        ),
        tool(
            "delete_project",
            "Delete a project from this host: its host registration, its project directory and \
             its lanes directory <HOST_LANES_ROOT>/<name> with every lane in it, then stop serving \
             it. `confirm` must repeat the name. While a pump still holds the project's lock the \
             call unregisters, keeps the files and says so: call it again once the pump service \
             has stopped that pump. Once the files are gone every grant on the project is removed \
             and a principal left with none (the operator key create_project minted) has its keys \
             revoked; the answer names them (ungranted, keys_revoked). Host principal only.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Project name, as given to create_project" },
                    "confirm": { "type": "string", "description": "The project name again" }
                },
                "required": ["name", "confirm"],
                "additionalProperties": false
            }),
        ),
    ] })
}

/// The `server_version` host tool: this server's own name and version, the
/// same pair `initialize` answers.
pub(super) fn server_version() -> Value {
    json!({ "name": "vivi-server", "version": env!("CARGO_PKG_VERSION") })
}

/// The `project_list` host tool: every hosted project this server serves,
/// read from its own project store (one `project.json` per project id),
/// plus every retired one whose metadata records an archive. A project
/// that cannot be read, or a retired one with no archive record, is
/// skipped, not failed: the list is what the server can serve right now,
/// plus the archives it remembers (their names stay taken).
pub(super) fn project_list(state: &AppState) -> Value {
    let mut items = Vec::new();
    for meta in hosted_metas(state) {
        if state.projects.is_retired(&meta.project_id) && !meta.is_archived() {
            continue;
        }
        let mut item = json!({
            "project_id": meta.project_id,
            "mode": if meta.is_archived() { "archived" } else { "hosted" },
        });
        if let Some(name) = super::server::hosted_name(&meta) {
            item["name"] = json!(name);
        }
        items.push(item);
    }
    items.sort_by(|a, b| a["project_id"].as_str().cmp(&b["project_id"].as_str()));
    Value::Array(items)
}

/// Every hosted project's metadata in the store, retired ones included.
fn hosted_metas(state: &AppState) -> Vec<ProjectMeta> {
    let Ok(entries) = std::fs::read_dir(state.projects.projects_root()) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| std::fs::read(entry.path().join("project.json")).ok())
        .filter_map(|bytes| serde_json::from_slice::<ProjectMeta>(&bytes).ok())
        .filter(ProjectMeta::is_hosted)
        .collect()
}

/// The hosted project metadata named `name` (as [`super::server::hosted_name]
/// spells it), served or retired, archived or not.
pub(super) fn hosted_entry(state: &AppState, name: &str) -> Option<ProjectMeta> {
    hosted_metas(state)
        .into_iter()
        .find(|meta| super::server::hosted_name(meta).as_deref() == Some(name))
}

/// Take every grant on `project_id` off its principals, and revoke the keys
/// of each principal left with no grant (such as the operator key
/// `create_project` minted). Returns `(principals ungranted, keys revoked)`.
fn drop_project_grants(state: &AppState, project_id: &str) -> (Vec<String>, usize) {
    let mut ungranted = Vec::new();
    let mut revoked = 0;
    for principal in state.server_keys.list() {
        if principal.host || !principal.grants.iter().any(|g| g.project_id == project_id) {
            continue;
        }
        if state
            .server_keys
            .remove_grant(&principal.principal, project_id)
            .is_err()
        {
            continue;
        }
        if principal.grants.len() == 1 {
            revoked += state
                .server_keys
                .revoke_principal(&principal.principal)
                .unwrap_or(0);
        }
        ungranted.push(principal.principal);
    }
    (ungranted, revoked)
}

/// The host-project mutation mutex: one mutation of the host's projects
/// (create, delete, lane add) at a time. The host registry takes a
/// non-waiting lock, so two creations in flight would fail the second with
/// "another registry update is running"; holding the mutex here also keeps
/// an archived-name check and the write it guards from racing.
static CREATING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Create one project. The `/v1/mcp` host tool.
pub(super) async fn create_project(
    state: &AppState,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let (request, rungs) = build_request(&state.host, arguments)?;
    let state = state.clone();
    blocking(move || {
        let _one = CREATING.lock().unwrap_or_else(|e| e.into_inner());
        if hosted_entry(&state, &request.name).is_some_and(|meta| meta.is_archived()) {
            return Err(ToolError::new(format!(
                "{} is archived: its name cannot be reused",
                request.name
            )));
        }
        let mut created =
            vivi::project_create::create(&request).map_err(|e| ToolError::new(e.to_string()))?;
        let rungs_error = rungs.seed(&mut created).err();
        let started = start_pump(&created.project_root, request.pump);
        let served = if request.serve {
            serve_now(&state, &created.project_root)
        } else {
            Serving::NotRequested
        };
        let mut out = answer(&created, served, started);
        if let (Some(out), Some(error)) = (out.as_object_mut(), rungs_error) {
            out.insert("rungs_error".into(), Value::String(error));
        }
        Ok(out)
    })
    .await
}

/// Run blocking work on the blocking pool.
pub(super) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ToolError> + Send + 'static,
) -> Result<T, ToolError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| ToolError::new(format!("host task failed: {e}")))?
}

/// The projects root, lanes root and git URL prefix, or one error naming
/// every `HOST_*` variable that is unset.
pub(super) fn require(
    host: &HostConfig,
    tool: &str,
) -> Result<(PathBuf, PathBuf, String), ToolError> {
    match (
        host.projects_root.clone(),
        host.lanes_root.clone(),
        host.git_url_prefix.clone(),
    ) {
        (Some(projects), Some(lanes), Some(prefix)) => Ok((projects, lanes, prefix)),
        (projects, lanes, prefix) => {
            let unset: Vec<&str> = [
                (projects.is_none(), ENV_PROJECTS_ROOT),
                (lanes.is_none(), ENV_LANES_ROOT),
                (prefix.is_none(), ENV_GIT_URL_PREFIX),
            ]
            .into_iter()
            .filter_map(|(missing, var)| missing.then_some(var))
            .collect();
            Err(ToolError::new(format!(
                "{tool} is not configured on this host: {} not set",
                list_vars(&unset)
            )))
        }
    }
}

/// The lanes root alone, or one error naming `HOST_LANES_ROOT` when it is
/// unset.
pub(super) fn require_lanes(host: &HostConfig, tool: &str) -> Result<PathBuf, ToolError> {
    host.lanes_root.clone().ok_or_else(|| {
        ToolError::new(format!(
            "{tool} is not configured on this host: {ENV_LANES_ROOT} not set"
        ))
    })
}

fn list_vars(vars: &[&str]) -> String {
    match vars {
        [one] => format!("{one} is"),
        [init @ .., last] => format!("{} and {last} are", init.join(", ")),
        [] => "nothing is".to_owned(),
    }
}

fn build_request(
    host: &HostConfig,
    arguments: &Value,
) -> Result<(ProjectCreate, Rungs), ToolError> {
    let (projects_root, lanes_root, prefix) = require(host, "create_project")?;
    let mut request = ProjectCreate::new(
        req_str(arguments, "name")?,
        projects_root,
        lanes_root,
        prefix,
    );
    request.target = opt_str(arguments, "target")?.map(str::to_owned);
    request.pool = opt_str(arguments, "pool")?
        .map(str::to_owned)
        .or_else(|| host.default_pool.clone());
    if let Some(branch) = opt_str(arguments, "branch")? {
        request.branch = branch.to_owned();
    }
    request.pump = opt_bool(arguments, "pump", request.pump)?;
    request.serve = opt_bool(arguments, "serve", request.serve)?;
    request.initial_commit = opt_bool(arguments, "initial_commit", request.initial_commit)?;
    if let Some(source) = opt_str(arguments, "source")? {
        source::check(source)?;
        request.source = Some(source.to_owned());
    }
    Ok((request, Rungs::parse(arguments)?))
}

/// Delete one project. The `/v1/mcp` host tool.
pub(super) async fn delete_project(
    state: &AppState,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let (projects_root, lanes_root, _) = require(&state.host, "delete_project")?;
    let request = ProjectDelete {
        name: req_str(arguments, "name")?.to_owned(),
        projects_root,
        lanes_root,
        confirm: req_str(arguments, "confirm")?.to_owned(),
    };
    let state = state.clone();
    blocking(move || {
        let _one = CREATING.lock().unwrap_or_else(|e| e.into_inner());
        let project_id = hosted_entry(&state, &request.name).map(|meta| meta.project_id);
        let outcome = vivi::project_create::delete::delete(&request);
        // Unregistered even when the files stay (a live pump): stop serving now.
        hosted::reconcile_registry(&state.projects, &state.identities);
        let deleted = outcome.map_err(|e| ToolError::new(e.to_string()))?;
        let mut out = serde_json::to_value(&deleted)
            .map_err(|e| ToolError::new(format!("failed to encode delete_project result: {e}")))?;
        if let (Some(id), Some(object)) = (project_id, out.as_object_mut()) {
            let (ungranted, revoked) = drop_project_grants(&state, &id);
            object.insert("project_id".into(), json!(id));
            object.insert("ungranted".into(), json!(ungranted));
            object.insert("keys_revoked".into(), json!(revoked));
        }
        Ok(out)
    })
    .await
}

pub(super) fn opt_bool(arguments: &Value, name: &str, default: bool) -> Result<bool, ToolError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(_) => Err(ToolError::new(format!("{name} must be true or false"))),
    }
}

/// The operator server key `create_project` shows once.
struct OperatorKey {
    key: String,
    key_id: String,
}

/// What came of serving a created project.
enum Serving {
    NotRequested,
    Served {
        project_id: String,
        member: String,
        key: Result<OperatorKey, String>,
    },
    Failed(String),
}

/// Reconcile the host registry now (what the watcher does within seconds),
/// find the new project among the served roots, and mint its operator key.
fn serve_now(state: &AppState, root: &Path) -> Serving {
    let Some(report) = hosted::reconcile_registry(&state.projects, &state.identities) else {
        return Serving::Failed("the host registry could not be read".into());
    };
    if let Some(served) = report.served.iter().find(|s| same_dir(&s.root, root)) {
        let key = mint_operator_key(state, &served.project_id, &served.member);
        return Serving::Served {
            project_id: served.project_id.clone(),
            member: served.member.clone(),
            key,
        };
    }
    let reason = report
        .refused
        .iter()
        .find(|r| same_dir(&r.root, root))
        .map_or_else(
            || "the project is not in the served set".to_owned(),
            |r| r.reason.clone(),
        );
    Serving::Failed(reason)
}

/// A server key granted on the new project, acting as the operator role.
/// Not a per-project key: those do not authenticate on `/v1/mcp`.
fn mint_operator_key(
    state: &AppState,
    project_id: &str,
    member: &str,
) -> Result<OperatorKey, String> {
    let minted = state
        .server_keys
        .mint(
            &format!("{member}@{project_id}"),
            false,
            vec![GrantRecord {
                project_id: project_id.to_owned(),
                member: member.to_owned(),
                identity: Some("operator".to_owned()),
                level: GrantLevel::Operator,
            }],
            "operator (create_project)",
            member,
        )
        .map_err(|err| format!("{err:?}"))?;
    Ok(OperatorKey {
        key: minted.key,
        key_id: minted.key_id,
    })
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

/// What came of starting the created project's pump: the inspected control
/// mode, or `None` with an error when it could not be written or read.
struct PumpStarted {
    mode: Option<String>,
    error: Option<String>,
}

/// Set a registered project's pump control mode to running (the same control
/// state `pump_control start` writes) and report the mode as `inspect` reads
/// it back; never a synthesized one. An unregistered (`pump:false`) project's
/// mode is left alone and only read. Failures are reported in the answer, not
/// raised: the project exists either way, and creation is not rolled back.
fn start_pump(root: &Path, registered: bool) -> PumpStarted {
    let mut error = if registered {
        set_project_mode(root, PumpMode::Running)
            .err()
            .map(|e| format!("the pump control mode was not set: {e}"))
    } else {
        None
    };
    let read = (|| {
        let home =
            project_home(root).map_err(|e| format!("the pump home could not be found: {e}"))?;
        let control =
            inspect(&home).map_err(|e| format!("the pump control state could not be read: {e}"))?;
        Ok(control.mode.to_string())
    })();
    match read {
        Ok(mode) => PumpStarted {
            mode: Some(mode),
            error,
        },
        Err(read_error) => {
            error.get_or_insert(read_error);
            PumpStarted { mode: None, error }
        }
    }
}

/// The tool answer. The operator key is a secret: it is in the answer and
/// nowhere else (never logged).
fn answer(created: &ProjectCreated, serving: Serving, pump: PumpStarted) -> Value {
    let mut out = serde_json::to_value(created).unwrap_or_else(|_| json!({}));
    let fields = match serving {
        Serving::NotRequested => json!({ "served": false }),
        Serving::Failed(reason) => json!({ "served": false, "serve_error": reason }),
        Serving::Served {
            project_id,
            member,
            key,
        } => {
            let mut fields = json!({
                "served": true,
                "project_id": project_id,
                "mcp_path": "/v1/mcp",
                "member": member,
            });
            fields["operator_key"] = match key {
                Ok(minted) => json!({
                    "key": minted.key,
                    "key_id": minted.key_id,
                    "identity": "operator",
                    "note": "shown once; the server keeps only a digest",
                }),
                Err(error) => json!({ "error": format!("could not mint: {error}") }),
            };
            fields
        }
    };
    if let Some(out) = out.as_object_mut() {
        if let Some(fields) = fields.as_object() {
            out.extend(fields.clone());
        }
        out.insert(
            "pump_mode".into(),
            pump.mode.map_or(Value::Null, Value::String),
        );
        if let Some(error) = pump.error {
            out.insert("pump_error".into(), Value::String(error));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_lanes_needs_only_the_lanes_root() {
        let configured = HostConfig {
            lanes_root: Some(PathBuf::from("/lanes")),
            ..HostConfig::default()
        };
        let lanes = require_lanes(&configured, "archive_project")
            .map_err(|error| error.message)
            .unwrap();
        assert_eq!(lanes, PathBuf::from("/lanes"));
        let error = require_lanes(&HostConfig::default(), "archive_project")
            .err()
            .expect("an unset HOST_LANES_ROOT is an error")
            .message;
        assert_eq!(
            error,
            "archive_project is not configured on this host: HOST_LANES_ROOT not set"
        );
    }
}
