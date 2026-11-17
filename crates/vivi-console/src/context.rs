//! The request context every page gets: the projects to show, and for a
//! project page the project its URL names. There is no sign-in.

use std::sync::Arc;

use axum::extract::{FromRequestParts, RawPathParams};
use axum::http::StatusCode;
use axum::http::header::LOCATION;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};

use crate::host::{Catalog, ConsoleHost, ProjectRef};
use crate::layout::project_path;
use crate::pages::not_found;

/// Shared by every handler.
#[derive(Clone)]
pub struct ConsoleState {
    pub host: Arc<dyn ConsoleHost>,
}

/// The projects to show, listed by the host for this request.
pub struct Shown {
    pub catalog: Catalog,
}

impl FromRequestParts<ConsoleState> for Shown {
    type Rejection = Response;

    async fn from_request_parts(
        _parts: &mut Parts,
        state: &ConsoleState,
    ) -> Result<Self, Self::Rejection> {
        let host = state.host.clone();
        match tokio::task::spawn_blocking(move || host.projects()).await {
            Ok(projects) => Ok(Self {
                catalog: Catalog { projects },
            }),
            Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        }
    }
}

/// A request for one project. An unknown project is a plain 404.
pub struct ProjectCtx {
    pub catalog: Catalog,
    pub project: ProjectRef,
}

impl FromRequestParts<ConsoleState> for ProjectCtx {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ConsoleState,
    ) -> Result<Self, Self::Rejection> {
        let shown = Shown::from_request_parts(parts, state).await?;
        let params = RawPathParams::from_request_parts(parts, state)
            .await
            .map_err(IntoResponse::into_response)?;
        let key = params
            .iter()
            .find(|(name, _)| *name == "project")
            .map(|(_, value)| value.to_string());
        let found = key.and_then(|key| shown.catalog.project(&key).cloned());
        match found {
            Some(project) => Ok(Self {
                catalog: shown.catalog,
                project,
            }),
            None => Err(not_found(&shown.catalog)),
        }
    }
}

/// `GET /ui/`: the first project's overview.
pub async fn home(shown: Shown) -> Response {
    match shown.catalog.projects.first() {
        Some(project) => (
            StatusCode::SEE_OTHER,
            [(LOCATION, project_path(&shown.catalog, project, "overview"))],
        )
            .into_response(),
        None => crate::pages::no_projects(&shown.catalog),
    }
}
