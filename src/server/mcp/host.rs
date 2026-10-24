//! Host tools for `POST /v1/mcp`. Today that is `create_project`: it acts on
//! the host rather than on one project, and only a host principal may call it.
//!
//! `create_project` calls [`vivi::project_create::create`] (bare repository,
//! project directory with an initialised mailspace, host registry entry) with
//! the roots from [`HostConfig`], then reconciles the host registry now so the
//! new project is served without waiting for the registry watcher, and mints
//! an operator server key granted on it. The answer's `mcp_path` is `/v1/mcp`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use vivi::project_create::{ProjectCreate, ProjectCreated};

use crate::AppState;
use crate::grants::{GrantLevel, GrantRecord};
use crate::hosted;

mod rungs;

use self::rungs::Rungs;
use super::tool;
use super::tools::{ToolError, opt_str, req_str};

/// Environment variable names for the host's project layout.
pub const ENV_PROJECTS_ROOT: &str = "HOST_PROJECTS_ROOT";
pub const ENV_LANES_ROOT: &str = "HOST_LANES_ROOT";
pub const ENV_GIT_URL_PREFIX: &str = "HOST_GIT_URL_PREFIX";
pub const ENV_DEFAULT_POOL: &str = "HOST_DEFAULT_POOL";

/// Where `create_project` puts things. A field left unset makes the tool
/// answer with a configuration error naming its variable.
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
    json!({ "tools": [tool(
        "create_project",
        "Create a project on this host: a bare git repository, a project directory with an \
         initialised mailspace and starter pump config, and a host registry entry; seed the \
         given rung commands into [pump.rungs]; then serve it and mint an operator API key. \
         Returns project_root, git_url, repo_path, \
         mailspace_id, initial_commit, readiness (what the pump still needs), project_id, \
         mcp_path and operator_key (shown once; store it). Refuses a name that already exists.",
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Project name: one safe path segment" },
                "branch": { "type": "string", "default": "main", "description": "The new repository's default branch" },
                "pool": { "type": "string", "description": "Pool whose remote_url is set to the git URL; overrides the host default" },
                "initial_commit": { "type": "boolean", "default": false, "description": "Create an empty first commit on the branch" },
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
        }),
    )] })
}

/// One creation at a time: the host registry takes a non-waiting lock, so
/// two creations in flight would fail the second with "another registry
/// update is running".
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
        let mut created =
            vivi::project_create::create(&request).map_err(|e| ToolError::new(e.to_string()))?;
        let rungs_error = rungs.seed(&mut created).err();
        let served = if request.serve {
            serve_now(&state, &created.project_root)
        } else {
            Serving::NotRequested
        };
        let mut out = answer(&created, served);
        if let (Some(out), Some(error)) = (out.as_object_mut(), rungs_error) {
            out.insert("rungs_error".into(), Value::String(error));
        }
        Ok(out)
    })
    .await
}

/// Run blocking work on the blocking pool.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ToolError> + Send + 'static,
) -> Result<T, ToolError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| ToolError::new(format!("create_project task failed: {e}")))?
}

fn build_request(
    host: &HostConfig,
    arguments: &Value,
) -> Result<(ProjectCreate, Rungs), ToolError> {
    let missing = |var: &str| {
        ToolError::new(format!(
            "create_project is not configured on this host: {var} is not set"
        ))
    };
    let projects_root = host
        .projects_root
        .clone()
        .ok_or_else(|| missing(ENV_PROJECTS_ROOT))?;
    let lanes_root = host
        .lanes_root
        .clone()
        .ok_or_else(|| missing(ENV_LANES_ROOT))?;
    let prefix = host
        .git_url_prefix
        .clone()
        .ok_or_else(|| missing(ENV_GIT_URL_PREFIX))?;
    let mut request = ProjectCreate::new(
        req_str(arguments, "name")?,
        projects_root,
        lanes_root,
        prefix,
    );
    request.pool = opt_str(arguments, "pool")?
        .map(str::to_owned)
        .or_else(|| host.default_pool.clone());
    if let Some(branch) = opt_str(arguments, "branch")? {
        request.branch = branch.to_owned();
    }
    request.pump = opt_bool(arguments, "pump", request.pump)?;
    request.serve = opt_bool(arguments, "serve", request.serve)?;
    request.initial_commit = opt_bool(arguments, "initial_commit", request.initial_commit)?;
    Ok((request, Rungs::parse(arguments)?))
}

fn opt_bool(arguments: &Value, name: &str, default: bool) -> Result<bool, ToolError> {
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

/// The tool answer. The operator key is a secret: it is in the answer and
/// nowhere else (never logged).
fn answer(created: &ProjectCreated, serving: Serving) -> Value {
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
    if let (Some(out), Some(fields)) = (out.as_object_mut(), fields.as_object()) {
        out.extend(fields.clone());
    }
    out
}
