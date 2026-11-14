//! `GET /ui/p/{project}/pump`: health, then what the pump recorded.

use axum::http::StatusCode;
use axum::response::Response;

use super::overview::{card, pump_card};
use super::widgets::time_cell;
use super::{blocking, chrome_of, html_response, now_epoch, read_failed};
use crate::auth::ProjectCtx;
use crate::data::pump_page::{self, Gate, PumpData, PumpException, PumpPage, RungRun, StoreState};
use crate::format;
use crate::html::{Html, div, join, span, text_tag};
use crate::layout::{self, heading, mono, notice, pill, row_classed, table};

/// Longest text shown from a free-text column.
const CLIP: usize = 240;

/// The handler.
pub async fn handler(ctx: ProjectCtx) -> Response {
    let root = ctx.project.mailspace_root.clone();
    let now = now_epoch();
    match blocking(move || pump_page::load(&root, now)).await {
        Ok(data) => {
            let chrome = chrome_of(&ctx, "pump", "Pump", true);
            let body = render(&ctx, &data);
            html_response(StatusCode::OK, &layout::page(&chrome, &body))
        }
        Err(message) => read_failed(&ctx, "pump", &message),
    }
}

/// The page body.
#[must_use]
pub fn render(ctx: &ProjectCtx, page: &PumpPage) -> Html {
    let mut body = heading("Pump", &ctx.project.name);
    match &page.store {
        StoreState::NoPump => {
            body.push(&notice(
                "This project has no pump: its config has no [pump] table.",
            ));
        }
        StoreState::Unavailable(message) => {
            body.push(&div("cards", &card("Health", &pump_card(&page.health))));
            body.push(&notice(&format!(
                "The pump store cannot be shown. {message}"
            )));
        }
        StoreState::Ready(data) => {
            body.push(&cards(page, data));
            body.push(&units(data));
            body.push(&events(page.now, data));
            body.push(&gates(page.now, &data.gates));
            body.push(&exceptions(page.now, &data.exceptions));
            body.push(&rungs(page.now, &data.rungs));
        }
    }
    body
}

fn cards(page: &PumpPage, data: &PumpData) -> Html {
    let mut row = card("Health", &pump_card(&page.health));
    let control = match &data.control {
        Some((mode, shutdown)) => join([
            div("big", &Html::text(mode)),
            div(
                "small",
                &Html::text(if *shutdown {
                    "shutdown requested"
                } else {
                    "no shutdown requested"
                }),
            ),
        ]),
        None => div("big", &Html::text("not set")),
    };
    row.push(&card("Control", &control));
    div("cards", &row)
}

fn clip(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= CLIP {
        flat
    } else {
        format!("{}...", flat.chars().take(CLIP).collect::<String>())
    }
}

fn units(data: &PumpData) -> Html {
    let mut out = text_tag("h2", &[], "Units by status");
    if data.units.is_empty() {
        out.push(&notice("The pump has no units."));
        return out;
    }
    let rows = data
        .units
        .iter()
        .map(|(status, count)| {
            row_classed(vec![
                ("", status_pill(status)),
                ("num", Html::text(&count.to_string())),
            ])
        })
        .collect();
    out.push(&table("table pools", &["Status", "Units"], rows));
    out
}

fn status_pill(status: &str) -> Html {
    let kind = match status {
        "inflight" => "busy",
        "merged" | "no-defect" => "ok",
        "stuck" | "needs-replan" | "quarantined" => "failed",
        "held" | "splitting" => "conflict",
        _ => "muted",
    };
    pill(kind, status)
}

fn events(now: i64, data: &PumpData) -> Html {
    let mut out = text_tag("h2", &[], "Unit events, newest first");
    if data.events.is_empty() {
        out.push(&notice("No unit events yet."));
        return out;
    }
    let rows = data
        .events
        .iter()
        .map(|event| {
            row_classed(vec![
                ("nowrap", time_cell(event.at, now)),
                ("", mono(&format::short(&event.unit))),
                ("num", Html::text(&event.attempt.to_string())),
                ("", Html::text(event.model.as_deref().unwrap_or(""))),
                ("", Html::text(&event.why)),
                ("", span("dim", &clip(&event.detail))),
            ])
        })
        .collect();
    out.push(&table(
        "table",
        &["Time", "Unit", "Attempt", "Model", "Why", "Detail"],
        rows,
    ));
    out
}

fn gates(now: i64, gates: &[Gate]) -> Html {
    let mut out = text_tag("h2", &[], "Gates");
    if gates.is_empty() {
        out.push(&notice("No gates recorded."));
        return out;
    }
    let rows = gates
        .iter()
        .map(|gate| {
            let failures = if gate.failures.is_empty() {
                pill("ok", "green")
            } else {
                join(
                    gate.failures
                        .iter()
                        .map(|(name, kind)| pill("failed", &format!("{name} ({kind})"))),
                )
            };
            row_classed(vec![
                ("nowrap", time_cell(gate.at, now)),
                ("", mono(&format::short(&gate.tip))),
                ("num", Html::text(&gate.merges.to_string())),
                ("", failures),
            ])
        })
        .collect();
    out.push(&table(
        "table",
        &["Time", "Tip", "Merges", "Failures"],
        rows,
    ));
    out
}

fn exceptions(now: i64, exceptions: &[PumpException]) -> Html {
    let mut out = text_tag("h2", &[], "Exceptions, newest first");
    if exceptions.is_empty() {
        out.push(&notice("No exceptions recorded."));
        return out;
    }
    let rows = exceptions
        .iter()
        .map(|exception| {
            row_classed(vec![
                ("nowrap", time_cell(exception.at, now)),
                ("", Html::text(&exception.kind)),
                ("", mono(&format::short(&exception.unit))),
                ("", span("dim", &clip(&exception.text))),
            ])
        })
        .collect();
    out.push(&table("table", &["Time", "Kind", "Unit", "Text"], rows));
    out
}

fn rungs(now: i64, rungs: &[RungRun]) -> Html {
    let mut out = text_tag("h2", &[], "Rung runs, newest first");
    if rungs.is_empty() {
        out.push(&notice("No rung runs for the newest flow steps."));
        return out;
    }
    let rows = rungs
        .iter()
        .map(|run| {
            let (took, result) = match run.finished_at {
                Some(end) => (
                    format::duration(end - run.started_at),
                    match run.rc {
                        Some(0) => pill("ok", "rc 0"),
                        Some(rc) => pill("failed", &format!("rc {rc}")),
                        None => pill("muted", "no rc"),
                    },
                ),
                None => (String::from("running"), pill("busy", "running")),
            };
            row_classed(vec![
                ("nowrap", time_cell(run.started_at, now)),
                ("", mono(&format::short(&run.task))),
                ("", Html::text(&run.rung)),
                ("nowrap", Html::text(&took)),
                ("", result),
            ])
        })
        .collect();
    out.push(&table(
        "table",
        &["Started", "Task", "Rung", "Duration", "Result"],
        rows,
    ));
    out
}

#[cfg(test)]
#[path = "pump_test.rs"]
mod tests;
