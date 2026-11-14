//! Long stored text on a page: capped, scrubbed of anything that looks like a
//! credential, escaped, and wrapped in a `<pre>`.

use crate::html::{Html, tag};

/// Most bytes of one block shown.
pub const BLOCK_MAX: usize = 200 * 1024;

/// Which end of an over-long text survives the cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    /// A brief or a message: the start matters.
    Head,
    /// A log or a check output: the end matters.
    Tail,
}

/// Words in a name that mark its value as a secret.
const SECRET_NAMES: [&str; 8] = [
    "secret",
    "token",
    "password",
    "passwd",
    "apikey",
    "api_key",
    "api-key",
    "authorization",
];

/// Prefixes of well-known credential formats.
const SECRET_PREFIXES: [&str; 8] = [
    "sk-",
    "ghp_",
    "gho_",
    "ghs_",
    "github_pat_",
    "xox",
    "vivi_",
    "AKIA",
];

const HIDDEN: &str = "[redacted]";

fn secret_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SECRET_NAMES.iter().any(|word| lower.contains(word))
}

fn secret_shaped(token: &str) -> bool {
    let bare = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-');
    bare.len() >= 12
        && (SECRET_PREFIXES
            .iter()
            .any(|prefix| bare.starts_with(prefix))
            || (bare.starts_with("eyJ") && bare.len() >= 20))
}

/// What to show for one word, and whether the next word is the secret.
fn judge(token: &str) -> (String, bool) {
    let lower = token.to_ascii_lowercase();
    if lower == "bearer" || lower == "bearer:" {
        return (token.to_string(), true);
    }
    if let Some((name, value)) = token.split_once('=')
        && secret_name(name)
    {
        return if value.is_empty() {
            (token.to_string(), true)
        } else {
            (format!("{name}={HIDDEN}"), false)
        };
    }
    if token.ends_with(':') && secret_name(token) {
        return (token.to_string(), true);
    }
    if secret_shaped(token) {
        return (HIDDEN.to_string(), false);
    }
    (token.to_string(), false)
}

fn scrub_line(line: &str) -> String {
    let mut words = Vec::new();
    let mut hide_next = false;
    for token in line.split(' ') {
        if hide_next && !token.is_empty() {
            // `Authorization: Bearer <token>`: the scheme word is not the secret.
            let scheme = matches!(token.to_ascii_lowercase().as_str(), "bearer" | "basic");
            words.push(if scheme {
                token.to_string()
            } else {
                HIDDEN.to_string()
            });
            hide_next = scheme;
            continue;
        }
        let (shown, hide) = judge(token);
        words.push(shown);
        hide_next = hide;
    }
    words.join(" ")
}

/// `text` with the value of every secret-looking name, every bearer token and
/// every credential-shaped word replaced by `[redacted]`. A heuristic, not a
/// guarantee: the console shows what the store holds, and the store should
/// not hold secrets.
#[must_use]
pub fn redact(text: &str) -> String {
    text.lines().map(scrub_line).collect::<Vec<_>>().join("\n")
}

/// `text` cut to at most `max` bytes at a character boundary, keeping the
/// start or the end, with a marker line where it was cut.
#[must_use]
pub fn cap(text: &str, max: usize, keep: Keep) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    match keep {
        Keep::Head => {
            let mut end = max;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}\n[cut: the rest is not shown]", &text[..end])
        }
        Keep::Tail => {
            let mut start = text.len() - max;
            while !text.is_char_boundary(start) {
                start += 1;
            }
            format!("[cut: the start is not shown]\n{}", &text[start..])
        }
    }
}

/// A wrapped `<pre>` of `text`, scrubbed and capped.
#[must_use]
pub fn pre(text: &str, keep: Keep) -> Html {
    let shown = cap(&redact(text), BLOCK_MAX, keep);
    tag("pre", &[("class", "block")], &Html::text(&shown))
}

#[cfg(test)]
#[path = "text_block_test.rs"]
mod tests;
