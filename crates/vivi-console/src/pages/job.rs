//! `GET /ui/p/{project}/jobs/{job}`: one dispatch end to end.

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::Response;

use super::job_sections;
use super::widgets::{landing_pill, missing_runs_notice, slot_href, time_cell};
use super::{blocking, chrome_of, html_response, now_epoch, read_failed};
use crate::auth::ProjectCtx;
use crate::data::RunsState;
use crate::data::job::{self, JobDetail, JobRun};
use crate::format;
use crate::html::{Html, div, join, link, link_text, tag, text_tag, url_encode};
use crate::layout::{self, heading, mono, notice, pill};

/// The handler. `Path` carries `(project, job)`.
pub async fn handler(ctx: ProjectCtx, Path((_, job_name)): Path<(String, String)>) -> Response {
    let root = ctx.project.mailspace_root.clone();
    let now = now_epoch();
    let wanted = job_name.clone();
    match blocking(move || job::load(&root, &wanted)).await {
        Ok(state) => {
            let (status, body) = render(&ctx, now, &job_name, &state);
            let title = format!("Job {}", format::short(&job_name));
            let chrome = chrome_of(&ctx, "jobs", &title, false);
            html_response(status, &layout::page(&chrome, &body))
        }
        Err(message) => read_failed(&ctx, "jobs", &message),
    }
}

/// The page body and its status: 404 for a job the index does not know.
#[must_use]
pub fn render(
    ctx: &ProjectCtx,
    now: i64,
    job_name: &str,
    state: &RunsState<Option<JobDetail>>,
) -> (StatusCode, Html) {
    let mut body = heading(&format!("Job {job_name}"), "");
    body.push(&back_link(ctx));
    let detail = match state {
        RunsState::Ready(Some(detail)) => detail,
        RunsState::Ready(None) => {
            body.push(&notice(
                "No job with this name is recorded in this project.",
            ));
            return (StatusCode::NOT_FOUND, body);
        }
        other => {
            if let Some(text) = missing_runs_notice(other) {
                body.push(&text);
            }
            return (StatusCode::OK, body);
        }
    };
    body.push(&header(ctx, now, &detail.run));
    body.push(&facts(ctx, now, &detail.run));
    body.push(&job_sections::sections(now, detail));
    (StatusCode::OK, body)
}

fn back_link(ctx: &ProjectCtx) -> Html {
    link_text(
        &layout::project_path(&ctx.principal, &ctx.project, "jobs"),
        "all jobs",
    )
}

/// The task and need handles, and the result pills.
fn header(ctx: &ProjectCtx, now: i64, run: &JobRun) -> Html {
    let mut line = join([
        Html::text("task "),
        mono(&format::task_handle(&run.task_message_id)),
    ]);
    if let Some(need) = run.need.as_deref().filter(|need| !need.is_empty()) {
        let href = layout::project_path(
            &ctx.principal,
            &ctx.project,
            &format!("jobs?need={}", url_encode(&format::short(need))),
        );
        line.push(&Html::text("   need "));
        line.push(&link(&href, &mono(&format::short(need))));
    }
    line.push(&Html::text("   dispatched "));
    line.push(&time_cell(run.dispatched_at, now));
    join([tag("p", &[("class", "sub")], &line), pills(run)])
}

fn code_pill(label: &str, code: Option<i64>) -> Option<Html> {
    let code = code?;
    let kind = if code == 0 { "ok" } else { "failed" };
    Some(pill(kind, &format!("{label} {code}")))
}

fn pills(run: &JobRun) -> Html {
    let mut out = Html::new();
    match run.outcome.as_deref().filter(|word| !word.is_empty()) {
        None => out.push(&pill("busy", "running")),
        Some("finished") => out.push(&pill("ok", "finished")),
        Some(word) => out.push(&pill("failed", word)),
    }
    for code in [
        code_pill("agent", run.agent_rc),
        code_pill("check", run.check_rc),
        landing_pill(run.landing.as_deref()),
    ]
    .into_iter()
    .flatten()
    {
        out.push(&code);
    }
    div("pills", &out)
}

fn seconds(label: &str, value: Option<i64>) -> Option<(String, Html)> {
    value.map(|secs| (label.to_string(), Html::text(&format::duration(secs))))
}

fn fact(label: &str, value: Html) -> (String, Html) {
    (label.to_string(), value)
}

/// The facts grid: where it ran, what it started from, and the clocks.
fn facts(ctx: &ProjectCtx, now: i64, run: &JobRun) -> Html {
    let slot_href = slot_href(ctx, &run.pool, &run.slot);
    let mut items = vec![
        fact(
            "pool / slot",
            link(&slot_href, &mono(&format!("{} / {}", run.pool, run.slot))),
        ),
        fact("machine", Html::text(&run.machine)),
        fact("model", Html::text(&run.model)),
    ];
    items.extend(placed(run));
    items.extend(clocks(now, run));
    grid(&items)
}

/// What the job started from and what it pushed.
fn placed(run: &JobRun) -> Vec<(String, Html)> {
    let mut items = Vec::new();
    if let Some(kind) = run.kind.as_deref().filter(|kind| !kind.is_empty()) {
        items.push(fact("kind", Html::text(kind)));
    }
    if let Some(target) = run.target.as_deref().filter(|target| !target.is_empty()) {
        items.push(fact("target", Html::text(target)));
    }
    let branch = if run.read_only {
        String::from("read-only")
    } else {
        run.branch.clone()
    };
    items.push(fact("branch", mono(&branch)));
    if !run.start_sha.is_empty() {
        items.push(fact("start", mono(&format::short_sha(&run.start_sha))));
    }
    if let Some(push) = run.push.as_deref().filter(|push| !push.is_empty()) {
        let sha = run
            .sha
            .as_deref()
            .map(format::short_sha)
            .unwrap_or_default();
        items.push(fact(
            "push",
            Html::text(&format!("{push} {sha}").trim_end().to_string()),
        ));
    }
    items
}

/// Wall and CPU seconds, the budget, and the landing.
fn clocks(now: i64, run: &JobRun) -> Vec<(String, Html)> {
    let mut items: Vec<(String, Html)> = Vec::new();
    items.extend(
        run.budget_s
            .map(|budget| fact("budget", Html::text(&format::duration(budget)))),
    );
    for item in [
        seconds("agent wall", run.agent_s),
        seconds("check wall", run.check_s),
        seconds("job wall", run.job_s),
        seconds("agent cpu", run.agent_cpu_s),
        seconds("check cpu", run.check_cpu_s),
    ]
    .into_iter()
    .flatten()
    {
        items.push(item);
    }
    if let Some(done) = run.collected_at {
        items.push(fact("collected", time_cell(done, now)));
    }
    if let Some(landing) = run.landing.as_deref().filter(|word| !word.is_empty()) {
        let mut cell = Html::text(landing);
        if let Some(at) = run.landing_at {
            cell.push(&Html::text(" "));
            cell.push(&time_cell(at, now));
        }
        items.push(fact("landing", cell));
    }
    items
}

fn grid(items: &[(String, Html)]) -> Html {
    let cells = items.iter().map(|(label, value)| {
        div(
            "fact",
            &join([text_tag("dt", &[], label), tag("dd", &[], value)]),
        )
    });
    tag("dl", &[("class", "facts")], &join(cells))
}

#[cfg(test)]
#[path = "job_test.rs"]
mod tests;
