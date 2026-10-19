//! Proof-command validation for the pump: a proof cargo itself will
//! reject (`cargo test` takes one TESTNAME positional) must fail here,
//! at plan or dispatch time, instead of burning a hand attempt on a
//! one-second usage error.

/// Binaries a proof command may start with.
const BINARIES: [&str; 26] = [
    "awk", "bash", "cargo", "cmake", "diff", "env", "find", "git", "go", "grep", "ls", "make",
    "mkdir", "node", "npm", "npx", "pnpm", "python", "python3", "rg", "sed", "sh", "true", "vivi",
    "yarn", "zsh",
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

/// Validate one proof command. `Ok` means the binary could plausibly parse
/// it; `Err` carries the reason it cannot, before any attempt is charged.
#[must_use]
pub fn check(cmd: &str) -> Result<(), String> {
    let words = crate::pump::host::shell_split(cmd);
    if words.is_empty() {
        return Err("empty proof command".to_string());
    }
    validate_words(&words, 0, cmd)
}

/// Validate `words` as one command, recursing through wrapper binaries up
/// to `MAX_DEPTH` levels deep.
fn validate_words(words: &[String], depth: usize, cmd: &str) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    let base = basename(&words[0]);
    if base == "env" {
        let tail: Vec<String> = words[1..]
            .iter()
            .skip_while(|w| w.starts_with('-') || w.contains('='))
            .cloned()
            .collect();
        return if tail.is_empty() {
            Ok(())
        } else {
            validate_words(&tail, depth + 1, cmd)
        };
    }
    if matches!(base, "sh" | "bash" | "zsh") && words.len() > 2 && words[1] == "-c" {
        let inner = crate::pump::host::shell_split(&words[2]);
        return if inner.is_empty() {
            Ok(())
        } else {
            validate_words(&inner, depth + 1, cmd)
        };
    }
    if !BINARIES.contains(&base) {
        return Err(format!(
            "proof command starts with unknown binary `{}`",
            words[0]
        ));
    }
    if base == "cargo" {
        return cargo_positionals(words, cmd);
    }
    Ok(())
}

/// Count `cargo test` positional TESTNAME filters: cargo accepts one, and
/// a second dies at the CLI before any test runs.
fn cargo_positionals(words: &[String], cmd: &str) -> Result<(), String> {
    let Some(sub) = words[1..]
        .iter()
        .position(|w| !w.starts_with('-') && !w.starts_with('+'))
    else {
        return Ok(());
    };
    if words[sub + 1].as_str() != "test" {
        return Ok(());
    }
    let after = &words[sub + 2..];
    let mut filters = 0;
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
            filters += 1;
        }
        i += 1;
    }
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
