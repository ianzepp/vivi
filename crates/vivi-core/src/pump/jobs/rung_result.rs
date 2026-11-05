//! Reading a rung job's result from its pool receipt.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::pool::record::{SECTION_RECEIPT, SECTION_STDERR, split_sections};
use crate::pump::attempt_record::agent_exit;
use crate::pump::jobs::{JobStatus, PoolJob};

/// Most lines of receipt output a result keeps.
const TAIL_LINES: usize = 60;
/// Most bytes of receipt output a result keeps.
const TAIL_BYTES: usize = 4096;

/// What a finished rung command reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RungResult {
    /// The command exited 0.
    pub pass: bool,
    /// The command's exit code; `None` when the job never reported one.
    pub exit: Option<i32>,
    /// The end of the receipt section followed by the stderr tail section of
    /// the posted reply, capped by lines and bytes. A reply with no sections
    /// is tailed whole.
    pub tail: String,
    /// A bounded excerpt of the failure evidence, selected whole from the
    /// complete collected reply before any limit: context windows around
    /// failure markers in the receipt and stderr sections, or the final
    /// lines of those sections when no marker matches. Evidence for a
    /// reader; never a parse input.
    pub excerpt: FailureExcerpt,
    /// Canonical ids of the tests the output named as failing, read from
    /// cargo's own output and never from model text: `<binary>::<test path>`,
    /// or just `<test path>` when the binary is unknown. The binary is the
    /// deps file stem without its trailing `-<hex hash>`, with `-` replaced by
    /// `_`, so every spelling of one crate gives one id.
    ///
    /// Consumer contract (N6 main-rung red detection, which compares failing
    /// sets between runs, and N7 repair-flow failing-test identity): compare
    /// sets of these ids only. An empty set with `pass == false` means the
    /// output named no failing test: a build error, a timeout, a lost job, or
    /// output cut by the 256 KiB head capture of agent.out
    /// (`src/pool/collect.rs`). An empty set is never green and never "all
    /// fixed".
    pub failing: BTreeSet<String>,
}

/// Read a rung job's result. A timed-out or lost job, or a receipt with no
/// agent exit, does not pass and carries no exit code.
#[must_use]
pub fn parse(job: &PoolJob) -> RungResult {
    let receipt = job.outcome.as_ref().map_or("", |o| o.receipt.as_str());
    let text = failure_text(receipt);
    let tail = tail_of(&text);
    let exit = match job.status {
        JobStatus::Finished => agent_exit(receipt),
        _ => None,
    };
    RungResult {
        pass: exit == Some(0),
        exit,
        tail,
        excerpt: failure_excerpt(receipt),
        failing: failing_ids(&text),
    }
}

/// The exception kind the flow raises for an environment failure: the worker
/// lacks a tool or its host setup failed, so the job says nothing about the
/// code under test. Raised once per subject, never retried.
pub const ENV_FAILURE: &str = "flow-env-failure";

/// Longest evidence line an environment failure keeps, in characters.
const EVIDENCE_CHARS: usize = 200;

/// What kind of environment failure a job hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvKind {
    /// A command the job needs is not installed on the worker.
    ToolMissing,
    /// Git or the checkout, fetch or target setup failed on the host.
    HostSetup,
}

/// A job that failed because of its worker, read from machine facts only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvFailure {
    /// Which kind of environment failure.
    pub kind: EnvKind,
    /// The missing command, when the facts name one.
    pub tool: Option<String>,
    /// One line of evidence, at most 200 characters.
    pub evidence: String,
}

/// Read an environment failure from a job's machine facts: the agent exit
/// code, the runner facts (`envfail::classify`) and shell markers in the
/// receipt's stderr tail. Model text is never read. `None` when the job has
/// no outcome, succeeded, or failed for another reason. The first rule that
/// matches wins.
#[must_use]
pub fn env_failure(job: &PoolJob) -> Option<EnvFailure> {
    let outcome = job.outcome.as_ref()?;
    if outcome.agent_success() == Some(true) {
        return None;
    }
    let stderr = stderr_marker(&outcome.receipt);
    let tool_of = |from_facts: Option<&str>| {
        from_facts
            .map(str::to_owned)
            .or_else(|| stderr.as_ref().and_then(|(tool, _)| tool.clone()))
    };
    if let Some(code @ (126 | 127)) = parse(job).exit {
        let evidence = match &stderr {
            Some((_, line)) => line.clone(),
            None => format!("agent exit {code}"),
        };
        return Some(env(EnvKind::ToolMissing, tool_of(None), &evidence));
    }
    if let Some(host) = outcome.host_failure() {
        match host.class.as_str() {
            "agent_exit" | "facts_exit" => {
                let tool = tool_of(host.command.as_deref());
                return Some(env(EnvKind::ToolMissing, tool, &host.detail));
            }
            "shell_marker" => {
                let missing = host.detail.contains("command not found");
                let kind = if missing {
                    EnvKind::ToolMissing
                } else {
                    EnvKind::HostSetup
                };
                let tool = if missing { tool_of(None) } else { None };
                return Some(env(kind, tool, &host.detail));
            }
            "push_outcome" => return Some(env(EnvKind::HostSetup, None, &host.detail)),
            _ => {}
        }
    }
    stderr.map(|(tool, line)| env(EnvKind::ToolMissing, tool, &line))
}

/// An [`EnvFailure`] of `kind` with the first line of `evidence`, capped.
pub(crate) fn env(kind: EnvKind, tool: Option<String>, evidence: &str) -> EnvFailure {
    let line = evidence.lines().next().unwrap_or_default().trim();
    EnvFailure {
        kind,
        tool,
        evidence: line.chars().take(EVIDENCE_CHARS).collect(),
    }
}

/// The first missing-tool line in the pool's stderr tail section: the tool
/// (when the line names it) and the line. Reads that section only, never the
/// facts, the receipt, or the runner log; an unknown heading inside the
/// section stays content, so evidence after it is still read.
fn stderr_marker(receipt: &str) -> Option<(Option<String>, String)> {
    let (_, mut sections) = split_sections(receipt);
    let body = sections
        .find(|(title, _)| *title == SECTION_STDERR)
        .map(|(_, body)| body)?;
    for line in body.lines().map(str::trim) {
        if let Some(tool) = missing_tool_of(line) {
            return Some((Some(tool), line.to_owned()));
        }
        if line.contains("command not found") {
            return Some((None, line.to_owned()));
        }
    }
    None
}

/// The tool of `<shell>: <tool>: command not found` or the dash form
/// `sh: <n>: <tool>: not found`.
fn missing_tool_of(line: &str) -> Option<String> {
    let tool = if let Some(head) = line.strip_suffix(": command not found") {
        head.rsplit(": ").next()?
    } else {
        let head = line.strip_prefix("sh: ")?.strip_suffix(": not found")?;
        let (number, tool) = head.split_once(": ")?;
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        tool
    };
    let tool = tool.trim();
    (!tool.is_empty() && !tool.contains(char::is_whitespace)).then(|| tool.to_owned())
}

/// The binary a cargo `Running` or `Doc-tests` line names, canonical; the
/// outer `None` for any other line.
fn binary_line(line: &str) -> Option<Option<String>> {
    let line = line.trim();
    if let Some(name) = line.strip_prefix("Doc-tests ") {
        return Some(Some(canonical(name.trim())));
    }
    if !line.starts_with("Running ") {
        return None;
    }
    let inner = line.strip_suffix(')').and_then(|l| l.rsplit_once('('));
    let stem = inner.and_then(|(_, path)| path.rsplit('/').next());
    Some(stem.filter(|s| !s.is_empty()).map(canonical))
}

/// A deps file stem without its trailing `-<hex hash>`, `-` replaced by `_`.
fn canonical(stem: &str) -> String {
    let bare = match stem.rsplit_once('-') {
        Some((head, hash)) if hash.len() >= 8 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => {
            head
        }
        _ => stem,
    };
    bare.replace('-', "_")
}

/// The binary named by a cargo `to rerun pass` footer, canonical.
fn footer_binary(line: &str) -> Option<String> {
    let command = line.split('`').nth(1)?;
    let words: Vec<&str> = command.split_whitespace().collect();
    let after = |flags: &[&str]| {
        words
            .iter()
            .position(|w| flags.contains(w))
            .and_then(|i| words.get(i + 1))
            .map(|w| canonical(w))
    };
    after(&["--test", "--bin"]).or_else(|| after(&["-p", "--package"]))
}

/// The test name of a libtest `... FAILED` line or a JSON failed event.
fn failed_line(line: &str) -> Option<String> {
    if let Some(rest) = line.strip_prefix("test ")
        && let Some(name) = rest.strip_suffix(" ... FAILED")
    {
        return Some(name.trim().to_owned());
    }
    if !line.starts_with('{') {
        return None;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return None;
    };
    if value["type"] != "test" || value["event"] != "failed" {
        return None;
    }
    value["name"].as_str().map(str::to_owned)
}

/// Where the parser is within a libtest `failures:` list.
#[derive(Clone, Copy, PartialEq)]
enum Block {
    None,
    Header,
    Names,
}

/// Canonical ids of the failing tests named in cargo output.
fn failing_ids(text: &str) -> BTreeSet<String> {
    let mut binary: Option<String> = None;
    let mut found: BTreeSet<(Option<String>, String)> = BTreeSet::new();
    let mut footers: BTreeSet<Option<String>> = BTreeSet::new();
    let mut state = Block::None;
    for line in text.lines() {
        if let Some(named) = binary_line(line) {
            binary = named;
            state = Block::None;
            continue;
        }
        if line.contains("to rerun pass `") {
            footers.insert(footer_binary(line));
        }
        if let Some(name) = failed_line(line) {
            found.insert((binary.clone(), name));
            continue;
        }
        let listed = state != Block::None && line.starts_with("    ") && !line.trim().is_empty();
        if listed {
            found.insert((binary.clone(), line.trim().to_owned()));
        }
        state = if line == "failures:" {
            Block::Header
        } else if listed {
            Block::Names
        } else if state == Block::Header && line.is_empty() {
            Block::Header
        } else {
            Block::None
        };
    }
    let lone = if footers.len() == 1 {
        footers.into_iter().next().flatten()
    } else {
        None
    };
    found
        .into_iter()
        .map(|(own, name)| match own.or_else(|| lone.clone()) {
            Some(bin) => format!("{bin}::{name}"),
            None => name,
        })
        .collect()
}

/// The receipt and stderr sections of a posted reply, joined in their
/// rendered order; the whole text when no rendered section exists. Unknown
/// and non-forward headings inside a selected section stay content and never
/// truncate it; a sectioned reply with only facts or a runner log is empty.
fn failure_text(text: &str) -> String {
    let (head, sections) = split_sections(text);
    let mut kept: Vec<&str> = Vec::new();
    let mut sectioned = false;
    for (title, body) in sections {
        sectioned = true;
        if title == SECTION_RECEIPT || title == SECTION_STDERR {
            kept.push(body);
        }
    }
    if !sectioned {
        return head.to_owned();
    }
    kept.join("\n")
}

fn tail_of(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let kept = lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n");
    if kept.len() <= TAIL_BYTES {
        return kept;
    }
    let mut start = kept.len() - TAIL_BYTES;
    while !kept.is_char_boundary(start) {
        start += 1;
    }
    kept[start..].to_owned()
}

/// Source lines of context kept around each failure-marker line.
const EXCERPT_CONTEXT: usize = 3;
/// Most source lines a failure excerpt keeps.
const EXCERPT_LINES: usize = 120;
/// Most bytes a failure excerpt keeps.
const EXCERPT_BYTES: usize = 12 * 1024;
/// Most source lines the markerless fallback excerpt keeps.
const EXCERPT_TAIL_LINES: usize = 40;

/// A bounded excerpt of the failure evidence in a rung receipt: either the
/// source lines around failure markers or, when no marker matches, the final
/// lines of the receipt and stderr sections. Evidence for a reader; never a
/// parse input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureExcerpt {
    /// How the content was chosen.
    pub mode: ExcerptMode,
    /// The selected source lines in source order, each omitted stretch named
    /// between the lines around it; empty when there is no content.
    pub text: String,
    /// Source lines the selection searched.
    pub source_lines: usize,
    /// Source lines the excerpt keeps, earliest evidence first.
    pub kept_lines: usize,
    /// Source lines left out, by the marker windows or by a cap.
    pub omitted_lines: usize,
    /// True when the line or byte cap cut content the selection would
    /// otherwise have kept.
    pub truncated: bool,
}

/// How a failure excerpt chose its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExcerptMode {
    /// Windows of context around failure-marker lines.
    Markers,
    /// The final lines, when no marker matched.
    Tail,
    /// No content: the receipt and stderr sections were empty.
    Empty,
}

/// Select a bounded excerpt of a receipt's failure evidence. The receipt and
/// stderr sections ([`failure_text`]) are searched whole for failure markers
/// before any limit applies; each marker keeps [`EXCERPT_CONTEXT`] source
/// lines on both sides, overlapping or touching windows merge, and the kept
/// lines stay in source order deduplicated by position. Markers are
/// case-insensitive error, FAILED (covering the `test result: FAILED`
/// footer), panicked, `failures:`, offender, and recognized nonzero exit
/// status lines; a zero exit alone is never a marker. When no marker
/// matches, the final [`EXCERPT_TAIL_LINES`] lines stand in. Either
/// selection is capped at [`EXCERPT_LINES`] source lines and
/// [`EXCERPT_BYTES`], keeping the earliest evidence and cutting at UTF-8
/// boundaries. Selection shares nothing with [`failing_ids`] or
/// [`tail_of`].
#[must_use]
pub fn failure_excerpt(receipt: &str) -> FailureExcerpt {
    let text = failure_text(receipt);
    let lines: Vec<&str> = text.lines().collect();
    let source_lines = lines.len();
    let windows = marker_windows(&lines);
    let (text, kept_lines, truncated) = if source_lines == 0 {
        (String::new(), 0, false)
    } else if windows.is_empty() {
        tail_excerpt(&lines)
    } else {
        window_excerpt(&lines, &windows)
    };
    let mode = if source_lines == 0 {
        ExcerptMode::Empty
    } else if windows.is_empty() {
        ExcerptMode::Tail
    } else {
        ExcerptMode::Markers
    };
    FailureExcerpt {
        mode,
        text,
        source_lines,
        kept_lines,
        omitted_lines: source_lines - kept_lines,
        truncated,
    }
}

/// Inclusive source-line ranges around each failure-marker line, merged when
/// they overlap or touch, in source order.
fn marker_windows(lines: &[&str]) -> Vec<(usize, usize)> {
    let mut windows: Vec<(usize, usize)> = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        if !is_marker(line) {
            continue;
        }
        let window = (
            at.saturating_sub(EXCERPT_CONTEXT),
            (at + EXCERPT_CONTEXT).min(lines.len() - 1),
        );
        match windows.last_mut() {
            Some(last) if window.0 <= last.1 + 1 => last.1 = last.1.max(window.1),
            _ => windows.push(window),
        }
    }
    windows
}

/// True when a source line carries failure evidence: a case-insensitive
/// error, FAILED, panicked, `failures:` or offender marker, or a recognized
/// nonzero exit status line.
fn is_marker(line: &str) -> bool {
    let lower = line.to_lowercase();
    ["error", "failed", "panicked", "failures:", "offender"]
        .iter()
        .any(|marker| lower.contains(marker))
        || reported_exit(&lower).is_some()
}

/// A nonzero exit code the line reports after a recognized keyword:
/// `exit code`, `exit status`, `agent exit`, `exited with` or `exited`.
fn reported_exit(lower: &str) -> Option<u32> {
    for keyword in [
        "exit code",
        "exit status",
        "agent exit",
        "exited with",
        "exited",
    ] {
        let mut rest = lower;
        while let Some(at) = rest.find(keyword) {
            let after = &rest[at + keyword.len()..];
            if let Some(code) = leading_code(after)
                && code != 0
            {
                return Some(code);
            }
            rest = after;
        }
    }
    None
}

/// The integer a recognized exit keyword is directly followed by, skipping
/// spaces, tabs and a colon.
fn leading_code(after: &str) -> Option<u32> {
    let digits: String = after
        .trim_start_matches([' ', '\t', ':'])
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    let Ok(code) = digits.parse() else {
        return None;
    };
    Some(code)
}

/// The longest prefix of `line` within `max` bytes that ends at a UTF-8
/// boundary.
fn byte_prefix(line: &str, max: usize) -> &str {
    let mut end = max.min(line.len());
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    &line[..end]
}

/// Append `content` as a line of `text` when it fits the excerpt byte cap
/// whole; false when it does not, leaving `text` untouched.
fn push_line(text: &mut String, content: &str) -> bool {
    if text.len() + content.len() + usize::from(!text.is_empty()) > EXCERPT_BYTES {
        return false;
    }
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(content);
    true
}

/// The line naming a stretch of omitted source lines.
fn gap_marker(gap: usize) -> String {
    format!("... {gap} source lines omitted ...")
}

/// Join the windowed source lines in source order under the excerpt caps:
/// the earliest evidence survives, every omitted stretch is named between
/// its neighbors, and the line where the bytes run out is cut at a UTF-8
/// boundary. Returns the text, the source lines it keeps, and whether a cap
/// cut content the windows would have kept.
fn window_excerpt(lines: &[&str], windows: &[(usize, usize)]) -> (String, usize, bool) {
    let mut text = String::new();
    let mut kept = 0usize;
    let mut next = 0usize;
    let mut truncated = false;
    for &(start, end) in windows {
        let gap = start - next;
        if gap > 0 && !push_line(&mut text, &gap_marker(gap)) {
            truncated = true;
            break;
        }
        for line in &lines[start..=end] {
            if kept == EXCERPT_LINES {
                truncated = true;
                break;
            }
            if push_line(&mut text, line) {
                kept += 1;
                continue;
            }
            let spare = EXCERPT_BYTES.saturating_sub(text.len() + usize::from(!text.is_empty()));
            let cut = byte_prefix(line, spare);
            if !cut.is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(cut);
                kept += 1;
            }
            truncated = true;
            break;
        }
        if truncated {
            break;
        }
        next = end + 1;
    }
    (text, kept, truncated)
}

/// The markerless fallback: the final [`EXCERPT_TAIL_LINES`] source lines,
/// byte-capped with the earliest bytes cut at a UTF-8 boundary. Returns the
/// text, the source lines it keeps, and whether a cap cut content.
fn tail_excerpt(lines: &[&str]) -> (String, usize, bool) {
    let whole = lines.len().min(EXCERPT_TAIL_LINES);
    let mut text = lines[lines.len() - whole..].join("\n");
    let mut truncated = whole < lines.len();
    if text.len() > EXCERPT_BYTES {
        let mut start = text.len() - EXCERPT_BYTES;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        text = text[start..].to_owned();
        truncated = true;
    }
    let kept = if text.is_empty() {
        0
    } else {
        text.split('\n').count()
    };
    (text, kept, truncated)
}

#[cfg(test)]
#[path = "rung_result_test.rs"]
mod tests;
