//! Quiet exit 0 when stdout is a closed pipe.
//!
//! Rust ignores `SIGPIPE`, so a `println!` whose stdout reader already exited
//! (`vivi skills show vivi | head -1`) panics with the standard
//! "failed printing to stdout" message instead of terminating the process.
//! Under `set -o pipefail` that turns ordinary pipelines into failures with
//! exit 101 and a panic on stderr.
//!
//! [`install`] chains a panic hook that turns exactly that panic — a stdout
//! write that failed with a broken pipe — into a silent exit 0. Every other
//! panic keeps the default hook, and stderr write failures are never swallowed.

use std::any::Any;
use std::panic;
use std::process;

/// Prefix std puts on the panic payload when a `println!` write fails.
const STDOUT_PRINT_PANIC_PREFIX: &str = "failed printing to stdout: ";

/// Case-insensitive spellings of a broken pipe in a rendered `io::Error`:
/// the Unix/macOS raw-os-error wording and the Windows wording.
const BROKEN_PIPE_MARKS: [&str; 2] = ["broken pipe", "pipe has been ended"];

/// Install the broken-pipe panic hook, keeping the previous hook for all
/// other panics. Call once, at the top of `main`, before any output.
pub fn install() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        if payload_is_stdout_broken_pipe(info.payload()) {
            process::exit(0);
        }
        previous(info);
    }));
}

/// True when `payload` is std's "failed printing to stdout" panic whose
/// rendered error is a broken pipe; false for every other panic, including
/// failed prints to stderr and stdout failures that are not broken pipes.
#[must_use]
pub fn payload_is_stdout_broken_pipe(payload: &(dyn Any + Send)) -> bool {
    let Some(message) = payload_message(payload) else {
        return false;
    };
    let Some(error) = message.strip_prefix(STDOUT_PRINT_PANIC_PREFIX) else {
        return false;
    };
    let error = error.to_lowercase();
    BROKEN_PIPE_MARKS.iter().any(|mark| error.contains(mark))
}

fn payload_message(payload: &(dyn Any + Send)) -> Option<&str> {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        return Some(message);
    }
    payload.downcast_ref::<String>().map(String::as_str)
}

#[cfg(test)]
#[path = "quiet_pipe_test.rs"]
mod tests;
