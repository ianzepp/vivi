//! Sign-in, sign-out, and the extractors that guard every page.

use std::sync::Arc;

use axum::Form;
use axum::extract::{FromRequestParts, RawPathParams, State};
use axum::http::header::{COOKIE, LOCATION, SET_COOKIE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::host::{ConsoleHost, Principal, ProjectRef};
use crate::html::{Html, div, text_tag, void};
use crate::layout::{bare, project_path};
use crate::pages::{html_response, not_found};
use crate::session::{self, SessionStore};

/// A key longer than this is refused without being looked up.
const MAX_KEY_LEN: usize = 512;

/// Shared by every handler.
#[derive(Clone)]
pub struct ConsoleState {
    pub host: Arc<dyn ConsoleHost>,
    pub sessions: Arc<SessionStore>,
}

/// A signed-in request. The rejection sends the browser to the sign-in page.
pub struct Authed {
    pub principal: Principal,
}

fn redirect(to: &str) -> Response {
    (StatusCode::SEE_OTHER, [(LOCATION, to.to_string())]).into_response()
}

/// The session id carried by the request's cookies, if any.
fn session_id(headers: &HeaderMap) -> Option<String> {
    for value in headers.get_all(COOKIE) {
        if let Ok(text) = value.to_str()
            && let Some(id) = session::id_from_cookie_header(text)
        {
            return Some(id.to_string());
        }
    }
    None
}

impl FromRequestParts<ConsoleState> for Authed {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ConsoleState,
    ) -> Result<Self, Self::Rejection> {
        let Some(session_id) = session_id(&parts.headers) else {
            return Err(redirect("/ui/login"));
        };
        match state.sessions.touch(&session_id) {
            Some(principal) => Ok(Self { principal }),
            None => Err(redirect("/ui/login")),
        }
    }
}

/// A signed-in request for a project the principal may read. Anything else
/// about the project (unknown, not granted) is the same plain 404.
pub struct ProjectCtx {
    pub principal: Principal,
    pub project: ProjectRef,
}

impl FromRequestParts<ConsoleState> for ProjectCtx {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ConsoleState,
    ) -> Result<Self, Self::Rejection> {
        let auth = Authed::from_request_parts(parts, state).await?;
        let params = RawPathParams::from_request_parts(parts, state)
            .await
            .map_err(IntoResponse::into_response)?;
        let key = params
            .iter()
            .find(|(name, _)| *name == "project")
            .map(|(_, value)| value.to_string());
        let found = key.and_then(|key| auth.principal.project(&key).cloned());
        match found {
            Some(project) => Ok(Self {
                principal: auth.principal,
                project,
            }),
            None => Err(not_found(&auth.principal)),
        }
    }
}

fn login_page(error: Option<&str>) -> Html {
    let mut form = Html::new();
    form.push(&text_tag("h1", &[], "vivi console"));
    form.push(&text_tag(
        "p",
        &[("class", "sub")],
        "Sign in with a vivi server API key.",
    ));
    if let Some(error) = error {
        form.push(&div("notice notice-error", &Html::text(error)));
    }
    form.push(&crate::html::tag(
        "label",
        &[("for", "key")],
        &Html::text("API key"),
    ));
    form.push(&void(
        "input",
        &[
            ("type", "password"),
            ("name", "key"),
            ("id", "key"),
            ("autocomplete", "off"),
            ("autofocus", ""),
            ("required", ""),
        ],
    ));
    form.push(&text_tag("button", &[("type", "submit")], "Sign in"));
    let card = crate::html::tag(
        "form",
        &[
            ("method", "post"),
            ("action", "/ui/login"),
            ("class", "card"),
        ],
        &form,
    );
    bare("Sign in", &card)
}

/// `GET /ui/login`.
pub async fn login_form(State(state): State<ConsoleState>, headers: HeaderMap) -> Response {
    if let Some(id) = session_id(&headers)
        && state.sessions.touch(&id).is_some()
    {
        return redirect("/ui/");
    }
    html_response(StatusCode::OK, &login_page(None))
}

/// The sign-in form body.
#[derive(Deserialize)]
pub struct LoginForm {
    key: String,
}

fn refused() -> Response {
    html_response(
        StatusCode::UNAUTHORIZED,
        &login_page(Some("That key was not accepted.")),
    )
}

/// `POST /ui/login`. The key is checked and dropped; only the principal it
/// resolves to is kept, under a random session id.
pub async fn login(State(state): State<ConsoleState>, Form(form): Form<LoginForm>) -> Response {
    let key = form.key.trim().to_string();
    if key.is_empty() || key.len() > MAX_KEY_LEN {
        return refused();
    }
    let host = state.host.clone();
    let resolved = tokio::task::spawn_blocking(move || host.authenticate(&key)).await;
    let Ok(Some(principal)) = resolved else {
        tracing::warn!("console sign-in refused");
        return refused();
    };
    let name = principal.name.clone();
    let Some(id) = state.sessions.create(principal) else {
        tracing::error!("console sign-in failed: no entropy for a session id");
        return html_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &login_page(Some("Could not start a session.")),
        );
    };
    tracing::info!(principal = %name, "console sign-in");
    (
        StatusCode::SEE_OTHER,
        [
            (LOCATION, "/ui/".to_string()),
            (SET_COOKIE, session::set_cookie(&id)),
        ],
    )
        .into_response()
}

/// `POST /ui/logout`.
pub async fn logout(State(state): State<ConsoleState>, headers: HeaderMap) -> Response {
    if let Some(id) = session_id(&headers) {
        state.sessions.destroy(&id);
    }
    (
        StatusCode::SEE_OTHER,
        [
            (LOCATION, "/ui/login".to_string()),
            (SET_COOKIE, session::clear_cookie()),
        ],
    )
        .into_response()
}

/// `GET /ui/`: the first project's overview.
pub async fn home(auth: Authed) -> Response {
    match auth.principal.projects.first() {
        Some(project) => redirect(&project_path(&auth.principal, project, "overview")),
        None => crate::pages::no_projects(&auth.principal),
    }
}
