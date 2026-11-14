//! `GET /ui/p/{project}/overview`: pump, slots, and the last 24 hours.

use axum::http::StatusCode;
use axum::response::Response;

use super::widgets::{job_link, missing_runs_notice, task_cell, time_cell};
use super::{blocking, chrome_of, html_response, now_epoch, read_failed};
use crate::auth::ProjectCtx;
use crate::data::RunsState;
use crate::data::overview::{self, DayRuns, HourBar, Overview, PoolSlots, PumpView};
use crate::data::runs::RunRow;
use crate::format;
use crate::html::{Html, div, join, span, tag, text_tag, url_encode};
use crate::layout::{self, heading, mono, pill, row_classed, table};

/// The handler.
pub async fn handler(ctx: ProjectCtx) -> Response {
    let root = ctx.project.mailspace_root.clone();
    let now = now_epoch();
    match blocking(move || overview::load(&root, now)).await {
        Ok(data) => {
            let chrome = chrome_of(&ctx, "overview", "Overview", true);
            let body = render(&ctx, &data);
            html_response(StatusCode::OK, &layout::page(&chrome, &body))
        }
        Err(message) => read_failed(&ctx, "overview", &message),
    }
}

/// The page body.
#[must_use]
pub fn render(ctx: &ProjectCtx, data: &Overview) -> Html {
    let mut body = heading("Overview", &ctx.project.name);
    body.push(&cards(data));
    body.push(&text_tag("h2", &[], "Slots by pool"));
    body.push(&pools_table(ctx, &data.pools));
    match &data.runs {
        RunsState::Ready(day) => body.push(&last_day(ctx, data.now, day)),
        other => {
            if let Some(notice) = missing_runs_notice(other) {
                body.push(&notice);
            }
        }
    }
    body
}

fn cards(data: &Overview) -> Html {
    let mut row = Html::new();
    row.push(&card("Pump", &pump_card(&data.pump)));
    row.push(&card("Slots", &slots_card(&data.pools)));
    if let RunsState::Ready(day) = &data.runs {
        row.push(&card("Dispatched, 24 h", &big(&day.dispatched.to_string())));
        row.push(&card("Collected, 24 h", &collected_card(day)));
    }
    div("cards", &row)
}

pub fn card(title: &str, inner: &Html) -> Html {
    div("card", &join([text_tag("h3", &[], title), inner.clone()]))
}

fn big(text: &str) -> Html {
    div("big", &Html::text(text))
}

fn small(text: &str) -> Html {
    div("small", &Html::text(text))
}

pub fn pump_card(pump: &PumpView) -> Html {
    match pump {
        PumpView::NoPump => join([
            big("no pump"),
            small("No [pump] table in this project's config."),
        ]),
        PumpView::Unreadable(message) => {
            join([div("big", &pill("failed", "unreadable")), small(message)])
        }
        PumpView::Health(view) => {
            let kind = match view.verdict {
                "ok" => "ok",
                "degraded" => "conflict",
                "stalled" => "failed",
                _ => "muted",
            };
            let mut parts = vec![format!("writer {}", view.writer)];
            if let Some(control) = &view.control {
                parts.push(format!("control {control}"));
            }
            if let Some(age) = view.last_tick_age_secs {
                let age = i64::try_from(age).unwrap_or(i64::MAX);
                parts.push(format!("tick {}", format::ago(age)));
            }
            let mut out = join([
                div("big", &pill(kind, view.verdict)),
                small(&parts.join(", ")),
            ]);
            for reason in view.reasons.iter().take(2) {
                out.push(&small(reason));
            }
            out
        }
    }
}

fn slots_card(pools: &[PoolSlots]) -> Html {
    let declared: u32 = pools.iter().map(|pool| pool.declared).sum();
    let busy: Option<u32> = pools
        .iter()
        .map(|pool| pool.busy)
        .try_fold(0_u32, |total, busy| busy.map(|busy| total + busy));
    match busy {
        Some(busy) => join([
            big(&format!("{busy} busy / {declared}")),
            small(&format!("{} free", declared.saturating_sub(busy))),
        ]),
        None => join([
            big(&format!("{declared} declared")),
            small("busy state needs the slot index"),
        ]),
    }
}

fn collected_card(day: &DayRuns) -> Html {
    let mut out = div(
        "big",
        &join([
            span("ok-num", &day.collected_ok().to_string()),
            Html::text(" ok · "),
            span("fail-num", &day.collected_failed().to_string()),
            Html::text(" failed"),
        ]),
    );
    let mut failed: Vec<String> = day
        .outcomes
        .iter()
        .filter(|row| row.failed)
        .map(|row| format!("{} {}", row.count, outcome_name(&row.outcome)))
        .collect();
    failed.sort();
    if !failed.is_empty() {
        out.push(&small(&failed.join(", ")));
    }
    out
}

fn outcome_name(outcome: &str) -> &str {
    if outcome.is_empty() {
        "no outcome"
    } else {
        outcome
    }
}

fn pools_table(ctx: &ProjectCtx, pools: &[PoolSlots]) -> Html {
    if pools.is_empty() {
        return layout::notice("This project declares no pools.");
    }
    let rows = pools
        .iter()
        .map(|pool| {
            let busy = pool
                .busy
                .map_or_else(|| String::from("-"), |n| n.to_string());
            let free = pool
                .free()
                .map_or_else(|| String::from("-"), |n| n.to_string());
            row_classed(vec![
                ("", mono(&pool.pool)),
                ("num", Html::text(&pool.declared.to_string())),
                ("num", Html::text(&busy)),
                ("num", Html::text(&free)),
                ("", named_tiles(ctx, pool)),
            ])
        })
        .collect();
    table(
        "table pools",
        &["Pool", "Declared", "Busy", "Free", "Slots"],
        rows,
    )
}

/// A tile per named slot of the pool, busy ones lit, each linking to the
/// slot's history. A pool that only counts its slots has none.
fn named_tiles(ctx: &ProjectCtx, pool: &PoolSlots) -> Html {
    let tiles = pool.named.iter().map(|(name, busy)| {
        let class = if *busy == Some(true) {
            "tile tile-busy"
        } else {
            "tile tile-free"
        };
        let rest = format!("slots/{}/{}", url_encode(&pool.pool), url_encode(name));
        let href = layout::project_path(&ctx.principal, &ctx.project, &rest);
        tag("a", &[("class", class), ("href", &href)], &Html::text(name))
    });
    div("tiles", &join(tiles))
}

fn last_day(ctx: &ProjectCtx, now: i64, day: &DayRuns) -> Html {
    let mut out = text_tag("h2", &[], "Per hour, last 24 h");
    out.push(&chart(&day.hours));
    out.push(&text_tag("h2", &[], "Newest failures"));
    out.push(&failures(ctx, now, &day.failures));
    out
}

/// Stacked bars per hour: ok (green) under failed (red). Plain SVG.
fn chart(hours: &[HourBar]) -> Html {
    const STEP: i64 = 20;
    const HEIGHT: f64 = 76.0;
    let max = hours
        .iter()
        .map(|bar| bar.ok + bar.failed)
        .max()
        .unwrap_or(0)
        .max(1);
    let mut shapes = Html::new();
    for (index, bar) in hours.iter().enumerate() {
        let x = index as i64 * STEP;
        let ok_h = bar.ok as f64 / max as f64 * HEIGHT;
        let fail_h = bar.failed as f64 / max as f64 * HEIGHT;
        let title = format!(
            "{} UTC: {} ok, {} failed",
            format::iso(bar.hour * 3600),
            bar.ok,
            bar.failed
        );
        shapes.push(&bar_group(x, ok_h, fail_h, &title));
        if index % 3 == 0 {
            shapes.push(&hour_label(x, bar.hour));
        }
    }
    let width = hours.len() as i64 * STEP;
    tag(
        "svg",
        &[
            ("class", "chart"),
            ("viewBox", &format!("0 0 {width} 92")),
            ("role", "img"),
            ("aria-label", "Runs collected per hour, ok and failed"),
        ],
        &shapes,
    )
}

fn bar_group(x: i64, ok_h: f64, fail_h: f64, title: &str) -> Html {
    use crate::html::empty;
    let rect = |class: &str, y: f64, height: f64| {
        empty(
            "rect",
            &[
                ("class", class),
                ("x", &x.to_string()),
                ("y", &format!("{y:.1}")),
                ("width", "16"),
                ("height", &format!("{height:.1}")),
            ],
        )
    };
    let mut shapes = rect("bar-ok", 76.0 - ok_h, ok_h);
    shapes.push(&rect("bar-fail", 76.0 - ok_h - fail_h, fail_h));
    shapes.push(&rect("bar-hit", 0.0, 76.0));
    tag("g", &[], &join([text_tag("title", &[], title), shapes]))
}

fn hour_label(x: i64, hour: i64) -> Html {
    tag(
        "text",
        &[("class", "axis"), ("x", &x.to_string()), ("y", "90")],
        &Html::text(&format!("{:02}", hour.rem_euclid(24))),
    )
}

fn failures(ctx: &ProjectCtx, now: i64, rows: &[RunRow]) -> Html {
    if rows.is_empty() {
        return layout::notice("No failures in the last 24 hours.");
    }
    let base = layout::project_path(&ctx.principal, &ctx.project, "")
        .trim_end_matches('/')
        .to_string();
    let body = rows
        .iter()
        .map(|run| failure_row(&base, now, run))
        .collect();
    table(
        "table failures",
        &["Time", "Task / need", "Kind", "Slot", "Machine", "Job"],
        body,
    )
}

fn failure_row(base: &str, now: i64, run: &RunRow) -> Html {
    let when = run.collected_at.unwrap_or(run.dispatched_at);
    row_classed(vec![
        ("nowrap", time_cell(when, now)),
        ("", task_cell(run)),
        (
            "",
            join([
                Html::text(run.kind.as_deref().unwrap_or("")),
                Html::text(" "),
                super::widgets::result_pills(run),
            ]),
        ),
        ("", mono(&format!("{}/{}", run.pool, run.slot))),
        ("", Html::text(&run.machine)),
        ("", job_link(base, run)),
    ])
}
