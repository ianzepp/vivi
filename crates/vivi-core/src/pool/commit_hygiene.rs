//! Keeping tracker handles out of the history a pool job writes.
//!
//! A commit a pool job publishes must read as a plain change: no task, need,
//! unit, attempt or job identifiers, and no branch or ref names. The task
//! subject carries tags (`[3ea8dee9-1 a1] Retire X`), so the commit subject is
//! the task subject with those tags removed. The runner also receives the list
//! of tokens that must not appear in any commit it is about to publish
//! (`FORBID` in `params.env`) and refuses to publish when one does.

use super::job::JobPlan;

/// The commit subject used when stripping tags leaves nothing.
pub const FALLBACK_SUBJECT: &str = "Vivi pool work";

/// The rule every pool job's brief carries about commit messages.
pub const COMMIT_MESSAGE_RULE: &str = "If you commit, write a plain commit message describing the change. Never put task, need, unit, attempt or job identifiers, or branch or ref names, in a commit message.";

/// Shortest token the runner will check for; shorter ones match too much.
const MIN_TOKEN_LEN: usize = 6;

/// The literal prefix the runner fetches commit-addressed sources under.
const WITH_REF_PREFIX: &str = "refs/vivi/with/";

/// The leading `[...]` tags of a task subject, as the text inside each.
fn leading_tags(subject: &str) -> (Vec<&str>, &str) {
    let mut rest = subject.trim();
    let mut tags = Vec::new();
    while rest.starts_with('[') {
        let Some(end) = rest.find(']') else { break };
        tags.push(&rest[1..end]);
        rest = rest[end + 1..].trim_start();
    }
    (tags, rest)
}

/// The commit subject for a task subject: its first line without any leading
/// bracketed tags and, when `need` is known, without the trailing
/// ` (<need> r<N>)` the pump puts on the subject of every task it mints. It
/// is [`FALLBACK_SUBJECT`] when nothing is left.
#[must_use]
pub fn commit_subject(task_subject: &str, need: Option<&str>) -> String {
    let first = task_subject.lines().next().unwrap_or_default();
    let (_, rest) = leading_tags(first);
    let rest = need.map_or(rest, |need| without_run_suffix(rest, need));
    if rest.is_empty() {
        FALLBACK_SUBJECT.to_string()
    } else {
        rest.to_string()
    }
}

/// `subject` without a trailing ` (<need> r<digits>)`, or unchanged.
fn without_run_suffix<'a>(subject: &'a str, need: &str) -> &'a str {
    let Some(open) = subject
        .rfind(" (")
        .or_else(|| subject.starts_with('(').then_some(0))
    else {
        return subject;
    };
    let group = subject[open..].trim_start().trim_start_matches('(');
    let run = group
        .strip_suffix(')')
        .and_then(|inner| inner.strip_prefix(need))
        .and_then(|rest| rest.strip_prefix(" r"));
    match run {
        Some(digits) if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => {
            subject[..open].trim_end()
        }
        _ => subject,
    }
}

/// The commit-message rule of a pool job's brief, ending with the commit
/// subject the job should use.
#[must_use]
pub fn commit_rule(task_subject: &str, need: Option<&str>) -> String {
    format!(
        "{COMMIT_MESSAGE_RULE} Use this commit subject: {}",
        commit_subject(task_subject, need)
    )
}

/// The unit id inside a `.../unit/<uid>-a<N>` branch name.
fn unit_id(branch: &str) -> Option<&str> {
    let (_, tail) = branch.rsplit_once("/unit/")?;
    let (uid, attempt) = tail.rsplit_once("-a")?;
    let numeric = !attempt.is_empty() && attempt.bytes().all(|b| b.is_ascii_digit());
    (numeric && !uid.is_empty()).then_some(uid)
}

fn is_commit_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn push_token(tokens: &mut Vec<String>, token: &str) {
    let token = token.trim();
    let usable = token.len() >= MIN_TOKEN_LEN && !token.chars().any(char::is_whitespace);
    if usable && !tokens.iter().any(|t| t == token) {
        tokens.push(token.to_string());
    }
}

/// Whether `name` is the pump's integration branch (`vivi/main`, or whatever
/// a target's `main` names under `vivi/`) rather than a branch that carries a
/// need or unit id. The integration branch is a plain, fixed name; a commit
/// message may mention it.
fn is_integration_branch(name: &str) -> bool {
    name.starts_with("vivi/") && !name.starts_with("vivi/need/") && !name.starts_with("vivi/unit/")
}

fn push_ref(tokens: &mut Vec<String>, name: &str) {
    if is_commit_id(name) || is_integration_branch(name) {
        return;
    }
    push_token(tokens, name);
    if let Some(uid) = unit_id(name) {
        push_token(tokens, uid);
    }
}

/// Every token a published commit message must not contain: the task handle,
/// the need id, the job id, the tags of the task subject, the job's own
/// branch, every extra source ref (and the unit ids inside their names), a
/// tracker-owned base ref, and the literal `refs/vivi/with/` prefix.
#[must_use]
pub fn forbidden_tokens(spec: &JobPlan) -> Vec<String> {
    let mut tokens = Vec::new();
    push_token(&mut tokens, &spec.task);
    push_token(&mut tokens, spec.need.as_deref().unwrap_or_default());
    push_token(&mut tokens, &spec.job);
    let first = spec.subject.lines().next().unwrap_or_default();
    for tag in leading_tags(first).0 {
        for word in tag.split_whitespace() {
            push_token(&mut tokens, word);
            if let Some((_, id)) = word.split_once(':') {
                push_token(&mut tokens, id);
            }
        }
    }
    push_ref(&mut tokens, &spec.refs.branch);
    for with in &spec.with {
        push_ref(&mut tokens, &with.name);
    }
    if spec.refs.from.starts_with("vivi/") || spec.refs.from.starts_with("pump/") {
        push_ref(&mut tokens, &spec.refs.from);
    }
    push_token(&mut tokens, WITH_REF_PREFIX);
    tokens
}

#[cfg(test)]
#[path = "commit_hygiene_test.rs"]
mod tests;
