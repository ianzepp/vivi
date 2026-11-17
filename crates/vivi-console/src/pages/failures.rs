//! `GET /ui/p/{project}/failures?window=1h|24h|7d&pool=`: what failed, why,
//! and which needs keep coming back.

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;

use super::chips::{self, Chip};
use super::widgets::missing_runs_notice;
use super::{blocking, chrome_of, html_response, now_epoch, read_failed};
use crate::context::ProjectCtx;
use crate::data::RunsState;
use crate::data::failures::{
    self, FailuresData, HotPath, KindCount, RedCommand, RepeatNeed, Trend,
};
use crate::data::window::Window;
use crate::html::{Html, join, link_text, span, tag, text_tag, url_encode};
use crate::layout::{self, heading, mono, notice, pill, project_path, row_classed, table};

/// The query string.
#[derive(Debug, Default, Deserialize)]
pub struct FailuresQuery {
    pub window: Option<String>,
    pub pool: Option<String>,
}

/// The handler.
pub async fn handler(ctx: ProjectCtx, Query(query): Query<FailuresQuery>) -> Response {
    let root = ctx.project.mailspace_root.clone();
    let now = now_epoch();
    let window = Window::parse(query.window.as_deref().unwrap_or_default()).unwrap_or_default();
    let pool = query.pool.unwrap_or_default().trim().to_string();
    match blocking(move || failures::load(&root, now, window, &pool)).await {
        Ok(data) => {
            let chrome = chrome_of(&ctx, "failures", "Failures", true);
            let body = render(&ctx, window, &data);
            html_response(StatusCode::OK, &layout::page(&chrome, &body))
        }
        Err(message) => read_failed(&ctx, "failures", &message),
    }
}

/// The page body.
#[must_use]
pub fn render(ctx: &ProjectCtx, window: Window, state: &RunsState<FailuresData>) -> Html {
    let mut body = heading("Failures", &ctx.project.name);
    let RunsState::Ready(data) = state else {
        body.push(&filter_rows(ctx, window, "", &[]));
        if let Some(note) = missing_runs_notice(state) {
            body.push(&note);
        }
        return body;
    };
    body.push(&filter_rows(ctx, data.window, &data.pool, &data.pools));
    body.push(&summary(data));
    body.push(&kinds_table(ctx, data));
    body.push(&text_tag("h2", &[], "Hot files (conflicts)"));
    body.push(&hot_table(&data.hot_paths));
    body.push(&text_tag("h2", &[], "Red landings by failing command"));
    body.push(&red_table(&data.red_commands));
    body.push(&text_tag("h2", &[], "Needs that keep coming back"));
    body.push(&repeat_table(ctx, &data.repeat_needs));
    body
}

fn page_href(ctx: &ProjectCtx, window: Window, pool: &str) -> String {
    let query = chips::query(&[("window", window.word()), ("pool", pool)]);
    project_path(&ctx.catalog, &ctx.project, &format!("failures{query}"))
}

fn filter_rows(ctx: &ProjectCtx, window: Window, pool: &str, pools: &[String]) -> Html {
    let windows: Vec<Chip> = Window::ALL
        .iter()
        .map(|each| Chip {
            text: each.word().to_string(),
            href: page_href(ctx, *each, pool),
            active: *each == window,
        })
        .collect();
    let mut out = chips::row("Window", &windows);
    if !pools.is_empty() || !pool.is_empty() {
        let mut names: Vec<Chip> = vec![Chip {
            text: "all".into(),
            href: page_href(ctx, window, ""),
            active: pool.is_empty(),
        }];
        names.extend(pools.iter().map(|name| Chip {
            text: name.clone(),
            href: page_href(ctx, window, name),
            active: name == pool,
        }));
        out.push(&chips::row("Pool", &names));
    }
    out
}

fn summary(data: &FailuresData) -> Html {
    let failed = data.failed();
    let text = format!(
        "{failed} failed of {} collected in the last {}.",
        data.collected,
        data.window.word()
    );
    if failed == 0 {
        notice(&format!("No failures. {text}"))
    } else {
        text_tag("p", &[("class", "sub")], &text)
    }
}

fn jobs_href(ctx: &ProjectCtx, data: &FailuresData, kind: &str) -> String {
    let query = format!(
        "jobs?failed=1&kind={}&window={}{}",
        url_encode(kind),
        data.window.word(),
        if data.pool.is_empty() {
            String::new()
        } else {
            format!("&pool={}", url_encode(&data.pool))
        }
    );
    project_path(&ctx.catalog, &ctx.project, &query)
}

fn kinds_table(ctx: &ProjectCtx, data: &FailuresData) -> Html {
    if data.kinds.is_empty() {
        return Html::new();
    }
    let total = data.failed().max(1);
    let rows = data
        .kinds
        .iter()
        .map(|kind| {
            row_classed(vec![
                ("", link_text(&jobs_href(ctx, data, kind.code), kind.label)),
                ("num", Html::text(&kind.count.to_string())),
                ("", share_bar(kind.count, total)),
                ("", trend_cell(kind)),
            ])
        })
        .collect();
    table("table", &["Kind", "Count", "Share", "Trend"], rows)
}

/// A bar for `count` of `total`, drawn as SVG so the page needs no inline
/// style.
fn share_bar(count: i64, total: i64) -> Html {
    let percent = count as f64 * 100.0 / total as f64;
    let label = format!("{percent:.0}%");
    let bar = tag(
        "svg",
        &[
            ("class", "meter"),
            ("viewBox", "0 0 100 8"),
            ("preserveAspectRatio", "none"),
            ("role", "img"),
            ("aria-label", &label),
        ],
        &join([
            crate::html::empty(
                "rect",
                &[("class", "meter-bg"), ("width", "100"), ("height", "8")],
            ),
            crate::html::empty(
                "rect",
                &[
                    ("class", "meter-fill"),
                    ("width", &format!("{percent:.1}")),
                    ("height", "8"),
                ],
            ),
        ]),
    );
    join([bar, span("meter-label", &label)])
}

fn trend_cell(kind: &KindCount) -> Html {
    let was = format!("was {}", kind.previous);
    match kind.trend() {
        Trend::New => pill("conflict", "new"),
        Trend::Up => join([pill("failed", "rising"), span("dim", &was)]),
        Trend::Down => join([pill("ok", "falling"), span("dim", &was)]),
        Trend::Flat => join([span("dim", "flat, "), span("dim", &was)]),
    }
}

fn hot_table(paths: &[HotPath]) -> Html {
    if paths.is_empty() {
        return notice("No conflicts recorded with their files in this window.");
    }
    let rows = paths
        .iter()
        .map(|path| {
            row_classed(vec![
                ("", mono(&path.prefix)),
                ("num", Html::text(&path.count.to_string())),
            ])
        })
        .collect();
    table("table", &["Path", "Conflicting runs"], rows)
}

fn red_table(commands: &[RedCommand]) -> Html {
    if commands.is_empty() {
        return notice("No red landings recorded with their command in this window.");
    }
    let rows = commands
        .iter()
        .map(|command| {
            row_classed(vec![
                ("", mono(&command.command)),
                ("num", Html::text(&command.count.to_string())),
            ])
        })
        .collect();
    table("table", &["Failing command", "Red landings"], rows)
}

fn repeat_table(ctx: &ProjectCtx, needs: &[RepeatNeed]) -> Html {
    if needs.is_empty() {
        return notice(&format!(
            "No need has {} or more runs and no landing in this window.",
            failures::REPEAT_RUNS
        ));
    }
    let rows = needs
        .iter()
        .map(|need| {
            let href = project_path(
                &ctx.catalog,
                &ctx.project,
                &format!("needs/{}", url_encode(&need.need)),
            );
            let mix = join(
                need.mix
                    .iter()
                    .map(|(label, count)| pill("muted", &format!("{label} x{count}"))),
            );
            row_classed(vec![
                ("", link_text(&href, &crate::format::short(&need.need))),
                ("num", Html::text(&need.runs.to_string())),
                ("", mix),
            ])
        })
        .collect();
    table("table", &["Need", "Runs", "Outcomes"], rows)
}

#[cfg(test)]
#[path = "failures_test.rs"]
mod tests;
