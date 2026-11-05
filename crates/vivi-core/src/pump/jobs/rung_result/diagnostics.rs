//! Canonical compiler and clippy diagnostics used as comparison evidence.
use std::collections::BTreeSet;

/// Parse rustc text diagnostics and Cargo's structured compiler-message output.
/// `None` means diagnostic evidence was visibly incomplete or malformed.
pub fn parse(output: &str) -> Option<Vec<String>> {
    let clean = strip_ansi(output);
    let mut signatures = BTreeSet::new();
    let mut pending: Option<(Option<String>, String)> = None;
    let mut malformed = false;
    for line in clean.lines() {
        if line.starts_with('{')
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(line)
            && value["reason"] == "compiler-message"
        {
            let message = &value["message"];
            if matches!(message["level"].as_str(), Some("error" | "warning")) {
                let text = message["message"].as_str()?.trim();
                let code = message["code"]["code"].as_str().map(str::to_owned);
                let spans = message["spans"].as_array()?;
                let span = spans
                    .iter()
                    .find(|s| s["is_primary"] == true)
                    .or_else(|| spans.first())?;
                signatures.insert(signature(
                    code,
                    format!(
                        "{}:{}:{}",
                        span["file_name"].as_str()?,
                        span["line_start"].as_u64()?,
                        span["column_start"].as_u64()?
                    )
                    .as_str(),
                    text,
                ));
            }
        }
        let line = line.trim();
        if let Some(rest) = line
            .strip_prefix("error")
            .or_else(|| line.strip_prefix("warning"))
            && (rest.starts_with('[') || rest.starts_with(':'))
        {
            if let Some((Some(_), _)) = pending.take() {
                malformed = true;
            }
            let rest = rest.trim_start_matches(':').trim();
            let (code, text) = if let Some(rest) = rest.strip_prefix('[') {
                let Some((code, text)) = rest.split_once(']') else {
                    malformed = true;
                    continue;
                };
                (Some(code.to_owned()), text.trim().to_owned())
            } else {
                (None, rest.to_owned())
            };
            pending = Some((code, text));
        } else if line.starts_with("-->") {
            if let Some((code, text)) = pending.take() {
                signatures.insert(signature(
                    code,
                    line.trim_start_matches("-->").trim(),
                    &text,
                ));
            }
        } else if let Some((Some(_), _)) = pending.as_ref()
            && !line.starts_with('|')
            && !line.is_empty()
            && !line.starts_with('^')
            && !line.starts_with("= ")
        {
            // A new unrecognized line ended a code-bearing block without location.
            malformed = true;
        }
    }
    if let Some((Some(_), _)) = pending {
        malformed = true;
    }
    if malformed {
        None
    } else {
        Some(signatures.into_iter().collect())
    }
}

/// Whether a candidate introduces clippy diagnostic evidence.
#[must_use]
pub fn intake_clippy_diagnostic_difference(baseline: &[String], candidate: &[String]) -> bool {
    let old: BTreeSet<_> = baseline.iter().collect();
    candidate.iter().any(|item| !old.contains(item))
}

fn signature(code: Option<String>, location: &str, text: &str) -> String {
    format!(
        "{}\0{}\0{}",
        code.unwrap_or_default(),
        normalize_location(location),
        text.trim()
    )
}

fn normalize_location(location: &str) -> String {
    let path = location.replace('\\', "/");
    let relative = ["/crates/", "/src/"]
        .iter()
        .find_map(|marker| path.find(marker).map(|i| &path[i + 1..]))
        .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(&path));
    relative.to_owned()
}

fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
#[path = "diagnostics_test.rs"]
mod tests;
