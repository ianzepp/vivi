//! Persistent reports for shared checks that fail once and pass on retry.

use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use sha2::{Digest, Sha256};

use crate::pump::board::{NeedRow, parse_rows};
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::store::{DB_FILENAME, PumpStore};

/// Three fail-pass events for the same test in a rolling day open a need.
const DAILY_NEED_THRESHOLD: usize = 3;
const DAY_SECS: u64 = 24 * 60 * 60;

/// Pool lanes share one pump home; serialize need lookup/send so concurrent
/// flapping lanes do not create duplicate needs for the same test.
static NEED_LOCK: Mutex<()> = Mutex::new(());

/// Record a fail-pass event reported by a validated pool merge receipt.
/// `name` is the check/test name supplied by the worker when available; an
/// empty name falls back to a stable test or command identity.
pub(super) fn record_worker_flake(ctx: &Ctx, name: &str, command: &str, failure: &str) {
    let name = if name.trim().is_empty() {
        shared_test_name(command, failure)
    } else {
        bounded(name.trim(), 160)
    };
    let detail = format!(
        "shared check failed once and passed on immediate retry; command={}; evidence={}",
        bounded(command, 180),
        bounded(failure, 240),
    );
    record_flake(ctx, &name, command, failure, &detail);
}

fn record_flake(ctx: &Ctx, name: &str, command: &str, failure: &str, detail: &str) {
    ctx.exception("flaky-check", name, detail);
    let _guard = NEED_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(count) = recent_flakes(ctx, name) else {
        ctx.log(&format!(
            "flaky-check {name}: cannot read daily flake history"
        ));
        return;
    };
    if count < DAILY_NEED_THRESHOLD {
        return;
    }
    let subject = need_subject(name);
    match open_need_exists(ctx, &subject) {
        Some(true) => return,
        Some(false) => {}
        None => {
            ctx.log(&format!(
                "flaky-check {name}: cannot check for an open need"
            ));
            return;
        }
    }
    send_need(ctx, name, &subject, count, command, failure);
}

/// The stable name used for daily counts: a reported failed test when
/// available, otherwise the shared-check command's final word.
pub(super) fn shared_test_name(command: &str, failure: &str) -> String {
    if let Some(name) = failed_test_name(failure) {
        return name;
    }
    let words = crate::pump::host::shell_split(command);
    if words.is_empty() {
        bounded(command, 160)
    } else {
        bounded(&words.join(" "), 160)
    }
}

fn failed_test_name(output: &str) -> Option<String> {
    for line in output.lines().map(str::trim) {
        if let Some(name) = line
            .strip_prefix("test ")
            .and_then(|rest| rest.strip_suffix(" ... FAILED"))
            .and_then(|rest| rest.split_whitespace().next())
        {
            return Some(name.to_owned());
        }
        if let Some(name) = line
            .strip_prefix("FAILED ")
            .and_then(|rest| rest.split_whitespace().next())
        {
            return Some(name.to_owned());
        }
    }
    None
}

fn recent_flakes(ctx: &Ctx, name: &str) -> Option<usize> {
    let store = PumpStore::open_read_only(&ctx.home.join(DB_FILENAME)).ok()??;
    let cutoff = ctx.now().saturating_sub(DAY_SECS);
    store.exceptions().ok().map(|rows| {
        rows.iter()
            .filter(|(at, kind, unit, _)| {
                *at >= cutoff && *at <= ctx.now() && kind == "flaky-check" && unit == name
            })
            .count()
    })
}

fn open_need_exists(ctx: &Ctx, subject: &str) -> Option<bool> {
    let output = ctx.vivi(
        &format!(
            "need list --status open --json --to {}",
            shell_quote(&ctx.cfg.reply_to())
        ),
        60,
    );
    if output.code != 0 {
        return None;
    }
    let rows = parse_rows(&output.text)?;
    Some(rows.iter().any(|row: &NeedRow| row.subject == subject))
}

fn send_need(ctx: &Ctx, name: &str, subject: &str, count: usize, command: &str, failure: &str) {
    let path = need_body_path(ctx, name);
    if let Some(parent) = path.parent()
        && let Err(error) = fs::create_dir_all(parent)
    {
        ctx.exception("flaky-check-need-failed", name, &error.to_string());
        return;
    }
    let body = format!(
        "The shared check `{name}` failed and passed on its immediate retry {count} times in the last 24 hours.\n\nCommand: {command}\nLatest failure evidence: {}\n\ndone_when: Make `{name}` pass reliably under normal shared-check load and remove the need for retry-based flake handling.\n",
        bounded(failure, 1200),
    );
    if let Err(error) = fs::write(&path, body) {
        ctx.exception("flaky-check-need-failed", name, &error.to_string());
        return;
    }
    let output = ctx.vivi(
        &format!(
            "need send --from {} --to {} --subject {} --body-file {}",
            shell_quote(&ctx.cfg.reply_from),
            shell_quote(&ctx.cfg.reply_to()),
            shell_quote(subject),
            shell_quote(&path.to_string_lossy()),
        ),
        60,
    );
    if output.code == 0 && parse_need_handle(&output.text).is_some() {
        ctx.log(&format!(
            "flaky-check need sent for {name}: {count} failures in 24h"
        ));
    } else {
        ctx.exception(
            "flaky-check-need-failed",
            name,
            &format!("need send failed: {}", bounded(&output.text, 200)),
        );
    }
}

fn parse_need_handle(output: &str) -> Option<&str> {
    output
        .lines()
        .find(|line| line.starts_with("created "))
        .and_then(|line| line.split_whitespace().nth(2))
        .or_else(|| {
            output
                .lines()
                .find(|line| line.starts_with("sent "))
                .and_then(|line| line.split_whitespace().nth(1))
        })
}

fn need_subject(name: &str) -> String {
    format!("pump: flaky shared check: {}", bounded(name, 120))
}

fn need_body_path(ctx: &Ctx, name: &str) -> PathBuf {
    let digest = hex::encode(Sha256::digest(name.as_bytes()));
    ctx.home
        .join("flaky-checks")
        .join(format!("{}.txt", &digest[..16]))
}

fn bounded(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

#[cfg(test)]
#[path = "flaky_test.rs"]
mod tests;
