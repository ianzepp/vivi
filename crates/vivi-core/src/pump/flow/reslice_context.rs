//! Quoted historical context for a dispatched reslice brief: the original
//! slice task with its inherited check and execution instructions removed.

/// The heading the quoted original slice task is rendered under.
pub(crate) const CONTEXT_HEADING: &str =
    "## original slice task (quoted context, not instructions to execute)";

/// Command words that make a line an inherited execution instruction.
const COMMANDS: [&str; 10] = [
    "cargo", "clippy", "git", "make", "npm", "pnpm", "pytest", "rustc", "rustfmt", "yarn",
];

/// The prose of `task_body`, inherited execution and check instructions
/// removed, every retained line prefixed as a Markdown blockquote.
pub(crate) fn quoted_context(task_body: &str) -> String {
    let prose = vivi_mail::letter::prose_of(task_body);
    let lines: Vec<&str> = prose.lines().collect();
    let mut out = String::new();
    for line in retained(&lines) {
        if line.trim().is_empty() {
            out.push_str(">\n");
        } else {
            out.push_str("> ");
            out.push_str(line.trim_end());
            out.push('\n');
        }
    }
    out
}

/// Filter state across the prose lines of one historical task.
#[derive(Default)]
struct Filter {
    /// An open checks/self-check section: a heading or bare `Checks:` label
    /// and its command list.
    section: bool,
    /// A fenced block: `Some(true)` when its body is filtered out.
    fence: Option<bool>,
    /// The previous line was a dropped instruction whose command
    /// continuations follow.
    continuation: bool,
}

/// The lines of `lines` that are historical context, not instructions.
fn retained<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    let mut filter = Filter::default();
    let mut kept = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if filter.keep(lines, index, line) {
            kept.push(*line);
        }
    }
    kept
}

impl Filter {
    /// Whether `line` at `index` is kept, updating the filter state.
    fn keep(&mut self, lines: &[&str], index: usize, line: &str) -> bool {
        if let Some(drop) = self.fence {
            if is_fence(line) {
                self.fence = None;
            }
            return !drop;
        }
        if is_fence(line) {
            self.fence = Some(self.section || self.continuation);
            return self.fence == Some(false);
        }
        if let Some((_, title)) = heading(line) {
            self.section = is_check_label(&title);
            self.continuation = false;
            return !self.section;
        }
        if self.section {
            return self.keep_section(lines, index, line);
        }
        if inline_checks(line) || is_check_label(&label(line)) {
            self.continuation = true;
            return false;
        }
        if self_check_instruction(lines, index) {
            self.continuation = true;
            return false;
        }
        if self.continuation {
            if is_continuation(lines, index, line) {
                return false;
            }
            self.continuation = false;
        }
        !command_line(line)
    }

    /// Whether a line inside an open checks section is kept. A heading or a
    /// peer list section ends the section; blank lines stay inside it.
    fn keep_section(&mut self, lines: &[&str], index: usize, line: &str) -> bool {
        let next = next_non_blank(lines, index);
        let label_boundary = line.trim().is_empty()
            && next.is_some_and(|next| !command_line(next) && !list_item(next));
        if label_boundary || peer_section(line) {
            self.section = false;
            self.continuation = false;
            return true;
        }
        false
    }
}

/// A heading: its level and its normalized title text.
fn heading(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|c| *c == '#').count();
    if level == 0 || level > 6 || !trimmed[level..].starts_with(' ') {
        return None;
    }
    Some((level, normalize(&trimmed[level..])))
}

/// Lowercase `text` without emphasis marks or a trailing colon or hash.
fn normalize(text: &str) -> String {
    let plain: String = text
        .chars()
        .filter(|c| !matches!(c, '*' | '_' | '`'))
        .collect();
    plain
        .trim()
        .trim_end_matches([':', '#'])
        .trim()
        .to_lowercase()
}

/// Equals one of `text` without emphasis marks or a trailing colon, or not.
fn is_check_label(text: &str) -> bool {
    matches!(
        normalize(text).as_str(),
        "check"
            | "checks"
            | "self-check"
            | "self check"
            | "self-checks"
            | "self checks"
            | "selfcheck"
            | "selfchecks"
    )
}

/// The line without its list marker, trimmed at the start.
fn strip_marker(line: &str) -> &str {
    let mut rest = line.trim_start();
    for marker in ["- ", "* ", "+ "] {
        if let Some(after) = rest.strip_prefix(marker) {
            rest = after.trim_start();
            break;
        }
    }
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        if let Some(after) = rest[digits..].strip_prefix(". ") {
            rest = after.trim_start();
        }
    }
    rest
}

/// The normalized label of a line: its text with any list marker removed.
fn label(line: &str) -> String {
    normalize(strip_marker(line))
}

/// Whether `line` is a list item.
fn list_item(line: &str) -> bool {
    let trimmed = line.trim_start();
    ["- ", "* ", "+ "]
        .iter()
        .any(|marker| trimmed.starts_with(marker))
        || trimmed.split_once(". ").is_some_and(|(number, _)| {
            !number.is_empty() && number.chars().all(|c| c.is_ascii_digit())
        })
}

/// A list section that is not about checks: a list item that is neither a
/// command nor a check label, or a bold peer heading.
fn peer_section(line: &str) -> bool {
    let trimmed = line.trim();
    let bold = trimmed.starts_with("**") && trimmed.ends_with("**") && trimmed.len() > 4;
    if bold && !is_check_label(&label(trimmed)) {
        return true;
    }
    list_item(line) && !command_line(line) && !is_check_label(&label(line))
}

/// Whether `text` names a command word.
fn contains_command(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .any(|token| COMMANDS.contains(&token))
}

/// Whether `line` is a bare command line (or the fence of a shell block).
fn command_line(line: &str) -> bool {
    let text = strip_marker(line).trim();
    if is_fence(text) {
        return true;
    }
    let text = text.trim_start_matches(['$', '`', '>']).trim();
    let first = text
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_matches('`')
        .to_lowercase();
    COMMANDS.contains(&first.as_str())
}

/// Whether `line` is a fenced-block fence.
fn is_fence(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

/// An inline `Checks:` entry: a check label followed by a command or nothing.
fn inline_checks(line: &str) -> bool {
    let label = label(line);
    ["checks:", "check:", "self-check:", "self check:"]
        .iter()
        .any(|prefix| {
            label
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.trim().is_empty() || contains_command(rest))
        })
}

/// A standalone self-check instruction: an imperative line that names a
/// command, or ends with a colon whose continuation is a command.
fn self_check_instruction(lines: &[&str], index: usize) -> bool {
    let line = lines[index];
    let lower = line.trim().to_lowercase();
    let verbs = [
        "run",
        "then run",
        "also run",
        "re-run",
        "rerun",
        "verify",
        "validate",
        "confirm",
        "check that",
        "make sure",
        "ensure",
    ];
    let imperative = verbs.iter().any(|verb| {
        lower.strip_prefix(verb).is_some_and(|rest| {
            rest.is_empty()
                || rest.starts_with(' ')
                || rest.starts_with(':')
                || rest.starts_with('\t')
        })
    });
    if !imperative {
        return false;
    }
    contains_command(line)
        || (line.trim_end().ends_with(':')
            && next_non_blank(lines, index).is_some_and(command_line))
}

/// Whether a line continues a dropped instruction: a command, or a blank line
/// whose next non-blank line is a command.
fn is_continuation(lines: &[&str], index: usize, line: &str) -> bool {
    command_line(line)
        || (line.trim().is_empty() && next_non_blank(lines, index).is_some_and(command_line))
}

/// The next non-blank line after `index`.
fn next_non_blank<'a>(lines: &[&'a str], index: usize) -> Option<&'a str> {
    lines
        .iter()
        .skip(index + 1)
        .find(|line| !line.trim().is_empty())
        .copied()
}

#[cfg(test)]
#[path = "reslice_context_test.rs"]
mod tests;
