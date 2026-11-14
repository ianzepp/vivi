//! `GET /ui/p/{project}/slots/{pool}/{slot}`: one slot's runs, newest first.

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::Response;

use super::widgets::{job_link, missing_runs_notice, result_pills, task_cell, time_cell};
use super::{blocking, chrome_of, html_response, now_epoch, read_failed};
use crate::auth::ProjectCtx;
use crate::data::RunsState;
use crate::data::runs::RunRow;
use crate::data::slots::{self, HISTORY_ROWS};
use crate::format;
use crate::html::{Html, join, link_text, text_tag};
use crate::layout::{self, heading, mono, notice, row_classed, table};

/// The handler. `Path` carries `(project, pool, slot)`.
pub async fn handler(
    ctx: ProjectCtx,
    Path((_, pool, slot)): Path<(String, String, String)>,
) -> Response {
    let root = ctx.project.mailspace_root.clone();
    let now = now_epoch();
    let (read_pool, read_slot) = (pool.clone(), slot.clone());
    match blocking(move || slots::history(&root, &read_pool, &read_slot)).await {
        Ok(runs) => {
            let title = format!("{pool} / {slot}");
            let chrome = chrome_of(&ctx, "slots", &title, false);
            let body = render(&ctx, &pool, &slot, now, &runs);
            html_response(StatusCode::OK, &layout::page(&chrome, &body))
        }
        Err(message) => read_failed(&ctx, "slots", &message),
    }
}

/// The page body.
#[must_use]
pub fn render(
    ctx: &ProjectCtx,
    pool: &str,
    slot: &str,
    now: i64,
    runs: &RunsState<Vec<RunRow>>,
) -> Html {
    let mut body = heading(&format!("{pool} / {slot}"), "Slot history, newest first");
    body.push(&link_text(
        &layout::project_path(&ctx.principal, &ctx.project, "slots"),
        "all slots",
    ));
    let RunsState::Ready(runs) = runs else {
        if let Some(text) = missing_runs_notice(runs) {
            body.push(&text);
        }
        return body;
    };
    if runs.is_empty() {
        body.push(&notice("No runs are recorded for this slot."));
        return body;
    }
    body.push(&history_table(ctx, now, runs));
    if runs.len() as i64 >= HISTORY_ROWS {
        body.push(&text_tag(
            "p",
            &[("class", "sub")],
            &format!("Showing the newest {HISTORY_ROWS} runs."),
        ));
    }
    body
}

fn history_table(ctx: &ProjectCtx, now: i64, runs: &[RunRow]) -> Html {
    let base = layout::project_path(&ctx.principal, &ctx.project, "")
        .trim_end_matches('/')
        .to_string();
    let rows = runs
        .iter()
        .map(|run| history_row(&base, now, run))
        .collect();
    table(
        "table history",
        &[
            "Dispatched",
            "Job",
            "Task / need",
            "Model",
            "Result",
            "Agent / check",
            "Wall",
        ],
        rows,
    )
}

fn codes(run: &RunRow) -> String {
    let code = |value: Option<i64>| value.map_or_else(|| String::from("-"), |v| v.to_string());
    format!("{} / {}", code(run.agent_rc), code(run.check_rc))
}

fn wall(run: &RunRow, now: i64) -> String {
    match (run.job_s, run.collected_at) {
        (Some(seconds), _) => format::duration(seconds),
        (None, Some(done)) => format::duration(done - run.dispatched_at),
        (None, None) => format!("{} (running)", format::duration(now - run.dispatched_at)),
    }
}

fn history_row(base: &str, now: i64, run: &RunRow) -> Html {
    row_classed(vec![
        ("nowrap", time_cell(run.dispatched_at, now)),
        ("", job_link(base, run)),
        ("", task_cell(run)),
        ("", Html::text(&run.model)),
        ("", result_pills(run)),
        ("nowrap", join([mono(&codes(run))])),
        ("nowrap num", Html::text(&wall(run, now))),
    ])
}
