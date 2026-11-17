//! `GET /ui/p/{project}/pools`: roles, pools and cadences, read from the
//! project config. Known, non-secret fields only (see `data::pools`).

use axum::http::StatusCode;
use axum::response::Response;

use super::{blocking, chrome_of, html_response, read_failed};
use crate::context::ProjectCtx;
use crate::data::pools::{self, DispatchView, PoolView, PoolsData, RoleView};
use crate::html::{Html, div, join, span, text_tag};
use crate::layout::{self, heading, mono, notice, pill, row_classed, table};

/// The handler.
pub async fn handler(ctx: ProjectCtx) -> Response {
    let root = ctx.project.mailspace_root.clone();
    match blocking(move || pools::load(&root)).await {
        Ok(data) => {
            let chrome = chrome_of(&ctx, "pools", "Pools and roles", false);
            let body = render(&ctx, &data);
            html_response(StatusCode::OK, &layout::page(&chrome, &body))
        }
        Err(message) => read_failed(&ctx, "pools", &message),
    }
}

/// The page body.
#[must_use]
pub fn render(ctx: &ProjectCtx, data: &PoolsData) -> Html {
    let mut body = heading("Pools and roles", &ctx.project.name);
    body.push(&text_tag("h2", &[], "Roles"));
    body.push(&roles(&data.roles));
    body.push(&text_tag("h2", &[], "Pools"));
    body.push(&pools_table(&data.pools));
    if let Some(dispatch) = &data.dispatch {
        body.push(&text_tag("h2", &[], "Pump dispatch"));
        body.push(&dispatch_view(dispatch));
    }
    body
}

fn opt(text: Option<&str>) -> Html {
    text.filter(|text| !text.is_empty())
        .map_or_else(|| span("dim", "-"), Html::text)
}

fn list(items: &[String]) -> Html {
    if items.is_empty() {
        return span("dim", "-");
    }
    join(items.iter().map(|item| join([mono(item), Html::text(" ")])))
}

fn roles(roles: &[RoleView]) -> Html {
    if roles.is_empty() {
        return notice("This project declares no roles.");
    }
    let rows = roles
        .iter()
        .map(|role| {
            let status = match role.status.as_str() {
                "active" => pill("ok", "active"),
                other => pill("muted", other),
            };
            row_classed(vec![
                ("", mono(&role.name)),
                ("", opt(role.kind.as_deref())),
                ("", status),
                ("", opt(role.pool.as_deref())),
                ("", list(&role.models)),
                ("", opt(role.harness.as_deref())),
                ("", opt(role.cadence.as_deref())),
            ])
        })
        .collect();
    table(
        "table",
        &[
            "Role", "Kind", "Status", "Pool", "Models", "Harness", "Cadence",
        ],
        rows,
    )
}

fn pools_table(pools: &[PoolView]) -> Html {
    if pools.is_empty() {
        return notice("This project declares no pools.");
    }
    let rows = pools.iter().map(pool_row).collect();
    table(
        "table",
        &[
            "Pool",
            "Provider",
            "Slots",
            "Fallback",
            "Caps",
            "Bands",
            "Harness",
            "Routed slots",
            "Roles",
        ],
        rows,
    )
}

fn pool_row(pool: &PoolView) -> Html {
    let caps: Vec<String> = pool
        .caps
        .iter()
        .map(|(model, cap)| format!("{model}: {cap}"))
        .collect();
    let bands: Vec<String> = pool
        .bands
        .iter()
        .map(|(band, choices)| format!("{band}: {}", choices.join(", ")))
        .collect();
    let harness = pool.harness.as_ref().map_or_else(
        || span("dim", "-"),
        |(name, models)| {
            join([
                mono(name),
                span("dim", &format!(" ({} models)", models.len())),
            ])
        },
    );
    let slots: Vec<String> = pool
        .declared_slots
        .iter()
        .map(|slot| format!("{} on {}", slot.name, slot.host))
        .collect();
    row_classed(vec![
        ("", mono(&pool.name)),
        ("", Html::text(&pool.provider)),
        ("num", Html::text(&pool.slots.to_string())),
        ("", opt(pool.fallback.as_deref())),
        ("", stacked(&caps)),
        ("", stacked(&bands)),
        ("", harness),
        ("", stacked(&slots)),
        ("", list(&pool.roles)),
    ])
}

/// One line per item.
fn stacked(items: &[String]) -> Html {
    if items.is_empty() {
        return span("dim", "-");
    }
    join(items.iter().map(|item| div("", &Html::text(item))))
}

fn dispatch_view(dispatch: &DispatchView) -> Html {
    let mut out = text_tag(
        "p",
        &[("class", "sub")],
        &format!("The pump ticks every {} s.", dispatch.tick),
    );
    if !dispatch.lanes.is_empty() {
        let rows = dispatch
            .lanes
            .iter()
            .map(|(model, cap)| {
                row_classed(vec![
                    ("", mono(model)),
                    ("num", Html::text(&cap.to_string())),
                ])
            })
            .collect();
        out.push(&table(
            "table pools",
            &["Lane model", "Units at once"],
            rows,
        ));
    }
    if dispatch.kinds.is_empty() {
        return out;
    }
    let rows = dispatch
        .kinds
        .iter()
        .map(|kind| {
            let default = if kind.name == dispatch.default_kind {
                pill("muted", "default")
            } else {
                Html::new()
            };
            row_classed(vec![
                ("", join([mono(&kind.name), Html::text(" "), default])),
                ("", list(&kind.pools)),
                (
                    "num",
                    kind.max
                        .map_or_else(|| span("dim", "-"), |max| Html::text(&max.to_string())),
                ),
            ])
        })
        .collect();
    out.push(&table(
        "table",
        &["Unit kind", "Pools", "In flight at most"],
        rows,
    ));
    out
}

#[cfg(test)]
#[path = "pools_test.rs"]
mod tests;
