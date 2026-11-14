//! The one JSON shape the pump store keeps in text columns: an array of
//! strings (a slice's write scope, its scope holders, its dependencies). The
//! console has no JSON parser, so this reads exactly that shape and nothing
//! else.

/// The strings of a JSON array of strings. Anything that is not such an
/// array yields what could be read before the first thing that is not a
/// string.
#[must_use]
pub fn json_strings(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = text.trim().chars().peekable();
    if chars.next() != Some('[') {
        return out;
    }
    loop {
        match chars.next() {
            Some('"') => out.push(string_tail(&mut chars)),
            Some(',' | ' ' | '\n' | '\t' | '\r') => {}
            _ => return out,
        }
    }
}

fn string_tail(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut out = String::new();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => break,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('u') => out.push(unicode_escape(chars)),
                Some(other) => out.push(other),
                None => break,
            },
            other => out.push(other),
        }
    }
    out
}

fn unicode_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> char {
    let hex: String = (0..4).filter_map(|_| chars.next()).collect();
    match u32::from_str_radix(&hex, 16) {
        Ok(code) => char::from_u32(code).unwrap_or('\u{fffd}'),
        Err(_) => '\u{fffd}',
    }
}

#[cfg(test)]
#[path = "json_list_test.rs"]
mod tests;
