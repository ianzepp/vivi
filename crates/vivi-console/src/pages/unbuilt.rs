//! Menu pages that are not built yet.
//!
//! Each shows a plain "not built yet" page. The seat that builds one removes
//! it from [`UNBUILT`] (the router refuses two routes on one path) and adds
//! its route to `pages::routes`.

use axum::Router;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;

use super::{chrome_of, html_response};
use crate::auth::{ConsoleState, ProjectCtx};
use crate::html::join;
use crate::layout::{self, heading, notice};

/// Menu segment and page name, for every page still to come.
pub const UNBUILT: &[(&str, &str)] = &[("needs", "Needs")];

/// Routes for each unbuilt page.
pub fn routes() -> Router<ConsoleState> {
    let mut router = Router::new();
    for (segment, name) in UNBUILT {
        let (segment, name) = (*segment, *name);
        router = router.route(
            &format!("/ui/p/{{project}}/{segment}"),
            get(move |ctx: ProjectCtx| async move { page(&ctx, segment, name) }),
        );
    }
    router
}

fn page(ctx: &ProjectCtx, active: &str, name: &str) -> Response {
    let chrome = chrome_of(ctx, active, name, false);
    let body = join([heading(name, ""), notice("This page is not built yet.")]);
    html_response(StatusCode::OK, &layout::page(&chrome, &body))
}
