//! `GET /ui/p/{project}/slots`: tiles by machine, and one row per slot.
//! Filters (`?pool=&machine=`) live in the query string.

use std::collections::BTreeMap;

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;

use super::widgets::{meter, missing_runs_notice, select, slot_href, task_cell, time_cell};
use super::{blocking, chrome_of, html_response, now_epoch, read_failed};
use crate::auth::ProjectCtx;
use crate::data::RunsState;
use crate::data::slots::{self, LastRun, SlotState, SlotView, SlotsData};
use crate::format;
use crate::html::{Html, div, join, link, link_text, span, tag, text_tag};
use crate::layout::{self, heading, mono, notice, pill, row_classed, table};

/// The query string.
#[derive(Debug, Default, Deserialize)]
pub struct SlotsQuery {
    pub pool: Option<String>,
    pub machine: Option<String>,
}

impl SlotsQuery {
    fn pool(&self) -> &str {
        self.pool.as_deref().unwrap_or("")
    }

    fn machine(&self) -> &str {
        self.machine.as_deref().unwrap_or("")
    }

    fn keeps(&self, slot: &SlotView) -> bool {
        (self.pool().is_empty() || slot.pool == self.pool())
            && (self.machine().is_empty() || machine_name(&slot.machine) == self.machine())
    }
}

/// The handler.
pub async fn handler(ctx: ProjectCtx, Query(query): Query<SlotsQuery>) -> Response {
    let root = ctx.project.mailspace_root.clone();
    let now = now_epoch();
    match blocking(move || slots::load(&root, now)).await {
        Ok(data) => {
            let chrome = chrome_of(&ctx, "slots", "Slots", true);
            let body = render(&ctx, &data, &query);
            html_response(StatusCode::OK, &layout::page(&chrome, &body))
        }
        Err(message) => read_failed(&ctx, "slots", &message),
    }
}

/// The page body.
#[must_use]
pub fn render(ctx: &ProjectCtx, data: &RunsState<SlotsData>, query: &SlotsQuery) -> Html {
    let mut body = heading("Slots", &ctx.project.name);
    let RunsState::Ready(data) = data else {
        if let Some(text) = missing_runs_notice(data) {
            body.push(&text);
        }
        return body;
    };
    body.push(&filters(ctx, data, query));
    let shown: Vec<&SlotView> = data.slots.iter().filter(|slot| query.keeps(slot)).collect();
    if shown.is_empty() {
        body.push(&notice(
            "No slots match. Slots appear here once they have run work.",
        ));
    } else {
        body.push(&tiles(ctx, &shown));
        body.push(&slot_table(ctx, data.now, &shown));
    }
    for unseen in &data.unseen {
        body.push(&text_tag(
            "p",
            &[("class", "sub")],
            &format!(
                "{} declared slot(s) in pool {} have not run anything in the last 7 days, \
                 so the console has no name for them.",
                unseen.count, unseen.pool
            ),
        ));
    }
    body
}

fn machine_name(machine: &str) -> &str {
    if machine.is_empty() {
        "(unknown)"
    } else {
        machine
    }
}

fn filters(ctx: &ProjectCtx, data: &SlotsData, query: &SlotsQuery) -> Html {
    let mut pools: Vec<String> = data.slots.iter().map(|slot| slot.pool.clone()).collect();
    pools.dedup();
    let mut machines: Vec<String> = data
        .slots
        .iter()
        .map(|slot| machine_name(&slot.machine).to_string())
        .collect();
    machines.sort();
    machines.dedup();
    let action = layout::project_path(&ctx.principal, &ctx.project, "slots");
    let inner = join([
        Html::text("pool "),
        select("pool", "all", &pools, query.pool()),
        Html::text(" machine "),
        select("machine", "all", &machines, query.machine()),
        text_tag("button", &[("type", "submit")], "Filter"),
        link_text(&action, "clear"),
    ]);
    tag(
        "form",
        &[("method", "get"), ("action", &action), ("class", "filters")],
        &inner,
    )
}

fn is_busy(slot: &SlotView) -> bool {
    matches!(slot.state, SlotState::Busy(_))
}

fn tiles(ctx: &ProjectCtx, slots: &[&SlotView]) -> Html {
    let mut by_machine: BTreeMap<&str, Vec<&SlotView>> = BTreeMap::new();
    for slot in slots {
        by_machine
            .entry(machine_name(&slot.machine))
            .or_default()
            .push(slot);
    }
    let sections = by_machine.iter().map(|(machine, members)| {
        let busy = members.iter().filter(|slot| is_busy(slot)).count();
        let heading = join([
            text_tag("h3", &[], machine),
            span("sub", &format!("{busy} busy / {}", members.len())),
        ]);
        let tiles = members.iter().map(|slot| tile(ctx, slot));
        tag(
            "section",
            &[("class", "machine")],
            &join([div("machine-head", &heading), div("tiles", &join(tiles))]),
        )
    });
    div("machines", &join(sections))
}

fn tile(ctx: &ProjectCtx, slot: &SlotView) -> Html {
    let class = if is_busy(slot) {
        "tile tile-busy"
    } else {
        "tile tile-free"
    };
    let state = if is_busy(slot) { "busy" } else { "free" };
    let title = format!("{}/{}: {state}", slot.pool, slot.slot);
    tag(
        "a",
        &[
            ("class", class),
            ("href", &slot_href(ctx, &slot.pool, &slot.slot)),
            ("title", &title),
        ],
        &Html::text(&slot.slot),
    )
}

fn slot_table(ctx: &ProjectCtx, now: i64, slots: &[&SlotView]) -> Html {
    let rows = slots.iter().map(|slot| slot_row(ctx, now, slot)).collect();
    table(
        "table slots",
        &["Slot", "State", "Task", "Need", "Model", "Runtime / budget"],
        rows,
    )
}

fn slot_row(ctx: &ProjectCtx, now: i64, slot: &SlotView) -> Html {
    let name = link(
        &slot_href(ctx, &slot.pool, &slot.slot),
        &join([span("dim", &format!("{} / ", slot.pool)), mono(&slot.slot)]),
    );
    match &slot.state {
        SlotState::Busy(run) => row_classed(vec![
            ("nowrap", name),
            ("", pill("busy", "busy")),
            ("", task_cell(run)),
            (
                "",
                run.need
                    .as_deref()
                    .map_or_else(Html::new, |need| mono(&format::short(need))),
            ),
            ("", Html::text(&run.model)),
            ("meter-cell", meter(now - run.dispatched_at, run.budget_s)),
        ]),
        SlotState::Free(last) => row_classed(vec![
            ("nowrap", name),
            ("", pill("free", "free")),
            ("dim", last_cell(last.as_ref(), now)),
            ("", Html::new()),
            ("", Html::new()),
            ("", Html::new()),
        ]),
    }
}

fn last_cell(last: Option<&LastRun>, now: i64) -> Html {
    let Some(last) = last else {
        return Html::text("no runs recorded");
    };
    let mut out = Html::text("last: ");
    match (&last.outcome, last.collected_at) {
        (Some(outcome), Some(at)) => {
            out.push(&Html::text(&format!("{outcome} ")));
            out.push(&time_cell(at, now));
        }
        _ => out.push(&Html::text("no result recorded")),
    }
    out
}

#[cfg(test)]
#[path = "slots_test.rs"]
mod tests;
