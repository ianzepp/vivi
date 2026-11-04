//! Pool slot counting and the task-handle parser used by the job dispatcher.

use std::collections::BTreeMap;

use crate::pump::host::Ctx;

/// Free slot counts per pool: run `pool status --porcelain` and count the
/// lines with at least five whitespace tokens whose first is `slot` and fifth
/// is `free`, keyed by the pool named in the second token, plus the number on
/// each `capacity POOL FREE` line (pools whose slots exist only per dispatch).
#[must_use]
pub fn slots(ctx: &Ctx) -> BTreeMap<String, u32> {
    let out = ctx.vivi("pool status --porcelain", 120);
    let mut free = BTreeMap::new();
    for line in out.text.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() >= 5 && tokens[0] == "slot" && tokens[4] == "free" {
            *free.entry(tokens[1].to_owned()).or_insert(0) += 1;
        }
        // A per-dispatch pool (worker, RunPod) holds no resting slot; its
        // `capacity` line says how many jobs it may start. A zero is not
        // recorded, so such a pool counts as full like any other.
        if tokens.len() >= 3 && tokens[0] == "capacity" {
            if let Ok(more @ 1..) = tokens[2].parse::<u32>() {
                *free.entry(tokens[1].to_owned()).or_insert(0) += more;
            }
        }
    }
    free
}

/// The first `node`/`created` handle in `out`: a line whose trimmed form
/// starts with the keyword and whose last token is exactly 8 lowercase hex
/// characters, with at most one token between keyword and handle. Ports the
/// regex `^(?:node|created)\s+(?:\S+\s+)?([0-9a-f]{8})\s*$` in multiline mode.
#[must_use]
pub fn parse_created_handle(out: &str) -> Option<String> {
    for line in out.lines() {
        let trimmed = line.trim();
        let rest = match trimmed
            .strip_prefix("node")
            .or_else(|| trimmed.strip_prefix("created"))
        {
            Some(rest) if rest.is_empty() || rest.starts_with(char::is_whitespace) => rest,
            _ => continue,
        };
        let tokens: Vec<&str> = rest.split_whitespace().collect();
        let handle = match tokens.as_slice() {
            [handle] => *handle,
            [_between, handle] => *handle,
            _ => continue,
        };
        if is_hex8(handle) {
            return Some(handle.to_owned());
        }
    }
    None
}

/// `true` when `s` is exactly 8 lowercase hex characters.
fn is_hex8(s: &str) -> bool {
    s.len() == 8 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
#[path = "slots_test.rs"]
mod tests;
