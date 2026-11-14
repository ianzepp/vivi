//! A row of link chips for filters kept in the query string: one chip is
//! lit, the rest are links that set that filter.

use crate::html::{Html, div, join, link_text, span, tag, url_encode};

/// One chip: its text, the address it links to, and whether it is lit.
pub struct Chip {
    pub text: String,
    pub href: String,
    pub active: bool,
}

/// A labelled row of chips.
#[must_use]
pub fn row(label: &str, chips: &[Chip]) -> Html {
    let items = chips.iter().map(|chip| {
        if chip.active {
            span("chip chip-on", &chip.text)
        } else {
            tag(
                "span",
                &[("class", "chip")],
                &link_text(&chip.href, &chip.text),
            )
        }
    });
    div("chips", &join([span("chips-label", label), join(items)]))
}

/// A labelled row holding one lit chip and a link that removes it.
#[must_use]
pub fn removable(label: &str, text: &str, remove_href: &str) -> Html {
    let chip = tag(
        "span",
        &[("class", "chip chip-on")],
        &join([
            Html::text(text),
            Html::text(" "),
            tag(
                "a",
                &[("href", remove_href), ("title", "remove this filter")],
                &Html::text("\u{d7}"),
            ),
        ]),
    );
    div("chips", &join([span("chips-label", label), chip]))
}

/// `?a=1&b=2` from the pairs whose value is not empty, encoded. Empty when
/// every value is empty.
#[must_use]
pub fn query(pairs: &[(&str, &str)]) -> String {
    let parts: Vec<String> = pairs
        .iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(key, value)| format!("{key}={}", url_encode(value)))
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    }
}
