//! The host MCP endpoint, `POST /v1/host/mcp`: tools that act on the host
//! rather than on one project. Today that is `create_project`.
//!
//! The caller is the host operator, never a project member: the Bearer must
//! equal `HOST_OPERATOR_TOKEN` (constant-time compare). Without that variable
//! the endpoint answers 404 and does nothing, so it is never open. A project
//! identity token or API key is not the operator token and is refused.
//!
//! `create_project` calls [`vivi::project_create::create`] (bare repository,
//! project directory with an initialised mailspace, host registry entry) with
//! the roots from [`HostConfig`], then reconciles the host registry now so the
//! new project is served at its project endpoint without waiting for the
//! registry watcher, and mints an operator API key for it.

use std::path::{Path, PathBuf};

use axum::body::Bytes;
use axum::extract::State;
use axum::extract::rejection::BytesRejection;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use vivi::project_create::{ProjectCreate, ProjectCreated};

use crate::AppState;
use crate::hosted;
use crate::projects_api::{missing_bearer, presented_bearer};
use crate::rooms::error_response;

mod rungs;

use self::rungs::Rungs;
use super::tools::{ToolError, opt_str, req_str};
use super::{Surface, respond, tool, tool_result};

pub(super) const INSTRUCTIONS: &str = "This is the vivi host endpoint, for the host operator. \
create_project makes a project (git repository, mailspace, host registration) and serves it; \
the answer carries the project id, its MCP path, and an operator API key shown once.";

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

/// `POST /v1/host/mcp` — Streamable HTTP MCP for the host operator.
pub async fn host_mcp_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let Some(expected) = state.host_operator_token.as_deref() else {
        return error_response(
            StatusCode::NOT_FOUND,
            "host_endpoint_disabled",
            "the host endpoint is disabled: HOST_OPERATOR_TOKEN is not set",
        );
    };
    let Some(provided) = presented_bearer(&headers) else {
        return missing_bearer();
    };
    if !bool::from(provided.as_bytes().ct_eq(expected.as_bytes())) {
        return error_response(
            StatusCode::FORBIDDEN,
            "host_unauthorized",
            "the host operator token is required; a project token or API key is not accepted",
        );
    }
    respond(&Surface::Host { state: &state }, &headers, body).await
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

pub(super) async fn call_tool(state: &AppState, name: &str, arguments: Value) -> Value {
    let result = match name {
        "create_project" => create_project(state, &arguments).await,
        other => Err(ToolError::new(format!("unknown tool '{other}'"))),
    };
    match result {
        Ok(value) => tool_result(value, false),
        Err(err) => tool_result(json!({ "error": err.message }), true),
    }
}

/// One creation at a time: the host registry takes a non-waiting lock, so
/// two creations in flight would fail the second with "another registry
/// update is running".
static CREATING: std::sync::Mutex<()> = std::sync::Mutex::new(());

async fn create_project(state: &AppState, arguments: &Value) -> Result<Value, ToolError> {
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

/// What came of serving a created project.
enum Serving {
    NotRequested,
    Served {
        project_id: String,
        member: String,
        key: Result<crate::keys::MintedKey, String>,
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
        let key = state
            .keys
            .mint(
                &served.project_id,
                &served.member,
                "operator (create_project)",
                Some("operator"),
            )
            .map_err(|e| format!("{e:?}"));
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
                "mcp_path": format!("/v1/projects/{project_id}/mcp"),
                "member": member,
            });
            fields["operator_key"] = match key {
                Ok(minted) => json!({
                    "key": minted.key,
                    "key_id": minted.key_id,
                    "identity": minted.identity,
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
