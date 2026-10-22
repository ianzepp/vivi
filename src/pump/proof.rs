//! Proof-command validation for the pump: a proof cargo itself will
//! reject (`cargo test` takes one TESTNAME positional) must fail here,
//! at plan or dispatch time, instead of burning a hand attempt on a
//! one-second usage error.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Binaries a proof command may start with.
const BINARIES: [&str; 28] = [
    "[", "awk", "bash", "cargo", "cmake", "diff", "env", "find", "git", "go", "grep", "ls", "make",
    "mkdir", "node", "npm", "npx", "pnpm", "python", "python3", "rg", "sed", "sh", "test", "true",
    "vivi", "yarn", "zsh",
];

/// `cargo test` flags that consume the next word as their value.
const CARGO_VALUE_FLAGS: [&str; 17] = [
    "--bench",
    "--bin",
    "--color",
    "--exclude",
    "--example",
    "--features",
    "--jobs",
    "--manifest-path",
    "--message-format",
    "--package",
    "--profile",
    "--target",
    "--target-dir",
    "--test",
    "-Z",
    "-j",
    "-p",
];

/// How many wrapper levels (`env`, `sh -c`) validation may recurse into.
const MAX_DEPTH: usize = 2;

/// Validate one proof command against the target repository at `repo`.
/// `Ok` means the binary could plausibly parse it; `Err` carries the reason
/// it cannot, before any attempt is charged. A first word that is a
/// repo-relative path (contains `/` but does not start with it) must name an
/// existing executable file under `repo`; absolute paths still go through
/// the binary allowlist.
#[must_use]
pub fn check(cmd: &str, repo: &Path, allow: &[String]) -> Result<(), String> {
    let commands = split_chain(cmd);
    if commands.is_empty() {
        return Err("empty proof command".to_string());
    }
    for command in commands {
        let words = crate::pump::host::shell_split(&command);
        if !words.is_empty() {
            validate_words(&words, 0, cmd, repo, allow)?;
        }
    }
    Ok(())
}

/// What precedes a command in a shell list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sep {
    /// The start of the text.
    Start,
    /// `;`
    Seq,
    /// `&&`
    And,
    /// `||`
    Or,
    /// `|`
    Pipe,
}

/// Split shell chains without treating separators inside quotes as commands.
fn split_chain(text: &str) -> Vec<String> {
    scan_list(text)
        .into_iter()
        .map(|(_, command)| command)
        .collect()
}

/// The commands of a shell list outside quotes, each with the separator that
/// precedes it. A single `&` stays inside its command.
pub(crate) fn scan_list(text: &str) -> Vec<(Sep, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut commands = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut sep = Sep::Start;
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if quote == Some('"') && ch == '\\' {
            current.push(ch);
            i += 1;
            if i < chars.len() {
                current.push(chars[i]);
            }
        } else if quote.is_some() {
            current.push(ch);
            if quote == Some(ch) {
                quote = None;
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
            current.push(ch);
        } else if ch == ';' || ch == '|' || (ch == '&' && chars.get(i + 1) == Some(&'&')) {
            push_command(&mut commands, &mut current, sep);
            sep = match ch {
                ';' => Sep::Seq,
                '&' => Sep::And,
                _ if chars.get(i + 1) == Some(&'|') => Sep::Or,
                _ => Sep::Pipe,
            };
            if ch != ';' && chars.get(i + 1) == Some(&ch) {
                i += 1;
            }
        } else {
            current.push(ch);
        }
        i += 1;
    }
    push_command(&mut commands, &mut current, sep);
    commands
}

fn push_command(commands: &mut Vec<(Sep, String)>, current: &mut String, sep: Sep) {
    let command = current.trim();
    if !command.is_empty() {
        commands.push((sep, command.to_owned()));
    }
    current.clear();
}

/// The pipelines of a `;` / `&&` list, each as its `|`-joined stages. `None`
/// when the list uses `||`: a command that may not run is not something a
/// caller can reason about.
#[must_use]
pub(crate) fn pipelines(text: &str) -> Option<Vec<Vec<String>>> {
    let mut lines: Vec<Vec<String>> = Vec::new();
    for (sep, command) in scan_list(text) {
        match sep {
            Sep::Or => return None,
            Sep::Pipe if !lines.is_empty() => lines.last_mut()?.push(command),
            _ => lines.push(vec![command]),
        }
    }
    Some(lines)
}

/// Validate `words` as one command, recursing through wrapper binaries up
/// to `MAX_DEPTH` levels deep.
fn validate_words(
    words: &[String],
    depth: usize,
    cmd: &str,
    repo: &Path,
    allow: &[String],
) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    let words = words
        .iter()
        .skip_while(|word| word.as_str() == "!")
        .cloned()
        .collect::<Vec<_>>();
    if words.is_empty() {
        return Ok(());
    }
    let base = basename(&words[0]);
    // `env` and a leading `NAME=value` assignment are both wrappers: the
    // real command is whatever follows the flags and assignments.
    let assignment_prefix = words[0].contains('=') && !words[0].starts_with('-');
    if base == "env" || assignment_prefix {
        return validate_env_wrapper(&words, depth, cmd, repo, allow);
    }
    if matches!(base, "sh" | "bash" | "zsh") && words.len() > 2 && words[1] == "-c" {
        return validate_shell_wrapper(&words, depth, cmd, repo, allow);
    }
    validate_leaf(&words, base, cmd, repo, allow)
}

/// Validate the command nested after an `env` or assignment wrapper.
fn validate_env_wrapper(
    words: &[String],
    depth: usize,
    cmd: &str,
    repo: &Path,
    allow: &[String],
) -> Result<(), String> {
    let tail = wrapped_tail(&words[1..]);
    if tail.is_empty() {
        Ok(())
    } else {
        validate_words(&tail, depth + 1, cmd, repo, allow)
    }
}

/// Validate every chained command inside `sh -c`.
fn validate_shell_wrapper(
    words: &[String],
    depth: usize,
    cmd: &str,
    repo: &Path,
    allow: &[String],
) -> Result<(), String> {
    for command in split_chain(&words[2]) {
        let inner = crate::pump::host::shell_split(&command);
        if !inner.is_empty() {
            validate_words(&inner, depth + 1, cmd, repo, allow)?;
        }
    }
    Ok(())
}

/// Validate one unwrapped command's executable and binary-specific syntax.
fn validate_leaf(
    words: &[String],
    base: &str,
    cmd: &str,
    repo: &Path,
    allow: &[String],
) -> Result<(), String> {
    // A repo-relative path must name an executable file in the target
    // repository; it never goes through the bare-name allowlist.
    if words[0].contains('/') && !words[0].starts_with('/') {
        let path = repo.join(&words[0]);
        return match fs::metadata(&path) {
            Ok(meta) if meta.is_file() && meta.permissions().mode() & 0o111 != 0 => Ok(()),
            _ if is_target_build_path(&words[0]) => Ok(()),
            _ => Err(format!(
                "proof command repo path `{}` is missing or not executable in the target repository",
                words[0]
            )),
        };
    }
    if !BINARIES.contains(&base) && !allow.iter().any(|allowed| allowed == base) {
        return Err(format!(
            "proof command starts with unknown binary `{}`",
            words[0]
        ));
    }
    if base == "cargo" {
        return cargo_positionals(&words, cmd);
    }
    Ok(())
}

/// A missing project-local build output may be created by the proof itself.
fn is_target_build_path(word: &str) -> bool {
    let mut components = Path::new(word).components();
    matches!(components.next(), Some(std::path::Component::Normal(name)) if name == "target")
        && components.all(|component| !matches!(component, std::path::Component::ParentDir))
}

/// The words after a wrapper prefix: leading flags and `NAME=value`
/// assignments are skipped, and what remains is the wrapped command.
fn wrapped_tail(words: &[String]) -> Vec<String> {
    words
        .iter()
        .skip_while(|w| w.starts_with('-') || w.contains('='))
        .cloned()
        .collect()
}

/// The positional TESTNAME filters of a `cargo test` command (`words` start
/// at `cargo`), or `None` when the command is not `cargo test`.
#[must_use]
pub(crate) fn cargo_test_filters(words: &[String]) -> Option<Vec<String>> {
    let sub = words
        .get(1..)?
        .iter()
        .position(|w| !w.starts_with('-') && !w.starts_with('+'))?;
    if words[sub + 1].as_str() != "test" {
        return None;
    }
    let after = &words[sub + 2..];
    let mut filters = Vec::new();
    let mut i = 0;
    while i < after.len() {
        let word = after[i].as_str();
        if word == "--" {
            break;
        }
        if word.starts_with('-') {
            if !word.contains('=') && CARGO_VALUE_FLAGS.contains(&word) {
                i += 1;
            }
        } else {
            filters.push(word.to_owned());
        }
        i += 1;
    }
    Some(filters)
}

/// Count `cargo test` positional TESTNAME filters: cargo accepts one, and
/// a second dies at the CLI before any test runs.
fn cargo_positionals(words: &[String], cmd: &str) -> Result<(), String> {
    let filters = cargo_test_filters(words).map_or(0, |filters| filters.len());
    if filters > 1 {
        return Err(format!(
            "cargo test takes one TESTNAME filter, found {filters}: {cmd}"
        ));
    }
    Ok(())
}

/// Whether `text` is cargo's usage-error family: an argument-parse error
/// line plus a usage line. Compile errors and plain test failures are not.
#[must_use]
pub fn usage_failure(text: &str) -> bool {
    let mut error = false;
    let mut usage = false;
    for line in text.lines() {
        if line.starts_with("error: unexpected argument") || line.starts_with("error: the argument")
        {
            error = true;
        }
        if line.starts_with("Usage:") {
            usage = true;
        }
    }
    error && usage
}

/// The final path component of a command word: everything through the
/// last `/`.
fn basename(word: &str) -> &str {
    match word.rfind('/') {
        Some(idx) => &word[idx + 1..],
        None => word,
    }
}
