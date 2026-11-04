//! `status_lines`: the `vivi sync status` stuck-letter and kept-local lines,
//! built from a plain pending selection — one oversize letter, one kept.

use crate::sync::kept::status_lines;
use crate::sync::select::{Selection, SkipReason, Skipped};

/// A pending selection holding one oversize letter and one kept-local one.
fn selection() -> Selection {
    Selection {
        letters: Vec::new(),
        skipped: vec![
            Skipped {
                handle: "deadbeef".into(),
                reason: SkipReason::TooLarge(70_000),
            },
            Skipped {
                handle: "12345678".into(),
                reason: SkipReason::KeptLocal,
            },
        ],
        held_back: 0,
    }
}

#[test]
fn a_stuck_letter_is_named_with_its_clearing_command() {
    let lines = status_lines(&selection(), 0);

    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(
        lines[0],
        "sync not uploaded: 1 too large (over the 64 KiB cap); run vivi sync keep-local \
         <handle> for each, or vivi sync push to list them"
    );
    assert_eq!(
        lines[1],
        "sync stuck deadbeef: 70000 bytes, over the 64 KiB cap; not uploaded; \
         clear it with: vivi sync keep-local deadbeef"
    );
}

#[test]
fn a_nonempty_kept_set_gets_its_lift_line() {
    let lines = status_lines(&selection(), 1);

    assert_eq!(
        lines.last().map(String::as_str),
        Some("sync kept local: 1 letter(s); vivi sync keep-local --undo <handle> lifts it")
    );
}

#[test]
fn an_empty_kept_set_gets_no_lift_line() {
    let lines = status_lines(&selection(), 0);

    assert!(
        !lines.iter().any(|line| line.contains("kept local")),
        "{lines:?}"
    );
}

#[test]
fn more_than_ten_stuck_letters_collapse_to_a_count() {
    let mut selection = selection();
    selection.skipped = (0..12)
        .map(|n| Skipped {
            handle: format!("deadbe{n:02}"),
            reason: SkipReason::TooLarge(70_000),
        })
        .collect();

    let lines = status_lines(&selection, 0);

    assert_eq!(lines.len(), 12, "{lines:?}");
    assert_eq!(lines[11], "sync stuck: ... and 2 more");
    assert!(
        !lines.iter().any(|line| line.contains("deadbe10")),
        "{lines:?}"
    );
}
