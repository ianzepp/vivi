//! Pieces several pages use: times, run results, the runtime meter, and the
//! notice for a store that has no slot index.

use crate::context::ProjectCtx;
use crate::data::RunsState;
use crate::data::runs::RunRow;
use crate::format;
use crate::html::{Html, join, link_text, span, tag, text_tag, url_encode};
use crate::layout::{self, mono, notice, pill};

/// One `<option>` of a filter menu.
#[must_use]
pub fn option(value: &str, label: &str, selected: bool) -> Html {
    let mut attrs = vec![("value", value)];
    if selected {
        attrs.push(("selected", ""));
    }
    text_tag("option", &attrs, label)
}

/// A filter menu: `all` (the empty value) and one option per value.
#[must_use]
pub fn select(name: &str, all: &str, values: &[String], current: &str) -> Html {
    let mut options = vec![option("", all, current.is_empty())];
    for value in values {
        options.push(option(value, value, value == current));
    }
    tag("select", &[("name", name)], &join(options))
}

/// The address of a slot's history page.
#[must_use]
pub fn slot_href(ctx: &ProjectCtx, pool: &str, slot: &str) -> String {
    layout::project_path(
        &ctx.catalog,
        &ctx.project,
        &format!("slots/{}/{}", url_encode(pool), url_encode(slot)),
    )
}

/// A time with its full UTC stamp as a tooltip.
#[must_use]
pub fn time_cell(epoch: i64, now: i64) -> Html {
    let stamp = format::iso(epoch);
    tag(
        "time",
        &[("datetime", &stamp), ("title", &format!("{stamp} UTC"))],
        &Html::text(&format::time(epoch, now)),
    )
}

/// What to show instead of a table when there is no slot index to read.
#[must_use]
pub fn missing_runs_notice<T>(state: &RunsState<T>) -> Option<Html> {
    match state {
        RunsState::Ready(_) => None,
        RunsState::NoStore => Some(notice(
            "This project has no mailspace store yet, so there are no runs to show.",
        )),
        RunsState::NoSlotRuns => Some(notice(
            "This store has no slot index (the slot_runs table). A vivi with storage \
             schema 11 creates it; until then there is nothing to show here.",
        )),
    }
}

/// The pill for a run's landing, if it has one.
#[must_use]
pub fn landing_pill(landing: Option<&str>) -> Option<Html> {
    let landing = landing.filter(|text| !text.is_empty())?;
    let kind = match landing {
        "landed" => "ok",
        "conflict" | "raced" => "conflict",
        _ => "failed",
    };
    Some(pill(kind, landing))
}

/// Result pills for a collected run: the outcome, exit codes when they are
/// not zero, and the landing.
#[must_use]
pub fn result_pills(run: &RunRow) -> Html {
    let mut out = Html::new();
    let outcome = run.outcome.as_deref().unwrap_or("");
    if outcome.is_empty() {
        out.push(&pill("busy", "running"));
        return out;
    }
    if outcome != "finished" {
        out.push(&pill("failed", outcome));
    }
    if run.agent_rc.unwrap_or(0) != 0 {
        out.push(&pill(
            "failed",
            &format!("agent rc {}", run.agent_rc.unwrap_or(0)),
        ));
    }
    if run.check_rc.unwrap_or(0) != 0 {
        out.push(&pill(
            "failed",
            &format!("check rc {}", run.check_rc.unwrap_or(0)),
        ));
    }
    if !run.failed() {
        out.push(&pill("ok", "finished"));
    }
    if let Some(landing) = landing_pill(run.landing.as_deref()) {
        out.push(&landing);
    }
    out
}

/// The task and need of a run, as handles: `need a1f3c2d0` then `task 3dc2af9b`.
#[must_use]
pub fn task_cell(run: &RunRow) -> Html {
    let mut parts = Vec::new();
    if let Some(need) = run.need.as_deref().filter(|need| !need.is_empty()) {
        parts.push(join([Html::text("need "), mono(&format::short(need))]));
    }
    parts.push(join([
        Html::text("task "),
        mono(&format::task_handle(&run.task_message_id)),
    ]));
    let mut out = Html::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            out.push(&span("dim", " · "));
        }
        out.push(part);
    }
    out
}

/// A link to a job's page.
#[must_use]
pub fn job_link(project_path: &str, run: &RunRow) -> Html {
    let href = format!("{project_path}/jobs/{}", url_encode(&run.job));
    link_text(&href, &format::short(&run.job))
}

/// A thin bar showing `elapsed` against `budget`. Over budget turns red.
/// Drawn as SVG attributes so the page needs no inline style.
#[must_use]
pub fn meter(elapsed: i64, budget: Option<i64>) -> Html {
    let label = match budget {
        Some(budget) if budget > 0 => {
            format!(
                "{} / {}",
                format::duration(elapsed),
                format::duration(budget)
            )
        }
        _ => format::duration(elapsed),
    };
    let (fraction, over) = match budget {
        Some(budget) if budget > 0 => {
            let ratio = elapsed.max(0) as f64 / budget as f64;
            (ratio.min(1.0), ratio > 1.0)
        }
        _ => (0.0, false),
    };
    let fill = if over {
        "meter-fill meter-over"
    } else {
        "meter-fill"
    };
    let width = format!("{:.1}", fraction * 100.0);
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
                &[("class", fill), ("width", &width), ("height", "8")],
            ),
        ]),
    );
    join([bar, span("meter-label", &label)])
}
