//! Pump notes: the front-matter form of a pump-marked reply.
//!
//! A pump note is a `mail` letter whose `[pump]` table names the note word, the
//! order stamp `t` and an optional reference. Thread order has one-second
//! precision, so same-second notes need their own order: [`next_stamp`] hands
//! out strictly increasing microsecond stamps.

use crate::error::VivariumError;
use crate::letter::records::{MailFront, NoteWord, PumpNote};
use crate::letter::work::JobFront;
use std::sync::atomic::{AtomicI64, Ordering};

/// The last order stamp handed out by [`next_stamp`].
static LAST_STAMP: AtomicI64 = AtomicI64::new(0);

/// A pump note read back from a reply body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub pump: PumpNote,
    pub job: Option<JobFront>,
    pub prose: String,
}

/// The next order stamp: the clock in unix microseconds, pushed one past the
/// last stamp this process issued, so it strictly increases.
#[must_use]
pub fn next_stamp() -> i64 {
    let now = chrono::Utc::now().timestamp_micros();
    LAST_STAMP
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |last| {
            Some(now.max(last.saturating_add(1)))
        })
        .map_or(now, |last| now.max(last.saturating_add(1)))
}

/// The body of a pump note: `word`, a fresh stamp, `reference` when given, and
/// `prose` below the front matter.
///
/// # Errors
/// Returns [`VivariumError`] for a brief note (it carries a `[job]` table) or
/// when the letter does not render.
pub fn note_body(
    word: NoteWord,
    reference: Option<&str>,
    prose: &str,
) -> Result<String, VivariumError> {
    if word == NoteWord::Brief {
        return Err(VivariumError::Message(
            "a brief note carries a [job] table: use brief_body".to_string(),
        ));
    }
    let front = MailFront {
        pump: Some(PumpNote {
            note: word,
            t: next_stamp(),
            reference: reference.map(str::to_owned),
        }),
        job: None,
    };
    crate::letter::render(&front, prose)
        .map_err(|err| VivariumError::Message(format!("pump note: {err}")))
}

/// The body of a brief note: the `[job]` table of `job`, a fresh stamp and
/// `prose` below the front matter.
///
/// # Errors
/// Returns [`VivariumError`] when the letter does not render.
pub fn brief_body(job: JobFront, prose: &str) -> Result<String, VivariumError> {
    let front = MailFront {
        pump: Some(PumpNote {
            note: NoteWord::Brief,
            t: next_stamp(),
            reference: None,
        }),
        job: Some(job),
    };
    crate::letter::render(&front, prose)
        .map_err(|err| VivariumError::Message(format!("pump brief: {err}")))
}

/// The pump note `body` is, or `None` for plain mail, an invalid letter or a
/// letter with no `[pump]` table.
#[must_use]
pub fn note_of(body: &str) -> Option<Note> {
    let letter = match crate::letter::parse_letter::<MailFront>(body) {
        Ok(letter) => letter,
        Err(_) => return None,
    };
    let pump = letter.front.pump?;
    Some(Note {
        pump,
        job: letter.front.job,
        prose: letter.body,
    })
}

#[cfg(test)]
#[path = "pump_note_test.rs"]
mod tests;
