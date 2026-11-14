//! `GET /ui/p/{project}/needs`: the project's needs, each with its attempts
//! and, when a pump is running, its place in the flow. Filters
//! (`?state=&kind=&step=&stuck=`) live in the query string.

use std::collections::BTreeMap;

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;

use super::widgets::{result_pills, time_cell};
use super::{blocking, chrome_of, html_response, now_epoch, read_failed};
use crate::auth::ProjectCtx;
use crate::data::need_flow::PumpState;
use crate::data::needs::{self, LIST_LIMIT, NeedRow, NeedsData, StateFilter};
use crate::format;
use crate::html::{Html, join, link_text, span, tag, text_tag, url_encode, void};
use crate::layout::{self, heading, mono, notice, pill, row_classed, table};

/// The query string.
#[derive(Debug, Default, Deserialize)]
pub struct NeedsQuery {
    pub state: Option<String>,
    pub kind: Option<String>,
    pub step: Option<String>,
    /// Hours the current step has run for, at least.
    pub stuck: Option<String>,
}

impl NeedsQuery {
    fn filter(&self) -> StateFilter {
        StateFilter::parse(self.state.as_deref().unwrap_or(""))
    }

    fn kind(&self) -> &str {
        self.kind.as_deref().unwrap_or("")
    }

    fn step(&self) -> &str {
        self.step.as_deref().unwrap_or("")
    }

    fn stuck_text(&self) -> &str {
        self.stuck.as_deref().unwrap_or("").trim()
    }

    /// The stuck threshold in seconds; a missing or unreadable value is none.
    fn stuck_secs(&self) -> Option<i64> {
        let Ok(hours) = self.stuck_text().parse::<f64>() else {
            return None;
        };
        (hours.is_finite() && hours >= 0.0).then(|| (hours * 3600.0) as i64)
    }

    fn keeps(&self, row: &NeedRow, now: i64) -> bool {
        if !self.kind().is_empty() && row.kind() != Some(self.kind()) {
            return false;
        }
        match self.stuck_secs() {
            Some(secs) => row
                .flow
                .as_ref()
                .and_then(|flow| flow.step_since)
                .is_some_and(|since| now - since >= secs),
            None => true,
        }
    }
}

/// The handler.
pub async fn handler(ctx: ProjectCtx, Query(query): Query<NeedsQuery>) -> Response {
    let root = ctx.project.mailspace_root.clone();
    let now = now_epoch();
    let filter = query.filter();
    match blocking(move || needs::load(&root, now, filter)).await {
        Ok(Some(data)) => {
            let chrome = chrome_of(&ctx, "needs", "Needs", true);
            let body = render(&ctx, &data, &query);
            html_response(StatusCode::OK, &layout::page(&chrome, &body))
        }
        Ok(None) => {
            let chrome = chrome_of(&ctx, "needs", "Needs", false);
            let body = join([
                heading("Needs", &ctx.project.name),
                notice("This project has no mailspace store yet, so there are no needs to show."),
            ]);
            html_response(StatusCode::OK, &layout::page(&chrome, &body))
        }
        Err(message) => read_failed(&ctx, "needs", &message),
    }
}

/// The page body.
#[must_use]
pub fn render(ctx: &ProjectCtx, data: &NeedsData, query: &NeedsQuery) -> Html {
    let mut body = heading("Needs", &ctx.project.name);
    body.push(&filters(ctx, data, query));
    if let Some(text) = pump_notice(&data.pump) {
        body.push(&text);
    }
    let kept: Vec<&NeedRow> = data
        .rows
        .iter()
        .filter(|row| query.keeps(row, data.now))
        .collect();
    body.push(&step_counts(ctx, &kept, query));
    let shown: Vec<&NeedRow> = kept
        .into_iter()
        .filter(|row| query.step().is_empty() || row.step() == Some(query.step()))
        .collect();
    if shown.is_empty() {
        body.push(&notice("No needs match."));
    } else {
        body.push(&need_table(ctx, data.now, &shown));
    }
    if data.truncated {
        body.push(&text_tag(
            "p",
            &[("class", "sub")],
            &format!("Showing the newest {LIST_LIMIT} needs; older ones are not read."),
        ));
    }
    body
}

fn pump_notice(pump: &PumpState<()>) -> Option<Html> {
    let text = match pump {
        PumpState::Ready(()) => return None,
        PumpState::NoPump => String::from(
            "This project has no pump, so flow positions are not shown: needs list their attempts only.",
        ),
        PumpState::NotStarted => String::from(
            "The pump is configured but has not written its store yet, so flow positions are not shown.",
        ),
        PumpState::Unreadable(why) => format!(
            "The pump store cannot be read right now ({why}), so flow positions are not shown."
        ),
    };
    Some(notice(&text))
}

fn option(value: &str, label: &str, selected: bool) -> Html {
    let mut attrs = vec![("value", value)];
    if selected {
        attrs.push(("selected", ""));
    }
    text_tag("option", &attrs, label)
}

fn select(name: &str, all: &str, values: &[String], current: &str) -> Html {
    let mut options = vec![option("", all, current.is_empty())];
    for value in values {
        options.push(option(value, value, value == current));
    }
    tag("select", &[("name", name)], &join(options))
}

fn filters(ctx: &ProjectCtx, data: &NeedsData, query: &NeedsQuery) -> Html {
    let mut kinds: Vec<String> = data
        .rows
        .iter()
        .filter_map(|row| row.kind().map(str::to_string))
        .collect();
    kinds.sort();
    kinds.dedup();
    let mut steps: Vec<String> = data
        .rows
        .iter()
        .filter_map(|row| row.step().map(str::to_string))
        .collect();
    steps.sort();
    steps.dedup();
    let action = layout::project_path(&ctx.principal, &ctx.project, "needs");
    let states: Vec<String> = ["open", "done", "any"].map(String::from).to_vec();
    let state = join(
        states
            .iter()
            .map(|s| option(s, s, s == query.filter().word())),
    );
    let inner = join([
        Html::text("state "),
        tag("select", &[("name", "state")], &state),
        Html::text(" kind "),
        select("kind", "any", &kinds, query.kind()),
        Html::text(" at step "),
        select("step", "any", &steps, query.step()),
        Html::text(" stuck over (hours) "),
        void(
            "input",
            &[
                ("type", "number"),
                ("name", "stuck"),
                ("min", "0"),
                ("step", "any"),
                ("value", query.stuck_text()),
                ("size", "4"),
            ],
        ),
        text_tag("button", &[("type", "submit")], "Filter"),
        link_text(&action, "clear"),
    ]);
    tag(
        "form",
        &[("method", "get"), ("action", &action), ("class", "filters")],
        &inner,
    )
}

fn list_href(ctx: &ProjectCtx, query: &NeedsQuery, step: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    if query.filter() != StateFilter::Open {
        parts.push(format!("state={}", query.filter().word()));
    }
    if !query.kind().is_empty() {
        parts.push(format!("kind={}", url_encode(query.kind())));
    }
    if !step.is_empty() {
        parts.push(format!("step={}", url_encode(step)));
    }
    if !query.stuck_text().is_empty() {
        parts.push(format!("stuck={}", url_encode(query.stuck_text())));
    }
    let base = layout::project_path(&ctx.principal, &ctx.project, "needs");
    if parts.is_empty() {
        base
    } else {
        format!("{base}?{}", parts.join("&"))
    }
}

/// The count per current step across the needs the other filters keep.
fn step_counts(ctx: &ProjectCtx, rows: &[&NeedRow], query: &NeedsQuery) -> Html {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for row in rows {
        *counts.entry(row.step().unwrap_or("")).or_default() += 1;
    }
    let mut items: Vec<Html> = Vec::new();
    for (step, count) in counts {
        let label = if step.is_empty() { "no step" } else { step };
        let text = format!("{label} {count}");
        let item = if step.is_empty() || step == query.step() {
            span("dim", &text)
        } else {
            link_text(&list_href(ctx, query, step), &text)
        };
        if !items.is_empty() {
            items.push(span("dim", " | "));
        }
        items.push(item);
    }
    let mut out = span("dim", "by step: ");
    out.push(&join(items));
    tag("p", &[("class", "steps")], &out)
}

fn need_table(ctx: &ProjectCtx, now: i64, rows: &[&NeedRow]) -> Html {
    let base = layout::project_path(&ctx.principal, &ctx.project, "needs");
    let rows = rows.iter().map(|row| need_row(&base, now, row)).collect();
    table(
        "table needs",
        &[
            "Need", "Subject", "State", "Run", "At step", "Attempts", "Age",
        ],
        rows,
    )
}

fn need_row(base: &str, now: i64, row: &NeedRow) -> Html {
    let href = format!("{base}/{}", url_encode(&row.node.handle));
    let age = row
        .node
        .created_at
        .map_or_else(Html::new, |at| Html::text(&format::duration(now - at)));
    row_classed(vec![
        (
            "nowrap",
            tag("a", &[("href", &href)], &mono(&row.node.handle)),
        ),
        ("", Html::text(&row.node.subject)),
        ("", state_pill(&row.node.state)),
        ("num", run_cell(row)),
        ("nowrap", step_cell(now, row)),
        ("", attempts_cell(now, row)),
        ("nowrap num", age),
    ])
}

fn state_pill(state: &str) -> Html {
    match state {
        "done" => pill("ok", "done"),
        "open" | "active" => pill("busy", "open"),
        other => pill("muted", other),
    }
}

fn run_cell(row: &NeedRow) -> Html {
    row.flow
        .as_ref()
        .map_or_else(Html::new, |flow| Html::text(&flow.run.run.to_string()))
}

fn step_cell(now: i64, row: &NeedRow) -> Html {
    let Some(flow) = &row.flow else {
        return Html::new();
    };
    let mut out = Html::new();
    if let Some(step) = &flow.step {
        out.push(&mono(&step.node));
        out.push(&span("dim", &format!(" {}", step.state)));
    }
    if flow.run.state != "live" {
        out.push(&pill("muted", &flow.run.state));
    }
    if let Some(since) = flow.step_since {
        out.push(&span(
            "dim",
            &format!(" for {}", format::duration(now - since)),
        ));
    }
    out
}

fn attempts_cell(now: i64, row: &NeedRow) -> Html {
    let Some(attempts) = &row.attempts else {
        return Html::new();
    };
    let mut out = Html::text(&attempts.count.to_string());
    if let Some(last) = attempts.newest.first() {
        out.push(&Html::text(" "));
        out.push(&result_pills(&last.run));
        out.push(&Html::text(" "));
        out.push(&time_cell(last.run.dispatched_at, now));
    }
    out
}

#[cfg(test)]
#[path = "needs_harness_test.rs"]
pub(crate) mod harness;
#[cfg(test)]
#[path = "needs_test.rs"]
mod tests;
