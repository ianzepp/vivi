//! The console router: everything under `/ui`.

use std::sync::Arc;

use axum::Router;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderName, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::context::{self, ConsoleState};
use crate::host::ConsoleHost;
use crate::pages;

const CSS: &str = include_str!("assets/console.css");
const JS: &str = include_str!("assets/console.js");

/// No scripts or styles but our own files; nothing may frame the console.
const CSP: &str = "default-src 'none'; style-src 'self'; script-src 'self'; \
                   connect-src 'self'; form-action 'self'; base-uri 'none'; \
                   frame-ancestors 'none'";

/// The console routes over `host`. Merge the result into the server's router.
pub fn router(host: Arc<dyn ConsoleHost>) -> Router {
    let state = ConsoleState { host };
    Router::new()
        .route("/ui", get(context::home))
        .route("/ui/", get(context::home))
        .route("/ui/static/console.css", get(css))
        .route("/ui/static/console.js", get(js))
        .merge(pages::routes())
        .layer(axum::middleware::map_response(security_headers))
        .with_state(state)
}

async fn css() -> Response {
    static_file("text/css; charset=utf-8", CSS)
}

async fn js() -> Response {
    static_file("text/javascript; charset=utf-8", JS)
}

fn static_file(content_type: &'static str, body: &'static str) -> Response {
    (
        [
            (CONTENT_TYPE, content_type),
            (CACHE_CONTROL, "private, max-age=300"),
        ],
        body,
    )
        .into_response()
}

async fn security_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    let fixed: [(HeaderName, &'static str); 4] = [
        (CACHE_CONTROL, "no-store"),
        (HeaderName::from_static("content-security-policy"), CSP),
        (HeaderName::from_static("x-content-type-options"), "nosniff"),
        (HeaderName::from_static("referrer-policy"), "no-referrer"),
    ];
    for (name, value) in fixed {
        if !headers.contains_key(&name) {
            headers.insert(name, HeaderValue::from_static(value));
        }
    }
    response
}

#[cfg(test)]
#[path = "router_test.rs"]
mod tests;
