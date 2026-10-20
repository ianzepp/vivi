//! Keeping a job brief under the project server's letter cap.
//!
//! Every pump job reaches its worker as a mailspace task whose body is the
//! job brief. A synced mailspace refuses a letter over
//! [`MAX_LETTER_BYTES`](crate::sync::MAX_LETTER_BYTES), so a brief that is too
//! large is never sent: the send fails the same way on every tick. [`fit`]
//! cuts such a brief down before the send.

use crate::sync::MAX_LETTER_BYTES;

/// The largest brief file the pump sends. The rest of the cap is headroom for
/// the letter's own headers and the job header lines the pump prepends.
pub(super) const BRIEF_BUDGET: usize = MAX_LETTER_BYTES as usize - 8 * 1024;

/// How much of the brief's end survives a cut. Prompts end with their output
/// contract (result markers, the plan format), which the worker cannot lose.
const TAIL_BYTES: usize = 4 * 1024;

/// `body` unchanged when it fits [`BRIEF_BUDGET`]; otherwise its head and
/// tail around a marker that says how much was cut and where the full text
/// is saved. `None` means no cut was needed.
pub(super) fn fit(body: &str, saved: &str) -> Option<String> {
    if body.len() <= BRIEF_BUDGET {
        return None;
    }
    let marker = format!(
        "\n\n[pump: brief truncated from {} bytes to fit the 64 KiB letter cap; the middle was \
         cut; full text at {saved}]\n\n",
        body.len()
    );
    let head_end = floor_boundary(body, BRIEF_BUDGET - TAIL_BYTES - marker.len());
    let tail_start = ceil_boundary(body, body.len() - TAIL_BYTES);
    Some(format!(
        "{}{marker}{}",
        &body[..head_end],
        &body[tail_start..]
    ))
}

/// The largest char boundary at or below `index`.
fn floor_boundary(text: &str, index: usize) -> usize {
    (0..=index.min(text.len()))
        .rev()
        .find(|at| text.is_char_boundary(*at))
        .unwrap_or(0)
}

/// The smallest char boundary at or above `index`.
fn ceil_boundary(text: &str, index: usize) -> usize {
    (index..=text.len())
        .find(|at| text.is_char_boundary(*at))
        .unwrap_or(text.len())
}
