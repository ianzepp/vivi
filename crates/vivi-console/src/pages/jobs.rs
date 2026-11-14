//! `GET /ui/p/{project}/jobs`: runs newest first. Filters and the page
//! cursor live in the query string, so any view can be pasted.

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;

use super::chips;
use super::slot_history::wall;
use super::widgets::{missing_runs_notice, option, result_pills, slot_href, time_cell};
use super::{blocking, chrome_of, html_response, now_epoch, read_failed};
use crate::auth::ProjectCtx;
use crate::data::RunsState;
use crate::data::failures::KINDS;
use crate::data::jobs::{self, Cursor, JobFilter, JobListRow, JobsPage, OUTCOMES};
use crate::data::window::Window;
use crate::format;
use crate::html::{Html, join, link, link_text, span, tag, text_tag, url_encode, void};
use crate::layout::{self, heading, mono, notice, row_classed, table};

/// The query string. Everything is optional text: a form sends blank fields,
/// and a bad value is ignored, never a 400.
#[derive(Debug, Default, Deserialize)]
pub struct JobsQuery {
    pub outcome: Option<String>,
    pub failed: Option<String>,
    pub kind: Option<String>,
    pub pool: Option<String>,
    pub slot: Option<String>,
    pub model: Option<String>,
    pub need: Option<String>,
    pub window: Option<String>,
    pub before: Option<String>,
    pub before_id: Option<String>,
}

fn nonblank(value: Option<&String>) -> Option<String> {
    value
        .map(|text| text.trim())
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

impl JobsQuery {
    fn filter(&self) -> JobFilter {
        JobFilter {
            outcome: nonblank(self.outcome.as_ref())
                .filter(|word| OUTCOMES.contains(&word.as_str())),
            failed: self.failed.as_deref() == Some("1"),
            kind: nonblank(self.kind.as_ref())
                .filter(|code| KINDS.iter().any(|(known, _)| known == code)),
            pool: nonblank(self.pool.as_ref()),
            slot: nonblank(self.slot.as_ref()),
            model: nonblank(self.model.as_ref()),
            need: nonblank(self.need.as_ref()),
            window: self
                .window
                .as_deref()
                .and_then(Window::parse)
                .unwrap_or_default(),
        }
    }

    fn cursor(&self) -> Option<Cursor> {
        let Ok(at) = self.before.as_deref()?.trim().parse::<i64>() else {
            return None;
        };
        let id = self
            .before_id
            .as_deref()
            .map_or(i64::MAX, |id| id.trim().parse().unwrap_or(i64::MAX));
        Some(Cursor {
            dispatched_at: at,
            id,
        })
    }
}

/// The handler.
pub async fn handler(ctx: ProjectCtx, Query(query): Query<JobsQuery>) -> Response {
    let root = ctx.project.mailspace_root.clone();
    let now = now_epoch();
    let (filter, cursor) = (query.filter(), query.cursor());
    let read_filter = filter.clone();
    match blocking(move || jobs::load(&root, now, &read_filter, cursor)).await {
        Ok(page) => {
            let chrome = chrome_of(&ctx, "jobs", "Jobs", false);
            let body = render(&ctx, now, &filter, cursor.is_some(), &page);
            html_response(StatusCode::OK, &layout::page(&chrome, &body))
        }
        Err(message) => read_failed(&ctx, "jobs", &message),
    }
}

/// The page body.
#[must_use]
pub fn render(
    ctx: &ProjectCtx,
    now: i64,
    filter: &JobFilter,
    paged: bool,
    page: &RunsState<JobsPage>,
) -> Html {
    let mut body = heading("Jobs", &format!("{}, newest first", ctx.project.name));
    let RunsState::Ready(page) = page else {
        if let Some(text) = missing_runs_notice(page) {
            body.push(&text);
        }
        return body;
    };
    body.push(&filter_form(ctx, filter));
    if let Some(chip) = kind_chip(ctx, filter) {
        body.push(&chip);
    }
    if page.rows.is_empty() {
        body.push(&notice(if paged {
            "No older jobs match."
        } else {
            "No jobs match. Widen the window or clear a filter."
        }));
    } else {
        body.push(&job_table(ctx, now, &page.rows));
    }
    body.push(&pager(ctx, filter, paged, page));
    body
}

fn path(ctx: &ProjectCtx, rest: &str) -> String {
    layout::project_path(&ctx.principal, &ctx.project, rest)
}

fn text_input(name: &str, value: Option<&String>, size: &str) -> Html {
    void(
        "input",
        &[
            ("type", "text"),
            ("name", name),
            ("value", value.map_or("", String::as_str)),
            ("size", size),
            ("placeholder", name),
            ("autocomplete", "off"),
        ],
    )
}

fn filter_form(ctx: &ProjectCtx, filter: &JobFilter) -> Html {
    let outcomes = std::iter::once(option("", "any outcome", filter.outcome.is_none())).chain(
        OUTCOMES
            .iter()
            .map(|word| option(word, word, filter.outcome.as_deref() == Some(word))),
    );
    let windows = [Window::Hour, Window::Day, Window::Week]
        .map(|window| option(window.word(), window.word(), window == filter.window));
    let mut failed = vec![("type", "checkbox"), ("name", "failed"), ("value", "1")];
    if filter.failed {
        failed.push(("checked", ""));
    }
    let action = path(ctx, "jobs");
    // The kind has no field of its own: carry it through a filter submit.
    let kind = filter.kind.as_deref().map_or_else(Html::new, |code| {
        void(
            "input",
            &[("type", "hidden"), ("name", "kind"), ("value", code)],
        )
    });
    let inner = join([
        kind,
        tag("select", &[("name", "outcome")], &join(outcomes)),
        tag(
            "label",
            &[],
            &join([void("input", &failed), Html::text(" failed")]),
        ),
        text_input("pool", filter.pool.as_ref(), "10"),
        text_input("slot", filter.slot.as_ref(), "10"),
        text_input("model", filter.model.as_ref(), "14"),
        text_input("need", filter.need.as_ref(), "10"),
        tag("select", &[("name", "window")], &join(windows)),
        text_tag("button", &[("type", "submit")], "Filter"),
        link_text(&action, "clear"),
    ]);
    tag(
        "form",
        &[("method", "get"), ("action", &action), ("class", "filters")],
        &inner,
    )
}

/// The active failure kind as a chip that links to the list without it.
fn kind_chip(ctx: &ProjectCtx, filter: &JobFilter) -> Option<Html> {
    let code = filter.kind.as_deref()?;
    let label = KINDS
        .iter()
        .find(|(known, _)| *known == code)
        .map_or(code, |(_, label)| *label);
    let without = JobFilter {
        kind: None,
        ..filter.clone()
    };
    let query = filter_query(&without);
    let href = if query.is_empty() {
        path(ctx, "jobs")
    } else {
        path(ctx, &format!("jobs?{query}"))
    };
    Some(chips::removable("Kind", label, &href))
}

fn job_table(ctx: &ProjectCtx, now: i64, rows: &[JobListRow]) -> Html {
    let base = path(ctx, "").trim_end_matches('/').to_string();
    let body = rows
        .iter()
        .map(|row| job_row(ctx, &base, now, row))
        .collect();
    table(
        "table jobs",
        &[
            "Dispatched",
            "Job",
            "Task",
            "Need",
            "Pool / slot",
            "Machine",
            "Model",
            "Result",
            "Wall",
        ],
        body,
    )
}

fn job_row(ctx: &ProjectCtx, base: &str, now: i64, row: &JobListRow) -> Html {
    let run = &row.run;
    let job_href = format!("{base}/jobs/{}", url_encode(&run.job));
    let slot_href = slot_href(ctx, &run.pool, &run.slot);
    let need = run
        .need
        .as_deref()
        .filter(|need| !need.is_empty())
        .map_or_else(Html::new, |need| {
            link(
                &format!("{base}/jobs?need={}", url_encode(&format::short(need))),
                &mono(&format::short(need)),
            )
        });
    row_classed(vec![
        ("nowrap", time_cell(run.dispatched_at, now)),
        ("", link(&job_href, &mono(&format::short(&run.job)))),
        ("", mono(&format::task_handle(&run.task_message_id))),
        ("", need),
        (
            "nowrap",
            link(
                &slot_href,
                &join([span("dim", &format!("{} / ", run.pool)), mono(&run.slot)]),
            ),
        ),
        ("", Html::text(&run.machine)),
        ("", Html::text(&run.model)),
        ("", result_pills(run)),
        ("nowrap num", Html::text(&wall(run, now))),
    ])
}

/// The query string for this filter, without a cursor: `?a=b&c=d`, or empty.
fn filter_query(filter: &JobFilter) -> String {
    let mut pairs: Vec<(&str, String)> = Vec::new();
    if let Some(outcome) = &filter.outcome {
        pairs.push(("outcome", outcome.clone()));
    }
    if filter.failed {
        pairs.push(("failed", "1".into()));
    }
    if let Some(kind) = &filter.kind {
        pairs.push(("kind", kind.clone()));
    }
    for (name, value) in [
        ("pool", &filter.pool),
        ("slot", &filter.slot),
        ("model", &filter.model),
        ("need", &filter.need),
    ] {
        if let Some(value) = value {
            pairs.push((name, value.clone()));
        }
    }
    if filter.window != Window::Day {
        pairs.push(("window", filter.window.word().to_string()));
    }
    pairs
        .iter()
        .map(|(name, value)| format!("{name}={}", url_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn pager(ctx: &ProjectCtx, filter: &JobFilter, paged: bool, page: &JobsPage) -> Html {
    let query = filter_query(filter);
    let mut out = Html::new();
    if paged {
        let href = if query.is_empty() {
            path(ctx, "jobs")
        } else {
            path(ctx, &format!("jobs?{query}"))
        };
        out.push(&link_text(&href, "newest"));
        out.push(&Html::text(" "));
    }
    if let Some(next) = page.next {
        let cursor = format!("before={}&before_id={}", next.dispatched_at, next.id);
        let joined = if query.is_empty() {
            cursor
        } else {
            format!("{query}&{cursor}")
        };
        out.push(&link_text(&path(ctx, &format!("jobs?{joined}")), "older"));
    }
    if out.is_empty() {
        return out;
    }
    tag("p", &[("class", "pager")], &out)
}

#[cfg(test)]
#[path = "jobs_test.rs"]
mod tests;
