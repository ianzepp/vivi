//! The flow tab of the need page: the need's runs through the pump's flow,
//! read from `pump.db`.

use vivi_core::pump::flow::def::FlowKind;
use vivi_core::pump::flow::registry;

use super::need::{NeedQuery, need_href};
use super::widgets::time_cell;
use crate::context::ProjectCtx;
use crate::data::need_flow::{
    FanoutGroup, FlowView, PathStep, PumpState, RunDetail, RungInfo, SliceInfo,
};
use crate::data::need_page::{FlowTab, NeedPage};
use crate::format;
use crate::html::{Html, join, link_text, span, tag, text_tag, url_encode};
use crate::layout::{self, mono, notice, pill, row_classed, table};

/// The tab body.
#[must_use]
pub fn render(ctx: &ProjectCtx, page: &NeedPage, query: &NeedQuery, now: i64) -> Html {
    let text = match &page.flow {
        None => "The flow was not read.".to_string(),
        Some(PumpState::NoPump) => String::from(
            "This project has no pump configured, so no flow runs are recorded for this need.",
        ),
        Some(PumpState::NotStarted) => String::from(
            "The pump is configured but has not written its store yet (no pump.db), so there is no flow to show.",
        ),
        Some(PumpState::Unreadable(why)) => format!(
            "The pump store cannot be read right now: {why}. The console never creates \
             the store's shared-memory file, so a pump that is not running leaves it \
             unreadable; start the pump, or use `vivi flow show` on the host."
        ),
        Some(PumpState::Ready(FlowTab::NoRuns)) => String::from(
            "This need has no flow runs; its work was dispatched straight into pool slots.",
        ),
        Some(PumpState::Ready(FlowTab::Runs(view))) => {
            return runs_view(ctx, page, view, query, now);
        }
    };
    notice(&text)
}

fn runs_view(
    ctx: &ProjectCtx,
    page: &NeedPage,
    view: &FlowView,
    query: &NeedQuery,
    now: i64,
) -> Html {
    let mut out = Html::new();
    if let Some(branch) = &view.branch {
        let base = branch
            .base
            .as_deref()
            .map_or(String::new(), |b| format!(" from {}", format::short(b)));
        out.push(&text_tag(
            "p",
            &[("class", "sub")],
            &format!("branch {}{base} ({})", branch.branch, branch.status),
        ));
    }
    out.push(&runs_table(ctx, page, view));
    if let Some(detail) = &view.selected {
        out.push(&run_section(ctx, page, detail, query, now));
    } else {
        out.push(&notice("The run asked for does not exist."));
    }
    out.push(&plans(view, now));
    out
}

fn runs_table(ctx: &ProjectCtx, page: &NeedPage, view: &FlowView) -> Html {
    let selected = view.selected.as_ref().map(|detail| detail.run.run);
    let rows = view
        .runs
        .iter()
        .map(|run| {
            let href = need_href(ctx, &page.handle, &format!("tab=flow&run={}", run.run));
            let label = if Some(run.run) == selected {
                format!("run {} (shown)", run.run)
            } else {
                format!("run {}", run.run)
            };
            let sent = view
                .restarts
                .iter()
                .find(|(number, _)| *number == run.run)
                .map_or(String::new(), |(_, entry)| {
                    format!("restart sent it to {entry}")
                });
            row_classed(vec![
                ("nowrap", link_text(&href, &label)),
                ("", Html::text(&format!("{} v{}", run.flow, run.version))),
                ("", mono(&run.entry)),
                ("", pill("muted", &run.state)),
                ("", Html::text(&sent)),
            ])
        })
        .collect();
    table(
        "table runs",
        &["Run", "Flow", "Entry", "State", "After it"],
        rows,
    )
}

fn run_section(
    ctx: &ProjectCtx,
    page: &NeedPage,
    detail: &RunDetail,
    query: &NeedQuery,
    now: i64,
) -> Html {
    let run = &detail.run;
    let mut out = text_tag(
        "h2",
        &[],
        &format!("Run {} (entry: {})", run.run, run.entry),
    );
    out.push(&text_tag(
        "pre",
        &[("class", "flowline")],
        &path_line(detail),
    ));
    out.push(&path_table(detail, now));
    for group in &detail.groups {
        out.push(&group_section(ctx, group, now));
    }
    let source_href = need_href(
        ctx,
        &page.handle,
        &format!("tab=flow&run={}&source=1", run.run),
    );
    out.push(&tag(
        "p",
        &[],
        &link_text(&source_href, "show the whole flow"),
    ));
    if query.wants_source() {
        out.push(&source_section(detail));
    }
    out
}

/// `[research] -READY-> [plan] ... [build1] (now)`.
fn path_line(detail: &RunDetail) -> String {
    let mut line = String::new();
    for (index, entry) in detail.path.iter().enumerate() {
        if index > 0 {
            let word = detail.path[index - 1].step.verdict.as_deref();
            line.push_str(&match word {
                Some(word) => format!(" -{word}-> "),
                None => String::from(" --> "),
            });
        }
        line.push_str(&format!("[{}]", entry.step.node));
        if detail.current == Some(index) {
            line.push_str(" (now)");
        }
    }
    line
}

fn path_table(detail: &RunDetail, now: i64) -> Html {
    let rows = detail
        .path
        .iter()
        .enumerate()
        .map(|(index, entry)| path_row(entry, detail.current == Some(index), now))
        .collect();
    table("table path", &["Step", "State", "Answer", "Rung", ""], rows)
}

fn step_pill(state: &str) -> Html {
    let kind = match state {
        "done" => "ok",
        "failed" => "failed",
        "running" => "busy",
        _ => "muted",
    };
    pill(kind, state)
}

fn path_row(entry: &PathStep, current: bool, now: i64) -> Html {
    let marker = if current {
        pill("busy", "now")
    } else {
        Html::new()
    };
    row_classed(vec![
        ("nowrap", mono(&entry.step.node)),
        ("", step_pill(&entry.step.state)),
        ("", Html::text(entry.step.verdict.as_deref().unwrap_or(""))),
        ("", rung_cell(entry.rung.as_ref(), now)),
        ("", marker),
    ])
}

fn rung_cell(rung: Option<&RungInfo>, now: i64) -> Html {
    let Some(rung) = rung else {
        return Html::new();
    };
    let (kind, word) = match (rung.finished_at, rung.rc) {
        (None, _) => ("busy", String::from("running")),
        (Some(_), Some(0)) => ("ok", String::from("pass")),
        (Some(_), Some(rc)) => ("failed", format!("fail rc {rc}")),
        (Some(_), None) => ("muted", String::from("no result")),
    };
    let wall = rung.finished_at.map_or(now, |end| end) - rung.started_at;
    join([
        Html::text(&format!("{} ", rung.rung)),
        pill(kind, &word),
        span("dim", &format!(" {} ", format::duration(wall))),
        time_cell(rung.started_at, now),
    ])
}

fn group_section(ctx: &ProjectCtx, group: &FanoutGroup, now: i64) -> Html {
    let mut title = format!("Slices of {}", group.node);
    if let Some(tip) = &group.tip {
        title.push_str(&format!(" (started from {})", format::short(tip)));
    }
    let mut out = text_tag("h3", &[], &title);
    if group.open {
        out.push(&pill("busy", "open"));
    }
    let base = layout::project_path(&ctx.catalog, &ctx.project, "")
        .trim_end_matches('/')
        .to_string();
    let rows = group
        .slices
        .iter()
        .map(|slice| slice_row(&base, slice, now))
        .collect();
    out.push(&table(
        "table slices",
        &[
            "Slice",
            "Write scope",
            "State",
            "Tries",
            "Scope",
            "Task rung",
            "Job",
        ],
        rows,
    ));
    out
}

fn slice_row(base: &str, slice: &SliceInfo, now: i64) -> Html {
    let scope = join(slice.scope.iter().map(|path| tag("div", &[], &mono(path))));
    let mut state = step_pill(&slice.state);
    if let Some(verdict) = &slice.verdict {
        state.push(&span("dim", &format!(" {verdict}")));
    }
    let job = slice.last_job.as_ref().map_or_else(Html::new, |job| {
        link_text(
            &format!("{base}/jobs/{}", url_encode(job)),
            &format::short(job),
        )
    });
    row_classed(vec![
        ("nowrap", mono(&slice.id)),
        ("", scope),
        ("", state),
        ("num", Html::text(&slice.tries.to_string())),
        ("", scope_cell(slice)),
        ("", rung_cell(slice.rung.as_ref(), now)),
        ("", job),
    ])
}

/// Blocked-scope resolution: what the slice asked for and who holds it.
fn scope_cell(slice: &SliceInfo) -> Html {
    if slice.scope_state == "none" && slice.scope_requests == 0 {
        return Html::new();
    }
    let mut parts = vec![format!(
        "{} ({} request(s))",
        slice.scope_state, slice.scope_requests
    )];
    if !slice.held_paths.is_empty() {
        parts.push(format!("wants {}", slice.held_paths.join(", ")));
    }
    if !slice.holders.is_empty() {
        parts.push(format!("held by {}", slice.holders.join(", ")));
    }
    Html::text(&parts.join("; "))
}

fn plans(view: &FlowView, now: i64) -> Html {
    if view.plans.is_empty() {
        return Html::new();
    }
    let rows = view
        .plans
        .iter()
        .map(|plan| {
            row_classed(vec![
                ("num", Html::text(&plan.revision.to_string())),
                ("", Html::text(&plan.verdict)),
                ("", Html::text(&plan.reason)),
                ("nowrap", time_cell(plan.created_at, now)),
            ])
        })
        .collect();
    join([
        text_tag("h2", &[], "Plan revisions"),
        table(
            "table plans",
            &["Revision", "Verdict", "Reason", "Recorded"],
            rows,
        ),
    ])
}

/// The flow's Mermaid source as this server ships it, with the nodes this
/// run visited listed beside it. No diagram is drawn.
fn source_section(detail: &RunDetail) -> Html {
    let mut out = text_tag("h2", &[], "The whole flow");
    let visited: Vec<&str> = detail
        .path
        .iter()
        .map(|entry| entry.step.node.as_str())
        .collect();
    out.push(&text_tag(
        "p",
        &[("class", "sub")],
        &format!("Nodes this run visited: {}", visited.join(", ")),
    ));
    let source = FlowKind::parse(&detail.run.flow).and_then(registry::shipped);
    match source {
        Some(source) => {
            out.push(&text_tag(
                "p",
                &[("class", "sub")],
                &format!(
                    "This is the source the server's vivi ships ({}); the run was made under flow version {}.",
                    registry::FLOW_VERSION,
                    detail.run.version
                ),
            ));
            out.push(&text_tag("pre", &[("class", "flowsrc")], source));
        }
        None => out.push(&notice(
            "This server does not ship a flow named like this run's.",
        )),
    }
    out
}
