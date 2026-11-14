//! `GET /ui/p/{project}/mail` and `/mail/{handle}`: threads, read-only.
//! Bodies are shown as escaped text in `<pre>`; markdown is not rendered.

use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;

use super::chips::{self, Chip};
use super::widgets::time_cell;
use super::{blocking, chrome_of, html_response, now_epoch, read_failed};
use crate::auth::ProjectCtx;
use crate::data::mail::{
    self, KINDS, MailFilter, MailList, ROLES, Thread, ThreadMessage, ThreadRow, ThreadState,
};
use crate::format;
use crate::html::{Html, div, join, link_text, span, tag, text_tag, url_encode};
use crate::layout::{self, heading, mono, notice, pill, project_path, row_classed, table};

/// The query string of the list.
#[derive(Debug, Default, Deserialize)]
pub struct MailQuery {
    pub identity: Option<String>,
    pub role: Option<String>,
    pub kind: Option<String>,
}

impl MailQuery {
    fn filter(&self) -> MailFilter {
        let clean = |value: &Option<String>| value.as_deref().unwrap_or("").trim().to_string();
        MailFilter {
            identity: clean(&self.identity),
            role: clean(&self.role),
            kind: clean(&self.kind),
        }
    }
}

/// The list handler.
pub async fn list_handler(ctx: ProjectCtx, Query(query): Query<MailQuery>) -> Response {
    let root = ctx.project.mailspace_root.clone();
    let filter = query.filter();
    let read_filter = filter.clone();
    match blocking(move || mail::list(&root, &read_filter)).await {
        Ok(list) => {
            let chrome = chrome_of(&ctx, "mail", "Mail", true);
            let body = render_list(&ctx, &filter, list.as_ref(), now_epoch());
            html_response(StatusCode::OK, &layout::page(&chrome, &body))
        }
        Err(message) => read_failed(&ctx, "mail", &message),
    }
}

/// The thread handler. `Path` carries `(project, handle)`.
pub async fn thread_handler(
    ctx: ProjectCtx,
    Path((_, handle)): Path<(String, String)>,
) -> Response {
    let root = ctx.project.mailspace_root.clone();
    let token = handle.clone();
    match blocking(move || mail::thread(&root, &token)).await {
        Ok(state) => {
            let chrome = chrome_of(&ctx, "mail", "Mail thread", false);
            let (status, body) = render_thread(&ctx, &handle, &state);
            html_response(status, &layout::page(&chrome, &body))
        }
        Err(message) => read_failed(&ctx, "mail", &message),
    }
}

fn list_href(ctx: &ProjectCtx, filter: &MailFilter) -> String {
    let query = chips::query(&[
        ("identity", &filter.identity),
        ("role", &filter.role),
        ("kind", &filter.kind),
    ]);
    project_path(&ctx.principal, &ctx.project, &format!("mail{query}"))
}

fn chip_row(
    ctx: &ProjectCtx,
    label: &str,
    names: &[&str],
    current: &str,
    set: impl Fn(&mut MailFilter, &str),
    filter: &MailFilter,
) -> Html {
    let with = |value: &str| {
        let mut next = filter.clone();
        set(&mut next, value);
        list_href(ctx, &next)
    };
    let mut list = vec![Chip {
        text: "all".into(),
        href: with(""),
        active: current.is_empty(),
    }];
    list.extend(names.iter().map(|name| Chip {
        text: (*name).to_string(),
        href: with(name),
        active: *name == current,
    }));
    chips::row(label, &list)
}

/// The list page body. `list` is `None` when the project has no store.
#[must_use]
pub fn render_list(
    ctx: &ProjectCtx,
    filter: &MailFilter,
    list: Option<&MailList>,
    now: i64,
) -> Html {
    let mut body = heading("Mail", &ctx.project.name);
    let Some(list) = list else {
        body.push(&notice("This project has no mailspace store yet."));
        return body;
    };
    let identities: Vec<&str> = list.identities.iter().map(String::as_str).collect();
    body.push(&chip_row(
        ctx,
        "Identity",
        &identities,
        &filter.identity,
        |next, value| next.identity = value.to_string(),
        filter,
    ));
    body.push(&chip_row(
        ctx,
        "Folder",
        &ROLES,
        &filter.role,
        |next, value| next.role = value.to_string(),
        filter,
    ));
    body.push(&chip_row(
        ctx,
        "Kind",
        &KINDS,
        &filter.kind,
        |next, value| next.kind = value.to_string(),
        filter,
    ));
    if list.rows.is_empty() {
        body.push(&notice("No threads match."));
        return body;
    }
    body.push(&threads_table(ctx, now, &list.rows));
    body
}

fn epoch_of(stamp: &str) -> Option<i64> {
    match chrono::DateTime::parse_from_rfc3339(stamp) {
        Ok(when) => Some(when.timestamp()),
        Err(_) => None,
    }
}

fn threads_table(ctx: &ProjectCtx, now: i64, rows: &[ThreadRow]) -> Html {
    let body = rows
        .iter()
        .map(|row| {
            let href = project_path(
                &ctx.principal,
                &ctx.project,
                &format!("mail/{}", url_encode(&row.handle)),
            );
            let when = epoch_of(&row.updated_at).map_or_else(
                || Html::text(&row.updated_at),
                |epoch| time_cell(epoch, now),
            );
            row_classed(vec![
                ("nowrap", when),
                ("", mono(&row.identity)),
                ("", Html::text(&row.role)),
                ("", pill("muted", row.kind())),
                ("", link_text(&href, subject_or_none(&row.subject))),
                ("", span("dim", &row.from)),
                ("num", Html::text(&row.replies.to_string())),
            ])
        })
        .collect();
    table(
        "table",
        &[
            "Updated", "Identity", "Folder", "Kind", "Subject", "From", "Replies",
        ],
        body,
    )
}

fn subject_or_none(subject: &str) -> &str {
    if subject.trim().is_empty() {
        "(no subject)"
    } else {
        subject
    }
}

/// The thread page: a status and its body.
#[must_use]
pub fn render_thread(ctx: &ProjectCtx, handle: &str, state: &ThreadState) -> (StatusCode, Html) {
    let mut body = heading("Mail thread", &ctx.project.name);
    body.push(&link_text(
        &project_path(&ctx.principal, &ctx.project, "mail"),
        "all threads",
    ));
    match state {
        ThreadState::NoStore => {
            body.push(&notice("This project has no mailspace store yet."));
            (StatusCode::OK, body)
        }
        ThreadState::NotFound => {
            body.push(&notice(&format!("No letter matches the handle {handle}.")));
            (StatusCode::NOT_FOUND, body)
        }
        ThreadState::Ambiguous(ids) => {
            body.push(&notice(&format!(
                "The handle {handle} matches {} letters; use more of the id.",
                ids.len()
            )));
            for id in ids {
                body.push(&div("", &mono(id)));
            }
            (StatusCode::BAD_REQUEST, body)
        }
        ThreadState::Ready(thread) => {
            body.push(&thread_body(ctx, handle, thread));
            (StatusCode::OK, body)
        }
    }
}

fn thread_body(ctx: &ProjectCtx, handle: &str, thread: &Thread) -> Html {
    let mut out = text_tag(
        "p",
        &[("class", "sub")],
        &format!("{} letters, oldest first.", thread.messages.len()),
    );
    for message in &thread.messages {
        out.push(&letter(ctx, handle, message));
    }
    out
}

fn letter(ctx: &ProjectCtx, opened: &str, message: &ThreadMessage) -> Html {
    let id = format!("m-{}", message.handle);
    let mut head = join([
        mono(&message.handle),
        Html::text(" "),
        text_tag("strong", &[], subject_or_none(&message.subject)),
    ]);
    if let Some(kind) = &message.kind {
        head.push(&Html::text(" "));
        head.push(&pill("muted", kind));
    }
    if let Some(event) = &message.event {
        head.push(&Html::text(" "));
        head.push(&pill("conflict", event));
    }
    let mut meta = Html::new();
    for (label, value) in [
        ("From", &message.from),
        ("To", &message.to),
        ("Cc", &message.cc),
        ("Date", &message.date),
    ] {
        if !value.is_empty() {
            meta.push(&div(
                "dim",
                &join([Html::text(&format!("{label}: ")), Html::text(value)]),
            ));
        }
    }
    if let Some(parent) = &message.parent_handle {
        let href = project_path(
            &ctx.principal,
            &ctx.project,
            &format!("mail/{}", url_encode(parent)),
        );
        meta.push(&div(
            "dim",
            &join([Html::text("Answers: "), link_text(&href, parent)]),
        ));
    }
    let mut parts = join([div("letter-head", &head), meta]);
    parts.push(&tag(
        "pre",
        &[("class", "mailbody")],
        &Html::text(&message.body),
    ));
    if let Some(note) = &message.note {
        parts.push(&notice(note));
    }
    let class = if message.handle == opened || format::short(opened) == message.handle {
        "letter letter-open"
    } else {
        "letter"
    };
    tag("section", &[("class", class), ("id", &id)], &parts)
}

#[cfg(test)]
#[path = "mail_test.rs"]
mod tests;
