//! The pages, and what they share.
//!
//! Each page is a module with one handler. [`routes`] lists them. A page that
//! is not built yet is served by `unbuilt`; the seat that builds it removes
//! its entry from `unbuilt::UNBUILT` and adds its own route here.

pub mod chips;
pub mod failures;
pub mod job;
pub mod job_sections;
pub mod jobs;
pub mod mail;
pub mod overview;
pub mod pools;
pub mod pump;
pub mod slot_history;
pub mod slots;
#[cfg(test)]
pub mod test_support;
pub mod text_block;
pub mod unbuilt;
pub mod widgets;

#[cfg(test)]
mod browser_test;

use axum::Router;
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use vivi_error::VivariumError;

use crate::auth::{ConsoleState, ProjectCtx};
use crate::host::Principal;
use crate::html::{Html, join, text_tag};
use crate::layout::{self, Chrome, heading, notice};

/// The page routes, all under `/ui/p/{project}/`.
pub fn routes() -> Router<ConsoleState> {
    Router::new()
        .route("/ui/p/{project}/overview", get(overview::handler))
        .route("/ui/p/{project}/slots", get(slots::handler))
        .route(
            "/ui/p/{project}/slots/{pool}/{slot}",
            get(slot_history::handler),
        )
        .route("/ui/p/{project}/jobs", get(jobs::handler))
        .route("/ui/p/{project}/jobs/{job}", get(job::handler))
        .route("/ui/p/{project}/failures", get(failures::handler))
        .route("/ui/p/{project}/pump", get(pump::handler))
        .route("/ui/p/{project}/pools", get(pools::handler))
        .route("/ui/p/{project}/mail", get(mail::list_handler))
        .route("/ui/p/{project}/mail/{handle}", get(mail::thread_handler))
        .merge(unbuilt::routes())
}

/// An HTML response.
#[must_use]
pub fn html_response(status: StatusCode, page: &Html) -> Response {
    (
        status,
        [(CONTENT_TYPE, "text/html; charset=utf-8")],
        page.to_string(),
    )
        .into_response()
}

/// The plain 404 for an unknown route or a project outside the principal.
#[must_use]
pub fn not_found(principal: &Principal) -> Response {
    let chrome = Chrome {
        principal,
        project: None,
        active: "",
        title: "Not found",
        refresh: false,
    };
    let body = join([
        heading("Not found", ""),
        notice("There is nothing at this address."),
    ]);
    html_response(StatusCode::NOT_FOUND, &layout::page(&chrome, &body))
}

/// What a principal with no projects sees after signing in.
#[must_use]
pub fn no_projects(principal: &Principal) -> Response {
    let chrome = Chrome {
        principal,
        project: None,
        active: "",
        title: "No projects",
        refresh: false,
    };
    let body = join([
        heading("No projects", ""),
        notice("This key is not granted on any hosted project, so there is nothing to show."),
    ]);
    html_response(StatusCode::OK, &layout::page(&chrome, &body))
}

/// A page that could not read what it shows.
#[must_use]
pub fn read_failed(ctx: &ProjectCtx, active: &str, message: &str) -> Response {
    let chrome = chrome_of(ctx, active, "Could not read", false);
    let body = join([
        heading("Could not read this project", ""),
        notice(message),
        text_tag(
            "p",
            &[("class", "sub")],
            "The console only reads. Nothing was changed.",
        ),
    ]);
    html_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        &layout::page(&chrome, &body),
    )
}

/// The frame for a project page.
#[must_use]
pub fn chrome_of<'a>(
    ctx: &'a ProjectCtx,
    active: &'a str,
    title: &'a str,
    refresh: bool,
) -> Chrome<'a> {
    Chrome {
        principal: &ctx.principal,
        project: Some(&ctx.project),
        active,
        title,
        refresh,
    }
}

/// Run a blocking store read off the async threads. Failures are logged in
/// full and returned as a short message for the page.
pub async fn blocking<T, F>(read: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, VivariumError> + Send + 'static,
{
    match tokio::task::spawn_blocking(read).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => {
            tracing::warn!(%error, "console read failed");
            Err(error.to_string())
        }
        Err(error) => {
            tracing::warn!(%error, "console read task failed");
            Err(String::from("the read did not finish"))
        }
    }
}

/// The current time, epoch seconds.
#[must_use]
pub fn now_epoch() -> i64 {
    chrono::Utc::now().timestamp()
}
