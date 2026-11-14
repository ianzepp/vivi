//! The text of a stored letter: its headers the console cares about and its
//! plain-text body, from the raw `.eml` bytes.
//!
//! The console has no MIME parser of its own beyond this: letters are
//! written by vivi's composer, so the shapes are few (a single text part,
//! or a `multipart` of text parts, in 7bit, quoted-printable or base64).
//! Anything else decodes lossily and still shows as text, never as markup.

/// What the thread page shows of one letter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Letter {
    /// The `X-Vivi-Kind` header, lowercased.
    pub kind: Option<String>,
    /// The `X-Vivi-Event` header, lowercased.
    pub event: Option<String>,
    /// The plain-text body with LF line ends. Not markdown-rendered.
    pub body: String,
}

/// Parse `raw` into its kind, event and text body.
#[must_use]
pub fn read_letter(raw: &[u8]) -> Letter {
    let (head, body) = split_head(raw);
    let headers = parse_headers(&head);
    let lookup = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.to_ascii_lowercase())
            .filter(|value| !value.is_empty())
    };
    Letter {
        kind: lookup("x-vivi-kind"),
        event: lookup("x-vivi-event"),
        body: text_of(&headers, body).unwrap_or_default(),
    }
}

/// The header block and the body, split at the first blank line.
fn split_head(raw: &[u8]) -> (String, &[u8]) {
    let mut start = 0;
    while start <= raw.len() {
        let end = raw[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(raw.len(), |found| start + found);
        let line = &raw[start..end];
        if line.is_empty() || line == b"\r" {
            let head = String::from_utf8_lossy(&raw[..start]).into_owned();
            return (head, raw.get(end + 1..).unwrap_or_default());
        }
        if end >= raw.len() {
            break;
        }
        start = end + 1;
    }
    (String::from_utf8_lossy(raw).into_owned(), &[])
}

/// Header names (lowercased) and unfolded values, in order.
fn parse_headers(head: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in head.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = out.last_mut() {
                if !value.is_empty() {
                    value.push(' ');
                }
                value.push_str(line.trim());
            }
        } else if let Some((name, value)) = line.split_once(':') {
            out.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    out
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// The text of one entity: a text part decoded, or the first text part of a
/// multipart, searched in order and preferring `text/plain`.
fn text_of(headers: &[(String, String)], body: &[u8]) -> Option<String> {
    let content_type = header(headers, "content-type").unwrap_or("text/plain");
    let media = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if media.starts_with("multipart/") {
        let boundary = parameter(content_type, "boundary")?;
        let parts = multipart_parts(body, &boundary);
        let texts: Vec<(String, String)> = parts
            .iter()
            .filter_map(|part| {
                let (head, inner) = split_head(part);
                let part_headers = parse_headers(&head);
                let part_media = header(&part_headers, "content-type")
                    .unwrap_or("text/plain")
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase();
                text_of(&part_headers, inner).map(|text| (part_media, text))
            })
            .collect();
        let plain = texts.iter().find(|(media, _)| media == "text/plain");
        return plain
            .or_else(|| texts.first())
            .map(|(_, text)| text.clone());
    }
    if !media.starts_with("text/") {
        return None;
    }
    let decoded = decode_transfer(header(headers, "content-transfer-encoding"), body);
    Some(
        String::from_utf8_lossy(&decoded)
            .replace("\r\n", "\n")
            .replace('\r', "\n"),
    )
}

/// A `;`-separated parameter of a header value, unquoted.
fn parameter(value: &str, name: &str) -> Option<String> {
    value.split(';').skip(1).find_map(|part| {
        let (key, found) = part.split_once('=')?;
        (key.trim().eq_ignore_ascii_case(name)).then(|| found.trim().trim_matches('"').to_string())
    })
}

/// The bodies of a multipart's parts, without their delimiters.
fn multipart_parts(body: &[u8], boundary: &str) -> Vec<Vec<u8>> {
    let open = format!("--{boundary}");
    let close = format!("--{boundary}--");
    let mut parts: Vec<Vec<u8>> = Vec::new();
    let mut current: Option<Vec<u8>> = None;
    for line in body.split_inclusive(|byte| *byte == b'\n') {
        let trimmed = std::str::from_utf8(line).unwrap_or_default().trim_end();
        if trimmed == close {
            parts.extend(current.take());
            break;
        }
        if trimmed == open {
            parts.extend(current.take());
            current = Some(Vec::new());
        } else if let Some(part) = current.as_mut() {
            part.extend_from_slice(line);
        }
    }
    parts.extend(current);
    parts
}

fn decode_transfer(encoding: Option<&str>, body: &[u8]) -> Vec<u8> {
    match encoding.map(str::to_ascii_lowercase).as_deref() {
        Some("base64") => decode_base64(body),
        Some("quoted-printable") => decode_quoted_printable(body),
        _ => body.to_vec(),
    }
}

fn decode_base64(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut bits: u32 = 0;
    let mut count = 0;
    for byte in input {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        };
        bits = (bits << 6) | u32::from(value);
        count += 1;
        if count == 4 {
            out.extend_from_slice(&[(bits >> 16) as u8, (bits >> 8) as u8, bits as u8]);
            bits = 0;
            count = 0;
        }
    }
    match count {
        2 => out.push((bits >> 4) as u8),
        3 => out.extend_from_slice(&[(bits >> 10) as u8, (bits >> 2) as u8]),
        _ => {}
    }
    out
}

fn decode_quoted_printable(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index] != b'=' {
            out.push(input[index]);
            index += 1;
            continue;
        }
        let rest = &input[index + 1..];
        if rest.starts_with(b"\r\n") {
            index += 3;
        } else if rest.starts_with(b"\n") {
            index += 2;
        } else if let Some(byte) = hex_pair(rest) {
            out.push(byte);
            index += 3;
        } else {
            out.push(b'=');
            index += 1;
        }
    }
    out
}

fn hex_pair(rest: &[u8]) -> Option<u8> {
    let digit = |byte: u8| char::from(byte).to_digit(16);
    let high = digit(*rest.first()?)?;
    let low = digit(*rest.get(1)?)?;
    // Two hex digits are at most 255.
    Some((high * 16 + low) as u8)
}

#[cfg(test)]
#[path = "mail_body_test.rs"]
mod tests;
