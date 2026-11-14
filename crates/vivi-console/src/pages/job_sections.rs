//! The folding sections of the job page: brief, check, agent, logs, landing,
//! thread, raw records and events. Each is a `<details>` with an anchor.

use super::text_block::{Keep, pre};
use super::widgets::time_cell;
use crate::data::job::{JobDetail, TaskEvent};
use crate::data::job_notes::{self, JobRecords, Landing, Receipt};
use crate::data::letter::Letter;
use crate::format;
use crate::html::{Html, join, span, tag, text_tag};
use crate::layout::{mono, notice, pill, row_classed, table};

/// All sections, in page order.
#[must_use]
pub fn sections(now: i64, detail: &JobDetail) -> Html {
    let records = job_notes::records_of(&detail.events, &detail.run.job);
    let receipt = job_notes::receipt_of(&detail.replies, &detail.run.job);
    join([
        brief(detail),
        check(&records),
        agent(receipt.as_ref(), &records),
        logs(receipt.as_ref()),
        landing(now, detail, &records),
        thread(&detail.replies),
        raw(&records),
        events(now, detail),
    ])
}

/// A folding section with an anchor `id`. `hint` is quiet text after the title.
fn section(id: &str, title: &str, hint: &str, open: bool, body: &Html) -> Html {
    let mut attrs = vec![("id", id), ("class", "section")];
    if open {
        attrs.push(("open", ""));
    }
    let summary = text_tag("summary", &[], title);
    let summary = if hint.is_empty() {
        summary
    } else {
        tag(
            "summary",
            &[],
            &join([Html::text(title), Html::text(" "), span("dim", hint)]),
        )
    };
    tag("details", &attrs, &join([summary, body.clone()]))
}

fn empty(text: &str) -> Html {
    tag("p", &[("class", "dim")], &Html::text(text))
}

fn brief(detail: &JobDetail) -> Html {
    match &detail.brief {
        Some(letter) => {
            let hint = if letter.subject.is_empty() {
                String::new()
            } else {
                letter.subject.clone()
            };
            section(
                "brief",
                "Brief",
                &hint,
                true,
                &pre(&letter.body, Keep::Head),
            )
        }
        None => section(
            "brief",
            "Brief",
            "not found",
            false,
            &empty("The task's text is not in the store."),
        ),
    }
}

fn check(records: &JobRecords) -> Html {
    let tail = records
        .collected
        .as_deref()
        .and_then(|note| job_notes::text_field(note, "check_tail"));
    let has = tail.is_some();
    match tail {
        Some(text) => section(
            "check",
            "Check output",
            "tail",
            has,
            &pre(&text, Keep::Tail),
        ),
        None => section(
            "check",
            "Check output",
            "none recorded",
            false,
            &empty(
                "The collect record has no check output (the job ran no check, or has not been collected).",
            ),
        ),
    }
}

/// The agent's final message, with the summary lines and facts around it.
fn agent(receipt: Option<&Receipt>, records: &JobRecords) -> Html {
    let Some(receipt) = receipt else {
        let hint = if records.collected.is_some() {
            "no receipt"
        } else {
            "not collected"
        };
        return section(
            "agent",
            "Agent",
            hint,
            false,
            &empty("There is no collect receipt on the task."),
        );
    };
    let mut body = Html::new();
    for (title, text, keep) in [
        ("summary", Some(&receipt.head), Keep::Head),
        ("final message", receipt.agent.as_ref(), Keep::Tail),
        ("facts", receipt.facts.as_ref(), Keep::Head),
    ] {
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            body.push(&text_tag("h3", &[], title));
            body.push(&pre(text, keep));
        }
    }
    if body.is_empty() {
        body = empty("The receipt is empty.");
    }
    section("agent", "Agent", "", false, &body)
}

fn logs(receipt: Option<&Receipt>) -> Html {
    let parts = receipt.map(|receipt| {
        [
            ("stderr tail", receipt.stderr.as_ref()),
            ("runner log tail", receipt.runner_log.as_ref()),
        ]
    });
    let mut body = Html::new();
    for (title, text) in parts.into_iter().flatten() {
        if let Some(text) = text {
            body.push(&text_tag("h3", &[], title));
            body.push(&pre(text, Keep::Tail));
        }
    }
    if body.is_empty() {
        return section(
            "logs",
            "Logs",
            "none recorded",
            false,
            &empty("The receipt holds no stderr or runner log tail."),
        );
    }
    section("logs", "Logs", "stderr and runner log tails", false, &body)
}

fn landing(now: i64, detail: &JobDetail, records: &JobRecords) -> Html {
    let run = &detail.run;
    let note = records.landed.as_deref().and_then(job_notes::landing_of);
    let word = note
        .as_ref()
        .map(|note| note.word.clone())
        .or_else(|| run.landing.clone().filter(|word| !word.is_empty()));
    let Some(word) = word else {
        return section(
            "landing",
            "Landing",
            "none recorded",
            false,
            &empty("Nothing has been recorded about landing this job."),
        );
    };
    let mut body = Html::new();
    let kind = match word.as_str() {
        "landed" => "ok",
        "conflict" | "raced" => "conflict",
        _ => "failed",
    };
    body.push(&pill(kind, &word));
    if let Some(at) = run.landing_at {
        body.push(&time_cell(at, now));
    }
    match &note {
        Some(note) => body.push(&landing_detail(note)),
        None => {
            if let Some(reason) = run
                .landing_reason
                .as_deref()
                .filter(|text| !text.is_empty())
            {
                body.push(&pre(reason, Keep::Tail));
            }
        }
    }
    section("landing", "Landing", &word, word != "landed", &body)
}

fn landing_detail(note: &Landing) -> Html {
    let mut out = Html::new();
    let mut facts = Vec::new();
    if let Some(base) = &note.base {
        facts.push(format!("base {}", format::short_sha(base)));
    }
    if let Some(tip) = &note.tip {
        facts.push(format!("tip {}", format::short_sha(tip)));
    }
    if let Some(secs) = note.secs {
        facts.push(format!("took {}", format::duration(secs)));
    }
    if !facts.is_empty() {
        out.push(&tag("p", &[], &mono(&facts.join("   "))));
    }
    if let Some(command) = &note.command {
        out.push(&text_tag("h3", &[], "failing command"));
        out.push(&pre(command, Keep::Head));
    }
    if let Some(tail) = &note.tail {
        out.push(&text_tag("h3", &[], "output tail"));
        out.push(&pre(tail, Keep::Tail));
    }
    if !note.files.is_empty() {
        out.push(&text_tag("h3", &[], "files in conflict"));
        let items = note.files.iter().map(|file| tag("li", &[], &mono(file)));
        out.push(&tag("ul", &[("class", "files")], &join(items)));
    }
    out
}

fn thread(replies: &[Letter]) -> Html {
    if replies.is_empty() {
        return section(
            "thread",
            "Thread",
            "no replies",
            false,
            &empty("The task has no replies."),
        );
    }
    let items = replies.iter().map(reply);
    let hint = format!("{} replies", replies.len());
    section("thread", "Thread", &hint, false, &join(items))
}

fn reply(letter: &Letter) -> Html {
    let when = letter.at.map_or_else(String::new, format::iso);
    let who = [when.as_str(), letter.from.as_str(), letter.subject.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("  ");
    tag(
        "div",
        &[("class", "reply")],
        &join([
            tag("h3", &[("class", "reply-head")], &Html::text(&who)),
            pre(&letter.body, Keep::Head),
        ]),
    )
}

fn raw(records: &JobRecords) -> Html {
    let mut body = Html::new();
    for (title, note) in [
        ("dispatch", &records.dispatched),
        ("collect", &records.collected),
        ("landing", &records.landed),
    ] {
        body.push(&text_tag("h3", &[], title));
        match note {
            Some(note) => body.push(&pre(&job_notes::pretty(note), Keep::Head)),
            None => body.push(&empty("not recorded")),
        }
    }
    section(
        "raw",
        "Raw records",
        "dispatch, collect and landing notes",
        false,
        &body,
    )
}

fn events(now: i64, detail: &JobDetail) -> Html {
    if detail.events.is_empty() {
        return section(
            "events",
            "Events",
            "none",
            false,
            &notice("The task has no events."),
        );
    }
    let rows = detail
        .events
        .iter()
        .map(|event| event_row(now, &detail.run.job, event))
        .collect();
    let hint = format!("{} on the task", detail.events.len());
    section(
        "events",
        "Events",
        &hint,
        false,
        &table(
            "table events",
            &["Time", "Event", "Job", "Command", "Actor"],
            rows,
        ),
    )
}

/// One event. Pool events of another job on the same task are dimmed.
fn event_row(now: i64, job: &str, event: &TaskEvent) -> Html {
    let named = event.note.as_deref().and_then(job_notes::job_of);
    let class = if named.as_deref().is_some_and(|other| other != job) {
        "dim"
    } else {
        ""
    };
    row_classed(vec![
        ("nowrap", time_cell(event.at, now)),
        (class, Html::text(&event.event_type)),
        (
            class,
            named.map_or_else(Html::new, |name| mono(&format::short(&name))),
        ),
        (class, Html::text(&event.command)),
        (class, Html::text(event.actor.as_deref().unwrap_or(""))),
    ])
}
