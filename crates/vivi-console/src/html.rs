//! A very small HTML writer: escaped text, escaped attributes, and a few
//! builders. Markup is trusted only when it came out of this module, so a
//! page cannot smuggle raw text into the output by accident.

use std::fmt::{self, Display};

/// A fragment of markup that is already safe to emit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Html(String);

impl Html {
    /// An empty fragment.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Markup the caller vouches for. Only static strings belong here.
    #[must_use]
    pub fn raw(markup: &str) -> Self {
        Self(markup.to_string())
    }

    /// Plain text, escaped.
    #[must_use]
    pub fn text(text: &str) -> Self {
        Self(escape(text))
    }

    /// Append another fragment.
    pub fn push(&mut self, other: &Html) {
        self.0.push_str(&other.0);
    }

    /// True when nothing has been written.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The markup as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The markup as an owned string.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl Display for Html {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Escape text for an element body or a double-quoted attribute value.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// Percent-encode one URL path segment or query value.
#[must_use]
pub fn url_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn open_tag(name: &str, attrs: &[(&str, &str)]) -> String {
    let mut out = format!("<{name}");
    for (key, value) in attrs {
        out.push_str(&format!(" {key}=\"{}\"", escape(value)));
    }
    out
}

/// `<name attrs>inner</name>`. Names and attribute names come from code;
/// attribute values are escaped.
#[must_use]
pub fn tag(name: &str, attrs: &[(&str, &str)], inner: &Html) -> Html {
    Html(format!("{}>{}</{name}>", open_tag(name, attrs), inner.0))
}

/// `<name attrs>`, for elements with no content (`input`, `meta`, `link`).
#[must_use]
pub fn void(name: &str, attrs: &[(&str, &str)]) -> Html {
    Html(format!("{}>", open_tag(name, attrs)))
}

/// `<name attrs/>`: a self-closing element, for SVG shapes (a bare `<rect>`
/// would swallow the elements after it).
#[must_use]
pub fn empty(name: &str, attrs: &[(&str, &str)]) -> Html {
    Html(format!("{}/>", open_tag(name, attrs)))
}

/// An element holding only text.
#[must_use]
pub fn text_tag(name: &str, attrs: &[(&str, &str)], text: &str) -> Html {
    tag(name, attrs, &Html::text(text))
}

/// `<div class="...">inner</div>`.
#[must_use]
pub fn div(class: &str, inner: &Html) -> Html {
    tag("div", &[("class", class)], inner)
}

/// `<span class="...">text</span>`.
#[must_use]
pub fn span(class: &str, text: &str) -> Html {
    text_tag("span", &[("class", class)], text)
}

/// A link around a fragment.
#[must_use]
pub fn link(href: &str, inner: &Html) -> Html {
    tag("a", &[("href", href)], inner)
}

/// A link around text.
#[must_use]
pub fn link_text(href: &str, text: &str) -> Html {
    link(href, &Html::text(text))
}

/// Concatenate fragments.
pub fn join<I>(items: I) -> Html
where
    I: IntoIterator<Item = Html>,
{
    let mut out = Html::new();
    for item in items {
        out.push(&item);
    }
    out
}

#[cfg(test)]
#[path = "html_test.rs"]
mod tests;
