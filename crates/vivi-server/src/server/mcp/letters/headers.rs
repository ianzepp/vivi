//! Text helpers for letters: RFC 3339 and Date header times, addresses and
//! domains, header splitting, unfolding and editing, reply subjects, and
//! hex. Split out of `letters.rs` to keep it under the file-length budget.

#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn rfc3339_micros(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value.trim())
        .ok()
        .map(|time| time.timestamp_micros())
}

/// A `Date` header as Unix microseconds, in whole seconds.
pub(super) fn date_micros(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc2822(value.trim())
        .ok()
        .map(|time| time.timestamp().saturating_mul(1_000_000))
}

pub(super) fn email_address(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if let Some(start) = trimmed.find('<')
        && let Some(end) = trimmed[start + 1..].find('>')
    {
        return Some(trimmed[start + 1..start + 1 + end].to_owned());
    }
    trimmed.contains('@').then(|| trimmed.to_owned())
}

pub(super) fn address_domain(raw: &str) -> Option<String> {
    let address = email_address(raw)?;
    address
        .rsplit_once('@')
        .map(|(_, domain)| domain.trim().to_ascii_lowercase())
}

pub(super) fn split_addresses(raw: &str) -> Vec<String> {
    raw.split(',')
        .filter_map(|part| email_address(part))
        .collect()
}

pub(super) fn split_headers(text: &str) -> (&str, &str) {
    if let Some(pos) = text.find("\r\n\r\n") {
        return (&text[..pos], &text[pos + 4..]);
    }
    if let Some(pos) = text.find("\n\n") {
        return (&text[..pos], &text[pos + 2..]);
    }
    (text, "")
}

pub(super) fn folded_headers(header_text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut name = String::new();
    let mut value = String::new();
    for line in header_text.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            value.push(' ');
            value.push_str(line.trim());
            continue;
        }
        if !name.is_empty() {
            out.push((std::mem::take(&mut name), value.trim().to_owned()));
        }
        match line.split_once(':') {
            Some((n, v)) => {
                name = n.trim().to_owned();
                value = v.trim().to_owned();
            }
            None => {
                name.clear();
                value.clear();
            }
        }
    }
    if !name.is_empty() {
        out.push((name, value.trim().to_owned()));
    }
    out
}

pub(super) fn add_header(eml: &str, name: &str, value: &str) -> Result<String, String> {
    let newline = if eml.contains("\r\n") { "\r\n" } else { "\n" };
    let separator = format!("{newline}{newline}");
    let (headers, body) = eml
        .split_once(&separator)
        .ok_or_else(|| "composed letter has no header/body separator".to_owned())?;
    Ok(format!(
        "{headers}{newline}{name}: {value}{separator}{body}"
    ))
}

pub(super) fn set_header(eml: &str, name: &str, value: &str) -> Result<String, String> {
    let newline = if eml.contains("\r\n") { "\r\n" } else { "\n" };
    let separator = format!("{newline}{newline}");
    let (headers, body) = eml
        .split_once(&separator)
        .ok_or_else(|| "composed letter has no header/body separator".to_owned())?;
    let needle = format!("{name}:");
    let mut replaced = false;
    let mut out = String::new();
    for line in headers.lines() {
        if !replaced
            && line.len() >= needle.len()
            && line[..needle.len()].eq_ignore_ascii_case(&needle)
        {
            if !out.is_empty() {
                out.push_str(newline);
            }
            out.push_str(name);
            out.push_str(": ");
            out.push_str(value);
            replaced = true;
        } else {
            if !out.is_empty() {
                out.push_str(newline);
            }
            out.push_str(line);
        }
    }
    if !replaced {
        out.push_str(newline);
        out.push_str(name);
        out.push_str(": ");
        out.push_str(value);
    }
    Ok(format!("{out}{separator}{body}"))
}

pub(super) fn reply_subject(subject: &str) -> String {
    let trimmed = subject.trim();
    if trimmed.is_empty() {
        "Re: (no subject)".to_owned()
    } else if trimmed.len() >= 3 && trimmed[..3].eq_ignore_ascii_case("re:") {
        trimmed.to_owned()
    } else {
        format!("Re: {trimmed}")
    }
}

pub(super) fn push_unique(list: &mut Vec<String>, value: String) {
    if !value.is_empty() && !list.iter().any(|seen| seen.eq_ignore_ascii_case(&value)) {
        list.push(value);
    }
}

pub(super) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}
