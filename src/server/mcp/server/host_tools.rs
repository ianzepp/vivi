//! The host tools of the server endpoint: project lifecycle, jobs, and the
//! key and grant admin. Every one needs a host principal; each write is
//! audited and rate limited under the `host` bucket.

use axum::http::StatusCode;
use axum::response::Response;
use serde_json::Value;

use crate::server::AppState;
use crate::server::audit::AuditOutcome;
use crate::server::mcp::{host, tool_result};
use crate::server::rooms::error_response;

use super::{HOST_PRINCIPAL, ServerPrincipal, allow_write, audit, internal, refused, settle};

pub(super) async fn host_tool(
    state: &AppState,
    auth: &ServerPrincipal,
    name: &str,
    arguments: Value,
) -> Result<Value, Response> {
    // The read host tools: `key_list` names keys, the others answer server
    // facts; none is audited or rate limited.
    let write = !matches!(
        name,
        "key_list" | "server_version" | "project_list" | "job_show" | "job_list"
    );
    if !auth.host {
        let message = format!("{name} is a host tool; this key is not a host principal");
        return Ok(refused(state, auth, None, name, write, &message).await);
    }
    match name {
        "server_version" => Ok(tool_result(host::server_version(), false)),
        "project_list" => project_list(state, auth).await,
        "job_show" | "job_list" => job_read(state, auth, name, arguments).await,
        "job_create" => job_create(state, auth, arguments).await,
        "create_project" | "lane_create" | "archive_project" | "project_register"
        | "project_unregister" | "restore_project" | "purge_project" => {
            lifecycle(state, auth, name, arguments).await
        }
        _ => key_tool(state, auth, name, write, arguments).await,
    }
}

async fn project_list(state: &AppState, auth: &ServerPrincipal) -> Result<Value, Response> {
    let list_state = state.clone();
    match tokio::task::spawn_blocking(move || host::project_list(&list_state)).await {
        Ok(value) => Ok(tool_result(value, false)),
        Err(err) => Err(internal(state, auth, None, "project_list", false, err).await),
    }
}

async fn job_read(
    state: &AppState,
    auth: &ServerPrincipal,
    name: &str,
    arguments: Value,
) -> Result<Value, Response> {
    let job_state = state.clone();
    let show = name == "job_show";
    match tokio::task::spawn_blocking(move || {
        if show {
            crate::server::jobs::show(&job_state, &arguments)
        } else {
            crate::server::jobs::list(&job_state, &arguments)
        }
    })
    .await
    {
        Ok(outcome) => Ok(settle(state, auth, None, name, false, outcome).await),
        Err(err) => Err(internal(state, auth, None, name, false, err).await),
    }
}

async fn job_create(
    state: &AppState,
    auth: &ServerPrincipal,
    arguments: Value,
) -> Result<Value, Response> {
    let name = "job_create";
    if let Err(message) = allow_write(state, HOST_PRINCIPAL, auth) {
        return Ok(refused(state, auth, None, name, true, &message).await);
    }
    let job_state = state.clone();
    match tokio::task::spawn_blocking(move || crate::server::jobs::create(&job_state, &arguments))
        .await
    {
        Ok(outcome) => Ok(settle(state, auth, None, name, true, outcome).await),
        Err(err) => Err(internal(state, auth, None, name, true, err).await),
    }
}

/// Create, lane, archive, register, unregister, restore, purge.
async fn lifecycle(
    state: &AppState,
    auth: &ServerPrincipal,
    name: &str,
    arguments: Value,
) -> Result<Value, Response> {
    if let Err(message) = allow_write(state, HOST_PRINCIPAL, auth) {
        return Ok(refused(state, auth, None, name, true, &message).await);
    }
    let outcome = match name {
        "create_project" => host::create_project(state, &arguments).await,
        "lane_create" => host::lane::lane_create(state, &arguments).await,
        "archive_project" => host::archive_project(state, &auth.principal, &arguments).await,
        "project_register" => host::project_register(state, &arguments).await,
        "project_unregister" => host::project_unregister(state, &arguments).await,
        "restore_project" => host::restore_project(state, &arguments).await,
        _ => host::purge_project(state, &arguments).await,
    };
    let outcome = outcome.map_err(|err| err.message);
    Ok(settle(state, auth, None, name, true, outcome).await)
}

/// The key and grant admin tools, on a blocking thread.
async fn key_tool(
    state: &AppState,
    auth: &ServerPrincipal,
    name: &str,
    write: bool,
    arguments: Value,
) -> Result<Value, Response> {
    if write && let Err(message) = allow_write(state, HOST_PRINCIPAL, auth) {
        return Ok(refused(state, auth, None, name, write, &message).await);
    }
    let admin_state = state.clone();
    let owned_name = name.to_owned();
    let result = match tokio::task::spawn_blocking(move || {
        super::key_admin::key_admin(&admin_state, &owned_name, &arguments)
    })
    .await
    {
        Ok(result) => result,
        Err(err) => {
            let message = format!("key admin task failed: {err}");
            if write {
                audit(state, auth, None, name, AuditOutcome::Error, &message).await;
            }
            return Err(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "storage failure",
            ));
        }
    };
    let result = result.map_err(|err| err.message);
    Ok(settle(state, auth, None, name, write, result).await)
}
