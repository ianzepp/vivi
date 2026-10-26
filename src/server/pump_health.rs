//! `GET /v1/projects/{id}/pump/health`: a read-only health verdict over a
//! hosted project's pump, for monitors that cannot read the host's files.
//!
//! The answer is `crate::pump::health` as JSON: the library opens `pump.db`
//! read-only in one read transaction, never migrates it, and finds the pump
//! process from `/proc/locks` without touching the pump lock. Nothing here
//! can pause, stop, requeue, withdraw or replan; there is no write route.
//!
//! Any authenticated member of the project may read it (identity token or
//! API key). A project the server owns end to end has no pump: `404`.

use std::path::{Path, PathBuf};

use crate::pump::health::{HealthOptions, health};
use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::Value;

use crate::server::AppState;
use crate::server::projects::ProjectStore;
use crate::server::projects_api::{authenticate_caller, require_existing_project};
use crate::server::rooms::error_response;

/// The query string: both optional, both whole seconds.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthQuery {
    /// A previous answer's `as_of`: report what changed after it.
    pub since: Option<u64>,
    /// Override the stale threshold.
    pub stale_after: Option<u64>,
}

impl HealthQuery {
    fn options(&self) -> HealthOptions {
        HealthOptions {
            since: self.since,
            stale_after: self.stale_after,
            now: None,
        }
    }
}

pub async fn pump_health_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    query: Result<Query<HealthQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_existing_project(&state, &id).await {
        return response;
    }
    if let Err(response) = authenticate_caller(&state, &id, &headers).await {
        return response;
    }
    let Ok(Query(query)) = query else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "query takes only since and stale_after, as whole seconds",
        );
    };
    let store = state.projects.clone();
    match tokio::task::spawn_blocking(move || read(&store, &id, &query.options())).await {
        Ok(Ok(Some(body))) => Json(body).into_response(),
        Ok(Ok(None)) => error_response(
            StatusCode::NOT_FOUND,
            "not_hosted",
            "this project is not a hosted mailspace; it has no pump",
        ),
        Ok(Err(message)) => error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal", &message),
        Err(_) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "pump health task failed",
        ),
    }
}

/// The health JSON of `project_id`'s pump; `None` when it is not hosted.
///
/// # Errors
/// Returns the library's message when the hosted mailspace cannot be opened.
pub fn read(
    store: &ProjectStore,
    project_id: &str,
    opts: &HealthOptions,
) -> Result<Option<Value>, String> {
    let Some(root) = hosted_root(store, project_id) else {
        return Ok(None);
    };
    pump_json(&root, opts).map(Some)
}

fn hosted_root(store: &ProjectStore, project_id: &str) -> Option<PathBuf> {
    store.load_meta(project_id).ok()?.mailspace_root
}

fn pump_json(root: &Path, opts: &HealthOptions) -> Result<Value, String> {
    let answer = health(root, opts).map_err(|error| format!("pump health: {error}"))?;
    serde_json::to_value(answer).map_err(|error| format!("pump health encode: {error}"))
}
