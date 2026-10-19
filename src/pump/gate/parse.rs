//! Reading the failing-test names out of a gate reply.
//!
//! The marker block, the `BUILD-FAILED` line and the `<crate> <path>` fallback
//! live here so `gate.rs` keeps room for the gate cycle itself.

use std::collections::BTreeSet;

/// The `FAILING-BEGIN` reply marker.
pub(crate) const BEGIN: &str = "FAILING-BEGIN";
/// The `FAILING-END` reply marker.
pub(crate) const END: &str = "FAILING-END";
/// The single line a gate worker replies with when the build itself broke.
pub(crate) const BUILD_FAILED: &str = "BUILD-FAILED";

/// Read the failing-test names from a gate reply.
///
/// The last `FAILING-BEGIN ... FAILING-END` block that is usable supplies the
/// names: an entirely empty block means zero failures, and `<`-prefixed
/// template lines are skipped. With no usable block and no `BUILD-FAILED` in
/// the text, the fallback reads `<crate> <path>` pairs (crate at least two
/// `[a-z0-9_-]` characters, path two or more `::`-joined segments), ignoring
/// surrounding punctuation, deduplicated and sorted. `Some(["BUILD-FAILED"])`
/// comes from a block holding exactly that line; `None` means unreadable.
#[must_use]
pub fn parse_failing(text: &str) -> Option<Vec<String>> {
    if let Some(names) = last_usable_block(text) {
        return Some(names);
    }
    if text.contains(BUILD_FAILED) {
        return None;
    }
    let pairs = crate_path_pairs(text);
    if pairs.is_empty() { None } else { Some(pairs) }
}

/// Every `FAILING-BEGIN ... FAILING-END` block body, in order: the port of
/// `FAILING-BEGIN\s*\n(.*?)\n?\s*FAILING-END` (dotall, non-greedy). A marker
/// followed by no newline starts no block; a `BEGIN` with no later `END` ends
/// the scan.
pub(crate) fn marker_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(BEGIN) {
        let after = &rest[at + BEGIN.len()..];
        let ws = after.len() - after.trim_start().len();
        if !after[..ws].contains('\n') {
            rest = after;
            continue;
        }
        let body = &after[ws..];
        let Some(end) = body.find(END) else {
            break;
        };
        blocks.push(body[..end].to_owned());
        rest = &body[end + END.len()..];
    }
    blocks
}

/// The last block with a usable line — non-empty and not a `<template>` — or
/// an entirely empty block, which yields the empty name list.
pub(crate) fn last_usable_block(text: &str) -> Option<Vec<String>> {
    for block in marker_blocks(text).iter().rev() {
        let lines: Vec<String> = block
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('<'))
            .map(String::from)
            .collect();
        if !lines.is_empty() || block.trim().is_empty() {
            return Some(lines);
        }
    }
    None
}

/// `true` for path characters: `[A-Za-z0-9_]`.
pub(crate) fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `true` for crate characters: `[a-z0-9_-]`.
pub(crate) fn is_crate_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-'
}

/// The fallback: every `<crate> <path>` pair where the crate
/// starts on a word boundary and is at least two `[a-z0-9_-]` characters, the
/// path is two or more `::`-joined `[A-Za-z0-9_]+` segments, and punctuation
/// around the tokens (`\b` on both ends) does not count. Deduplicated and
/// sorted as `"<crate> <path>"`.
pub(crate) fn crate_path_pairs(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut found = BTreeSet::new();
    for i in 0..bytes.len() {
        if !bytes[i].is_ascii_lowercase() || (i > 0 && is_word_byte(bytes[i - 1])) {
            continue;
        }
        let Some(space) = bytes[i..].iter().position(|b| !is_crate_byte(*b)) else {
            continue;
        };
        let crate_end = i + space;
        if space < 2 || bytes[crate_end] != b' ' {
            continue;
        }
        let Some(path_end) = parse_path(bytes, crate_end + 1) else {
            continue;
        };
        if path_end < bytes.len() && is_word_byte(bytes[path_end]) {
            continue;
        }
        found.insert(format!(
            "{} {}",
            &text[i..crate_end],
            &text[crate_end + 1..path_end]
        ));
    }
    found.into_iter().collect()
}

/// Longest `(?:[A-Za-z0-9_]+::)+[A-Za-z0-9_]+` at `start`: the end offset of
/// the path, or `None` for fewer than two `::`-joined segments. A trailing
/// `::` with no following segment backs off to the shorter path.
pub(crate) fn parse_path(bytes: &[u8], start: usize) -> Option<usize> {
    let mut at = start;
    let mut leading = 0;
    let mut after_last = None;
    loop {
        let seg = at;
        while at < bytes.len() && is_word_byte(bytes[at]) {
            at += 1;
        }
        if at == seg {
            break;
        }
        if bytes[at..].starts_with(b"::") {
            after_last = Some(at);
            at += 2;
            leading += 1;
        } else {
            return if leading >= 1 { Some(at) } else { None };
        }
    }
    if leading >= 2 { after_last } else { None }
}
