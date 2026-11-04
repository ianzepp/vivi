//! Plain-text body extraction from a raw `.eml`.

use mail_parser::MessageParser;

use vivi_error::VivariumError;

/// Extract the plain-text body from raw `.eml` bytes.
///
/// Prefers a text part, falls back to stripping tags from an HTML part, and
/// returns an empty string when the message carries no usable body.
///
/// # Errors
/// Returns an error if the bytes cannot be parsed as an email message.
pub fn extract_text(data: &[u8]) -> Result<String, VivariumError> {
    let parsed = MessageParser::default()
        .parse(data)
        .ok_or_else(|| VivariumError::Parse("failed to parse email for extraction".into()))?;

    if let Some(body) = parsed.body_text(0) {
        let body = body.trim();
        if !body.is_empty() {
            return Ok(body.to_string());
        }
    }

    if let Some(html) = parsed.body_html(0) {
        let text = html_to_text(html.as_ref());
        if !text.trim().is_empty() {
            return Ok(text.trim().to_string());
        }
    }

    Ok(parsed
        .body_text(0)
        .map_or_else(String::new, |body| body.into_owned()))
}

/// Strip HTML tags, collapsing runs of newlines.
fn html_to_text(html: &str) -> String {
    let mut result = String::new();
    let mut in_tag = false;
    let mut prev_was_newline = false;

    for ch in html.chars() {
        if ch == '<' {
            in_tag = true;
            continue;
        }
        if ch == '>' {
            in_tag = false;
            continue;
        }
        if !in_tag {
            if ch == '\n' || ch == '\r' {
                if !prev_was_newline {
                    result.push('\n');
                    prev_was_newline = true;
                }
            } else {
                result.push(ch);
                prev_was_newline = false;
            }
        }
    }

    result
}
