//! The page frame: header with the project picker, the left menu, and the
//! small pieces pages share (pills, tables, notices).

use crate::host::{Catalog, ProjectRef};
use crate::html::{Html, div, join, link_text, span, tag, text_tag, url_encode, void};

/// The left menu: URL segment, label.
pub const MENU: &[(&str, &str)] = &[
    ("overview", "Overview"),
    ("slots", "Slots"),
    ("jobs", "Jobs"),
    ("needs", "Needs"),
    ("failures", "Failures"),
    ("pump", "Pump"),
    ("pools", "Pools and roles"),
    ("mail", "Mail"),
];

/// Who is looking at which project, and which menu entry is lit.
pub struct Chrome<'a> {
    pub catalog: &'a Catalog,
    pub project: Option<&'a ProjectRef>,
    pub active: &'a str,
    pub title: &'a str,
    /// Show the auto-refresh toggle.
    pub refresh: bool,
}

/// `/ui/p/<project>/<rest>`, with the project and every segment encoded
/// except `rest` itself, which the caller builds from encoded pieces.
#[must_use]
pub fn project_path(catalog: &Catalog, project: &ProjectRef, rest: &str) -> String {
    format!("/ui/p/{}/{rest}", url_encode(catalog.url_key(project)))
}

/// A complete document with the frame around `body`.
#[must_use]
pub fn page(chrome: &Chrome<'_>, body: &Html) -> Html {
    let mut frame = Html::new();
    frame.push(&header(chrome));
    let mut shell = Html::new();
    if chrome.project.is_some() {
        shell.push(&menu(chrome));
    }
    shell.push(&tag("main", &[("id", "main")], body));
    frame.push(&div("shell", &shell));
    document(chrome.title, &frame)
}

fn document(title: &str, body: &Html) -> Html {
    let head = join([
        void("meta", &[("charset", "utf-8")]),
        void(
            "meta",
            &[
                ("name", "viewport"),
                ("content", "width=device-width, initial-scale=1"),
            ],
        ),
        text_tag("title", &[], &format!("{title} - vivi console")),
        void(
            "link",
            &[("rel", "stylesheet"), ("href", "/ui/static/console.css")],
        ),
        tag(
            "script",
            &[("src", "/ui/static/console.js"), ("defer", "")],
            &Html::new(),
        ),
    ]);
    let mut out = Html::raw("<!doctype html>");
    out.push(&tag(
        "html",
        &[("lang", "en")],
        &join([tag("head", &[], &head), tag("body", &[], body)]),
    ));
    out
}

fn header(chrome: &Chrome<'_>) -> Html {
    let mut bar = Html::new();
    bar.push(&tag(
        "a",
        &[("class", "brand"), ("href", "/ui/")],
        &Html::text("vivi console"),
    ));
    if let Some(project) = chrome.project {
        bar.push(&picker(chrome, project));
    }
    bar.push(&div("grow", &Html::new()));
    if chrome.refresh {
        bar.push(&refresh_toggle());
    }
    tag("header", &[("class", "top")], &bar)
}

fn refresh_toggle() -> Html {
    tag(
        "label",
        &[("class", "refresh")],
        &join([
            void("input", &[("type", "checkbox"), ("id", "auto-refresh")]),
            Html::text(" auto-refresh 10 s"),
        ]),
    )
}

/// The project picker: a dropdown of links, so it needs no script. Each
/// link keeps the section being viewed.
fn picker(chrome: &Chrome<'_>, current: &ProjectRef) -> Html {
    let section = if chrome.active.is_empty() {
        "overview"
    } else {
        chrome.active
    };
    let items = chrome.catalog.projects.iter().map(|project| {
        let href = project_path(chrome.catalog, project, section);
        tag("li", &[], &link_text(&href, &label(project)))
    });
    let list = tag("ul", &[], &join(items));
    tag(
        "details",
        &[("class", "picker")],
        &join([text_tag("summary", &[], &label(current)), list]),
    )
}

fn label(project: &ProjectRef) -> String {
    if project.name.is_empty() {
        project.id.clone()
    } else {
        project.name.clone()
    }
}

fn menu(chrome: &Chrome<'_>) -> Html {
    let Some(project) = chrome.project else {
        return Html::new();
    };
    let items = MENU.iter().map(|(segment, name)| {
        let href = project_path(chrome.catalog, project, segment);
        let class = if *segment == chrome.active {
            "active"
        } else {
            ""
        };
        tag("a", &[("href", &href), ("class", class)], &Html::text(name))
    });
    tag("nav", &[("class", "menu")], &join(items))
}

/// A status pill. `kind` is one of `busy`, `free`, `failed`, `conflict`,
/// `ok`, or `muted`; it picks the colour.
#[must_use]
pub fn pill(kind: &str, text: &str) -> Html {
    span(&format!("pill pill-{kind}"), text)
}

/// A page title with an optional subtitle line.
#[must_use]
pub fn heading(title: &str, subtitle: &str) -> Html {
    let mut out = text_tag("h1", &[], title);
    if !subtitle.is_empty() {
        out.push(&text_tag("p", &[("class", "sub")], subtitle));
    }
    out
}

/// A quiet notice box.
#[must_use]
pub fn notice(text: &str) -> Html {
    div("notice", &Html::text(text))
}

/// A table with a header row; each row is already built from cells.
#[must_use]
pub fn table(class: &str, head: &[&str], rows: Vec<Html>) -> Html {
    let header = tag(
        "thead",
        &[],
        &tag(
            "tr",
            &[],
            &join(head.iter().map(|name| text_tag("th", &[], name))),
        ),
    );
    tag(
        "table",
        &[("class", class)],
        &join([header, tag("tbody", &[], &join(rows))]),
    )
}

/// One table row from cells, each with its own class.
#[must_use]
pub fn row_classed(cells: Vec<(&str, Html)>) -> Html {
    tag(
        "tr",
        &[],
        &join(
            cells
                .iter()
                .map(|(class, cell)| tag("td", &[("class", class)], cell)),
        ),
    )
}

/// A monospace span for ids and hashes.
#[must_use]
pub fn mono(text: &str) -> Html {
    span("mono", text)
}
