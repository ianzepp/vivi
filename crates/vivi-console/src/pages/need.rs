//! `GET /ui/p/{project}/needs/{need}`: one need, in two tabs.
//!
//! The attempts tab (`?tab=attempts`, the default) shows what the need asks,
//! its thread and the runs of its work. The flow tab (`?tab=flow`) is in
//! `need_flow`.

use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;

use super::need_flow;
use super::widgets::{job_link, result_pills, time_cell};
use super::{blocking, chrome_of, html_response, not_found, now_epoch, read_failed};
use crate::auth::ProjectCtx;
use crate::data::need_page::{self, NeedLetter, NeedPage, Reply, Tab, Thread};
use crate::data::needs::AttemptRow;
use crate::format;
use crate::html::{Html, div, join, tag, text_tag, url_encode};
use crate::layout::{self, mono, notice, pill, row_classed, table};

/// Characters of a reply shown before it is cut.
const REPLY_CLIP: usize = 4000;

/// The query string.
#[derive(Debug, Default, Deserialize)]
pub struct NeedQuery {
    pub tab: Option<String>,
    /// The flow run to show on the flow tab; the newest when missing.
    pub run: Option<String>,
    /// `1` to show the whole flow's source on the flow tab.
    pub source: Option<String>,
}

impl NeedQuery {
    fn tab(&self) -> Tab {
        Tab::parse(self.tab.as_deref().unwrap_or(""))
    }

    pub(super) fn run(&self) -> Option<i64> {
        match self.run.as_deref()?.trim().parse() {
            Ok(run) => Some(run),
            Err(_) => None,
        }
    }

    pub(super) fn wants_source(&self) -> bool {
        self.source.as_deref() == Some("1")
    }
}

/// The handler. `Path` carries `(project, need)`.
pub async fn handler(
    ctx: ProjectCtx,
    Path((_, need)): Path<(String, String)>,
    Query(query): Query<NeedQuery>,
) -> Response {
    let root = ctx.project.mailspace_root.clone();
    let (tab, run) = (query.tab(), query.run());
    let handle = need.clone();
    match blocking(move || need_page::load(&root, &handle, tab, run)).await {
        Ok(Some(page)) if page.exists() => {
            let title = format!("Need {need}");
            let chrome = chrome_of(&ctx, "needs", &title, tab == Tab::Attempts);
            let body = render(&ctx, &page, &query, now_epoch());
            html_response(StatusCode::OK, &layout::page(&chrome, &body))
        }
        Ok(_) => not_found(&ctx.principal),
        Err(message) => read_failed(&ctx, "needs", &message),
    }
}

/// `/ui/p/<project>/needs/<need>` with an optional query string.
pub(super) fn need_href(ctx: &ProjectCtx, handle: &str, query: &str) -> String {
    let base = layout::project_path(
        &ctx.principal,
        &ctx.project,
        &format!("needs/{}", url_encode(handle)),
    );
    if query.is_empty() {
        base
    } else {
        format!("{base}?{query}")
    }
}

/// The page body.
#[must_use]
pub fn render(ctx: &ProjectCtx, page: &NeedPage, query: &NeedQuery, now: i64) -> Html {
    let mut body = text_tag("h1", &[], &format!("Need {}", page.handle));
    body.push(&summary(page));
    body.push(&tabs(ctx, page, query.tab()));
    match query.tab() {
        Tab::Attempts => body.push(&attempts_tab(ctx, page, now)),
        Tab::Flow => body.push(&need_flow::render(ctx, page, query, now)),
    }
    body
}

fn summary(page: &NeedPage) -> Html {
    let mut line = Html::new();
    match &page.node {
        Some(node) => {
            line.push(&Html::text(&format!("{} ", node.subject)));
            line.push(&match node.state.as_str() {
                "done" => pill("ok", "done"),
                "open" | "active" => pill("busy", "open"),
                other => pill("muted", other),
            });
        }
        None => line.push(&Html::text(
            "Not in the backlog graph; shown from its recorded runs.",
        )),
    }
    if let Some(attempts) = &page.attempts {
        line.push(&Html::text(&format!(" {} attempt(s)", attempts.count)));
    }
    tag("p", &[("class", "sub")], &line)
}

fn tabs(ctx: &ProjectCtx, page: &NeedPage, current: Tab) -> Html {
    let links = [Tab::Attempts, Tab::Flow].map(|tab| {
        let class = if tab == current { "active" } else { "" };
        let href = need_href(ctx, &page.handle, &format!("tab={}", tab.word()));
        let label = match tab {
            Tab::Attempts => "Attempts",
            Tab::Flow => "Flow",
        };
        tag(
            "a",
            &[("href", &href), ("class", class)],
            &Html::text(label),
        )
    });
    tag("nav", &[("class", "tabs")], &join(links))
}

fn attempts_tab(ctx: &ProjectCtx, page: &NeedPage, now: i64) -> Html {
    let mut out = Html::new();
    match &page.thread {
        Some(thread) => out.push(&need_and_thread(page, thread)),
        None => out.push(&notice("The need letter was not read.")),
    }
    out.push(&text_tag("h2", &[], "Attempts"));
    out.push(&timeline(ctx, page, now));
    out
}

fn need_and_thread(page: &NeedPage, thread: &Thread) -> Html {
    let done = page.node.as_ref().is_some_and(|node| node.state == "done");
    let mut out = Html::new();
    match &thread.letter {
        Some(letter) => out.push(&letter_section(letter, done)),
        None => out.push(&notice(
            "No stored copy of this need's letter was found, so its done_when and thread are not shown.",
        )),
    }
    out.push(&text_tag("h2", &[], "Thread"));
    if thread.replies.is_empty() {
        out.push(&notice("No replies yet."));
    }
    for reply in &thread.replies {
        out.push(&reply_block(reply));
    }
    if thread.records > 0 {
        out.push(&text_tag(
            "p",
            &[("class", "sub")],
            &format!(
                "{} lifecycle record(s) (done, marked, ...) not shown.",
                thread.records
            ),
        ));
    }
    out
}

fn letter_section(letter: &NeedLetter, done: bool) -> Html {
    let mut out = text_tag("h2", &[], "done when");
    let facts = &letter.text.facts;
    if facts.done_when.is_empty() {
        out.push(&notice("The need letter lists no done_when."));
    } else {
        let mark = if done { "[x] " } else { "[ ] " };
        let items = facts
            .done_when
            .iter()
            .map(|item| tag("li", &[], &join([mono(mark), Html::text(item)])));
        out.push(&tag("ul", &[("class", "checks")], &join(items)));
        out.push(&text_tag(
            "p",
            &[("class", "sub")],
            "Whether an item is met is not recorded; the boxes show only whether the need is done.",
        ));
    }
    if let Some(error) = &letter.text.front_error {
        out.push(&div(
            "notice notice-error",
            &Html::text(&format!("The front matter could not be read: {error}")),
        ));
    }
    if let Some(target) = &facts.target {
        out.push(&text_tag(
            "p",
            &[("class", "sub")],
            &format!("target: {target}"),
        ));
    }
    out.push(&text_tag(
        "p",
        &[("class", "sub")],
        &format!("filed by {} on {}", letter.from, letter.date),
    ));
    out.push(&text_tag("pre", &[("class", "letter")], &letter.text.prose));
    out
}

fn reply_block(reply: &Reply) -> Html {
    let head = format!("{} - {} - {}", reply.date, reply.from, reply.subject);
    let mut text: String = reply.body.chars().take(REPLY_CLIP).collect();
    if reply.body.chars().count() > REPLY_CLIP {
        text.push_str("\n... (cut)");
    }
    div(
        "reply",
        &join([
            text_tag("p", &[("class", "sub")], &head),
            text_tag("pre", &[("class", "letter")], &text),
        ]),
    )
}

fn timeline(ctx: &ProjectCtx, page: &NeedPage, now: i64) -> Html {
    let Some(attempts) = &page.attempts else {
        return notice(
            "This store has no slot index (the slot_runs table), so attempts are not recorded.",
        );
    };
    if attempts.newest.is_empty() {
        return notice("No attempts are recorded for this need.");
    }
    let base = layout::project_path(&ctx.principal, &ctx.project, "")
        .trim_end_matches('/')
        .to_string();
    let rows = attempts
        .newest
        .iter()
        .enumerate()
        .map(|(index, row)| attempt_row(&base, now, attempts.count, index, row))
        .collect();
    let mut out = table(
        "table attempts",
        &["#", "Dispatched", "Job", "Model", "Result", "Why", "Wall"],
        rows,
    );
    if attempts.count > i64::try_from(attempts.newest.len()).unwrap_or(i64::MAX) {
        out.push(&text_tag(
            "p",
            &[("class", "sub")],
            &format!(
                "Showing the newest {} of {} attempts.",
                attempts.newest.len(),
                attempts.count
            ),
        ));
    }
    out
}

fn attempt_row(base: &str, now: i64, total: i64, index: usize, row: &AttemptRow) -> Html {
    let number = total - i64::try_from(index).unwrap_or(0);
    let run = &row.run;
    let wall = match (run.job_s, run.collected_at) {
        (Some(seconds), _) => format::duration(seconds),
        (None, Some(done)) => format::duration(done - run.dispatched_at),
        (None, None) => format!("{} (running)", format::duration(now - run.dispatched_at)),
    };
    row_classed(vec![
        ("num", Html::text(&number.to_string())),
        ("nowrap", time_cell(run.dispatched_at, now)),
        ("", job_link(base, run)),
        ("", Html::text(&run.model)),
        ("", result_pills(run)),
        ("", Html::text(row.landing_reason.as_deref().unwrap_or(""))),
        ("nowrap num", Html::text(&wall)),
    ])
}

#[cfg(test)]
#[path = "need_test.rs"]
mod tests;
