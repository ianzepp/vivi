//! Tests for [`crate::mind::store`].

use tempfile::tempdir;

use super::*;

#[test]
fn missing_state_is_default_and_round_trips() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("mind").join("state.json");
    assert_eq!(load_at(&path).unwrap(), MindState::default());
    let mut state = MindState {
        session_id: Some("s-1".into()),
        paused: true,
        ..MindState::default()
    };
    state.ledger.insert("n1".into(), vec![5, 6]);
    state.ignored.insert("/w/a".into());
    save_at(&path, &state).unwrap();
    assert_eq!(load_at(&path).unwrap(), state);
    assert!(!path.with_extension("json.tmp").exists());
}

#[test]
fn update_changes_and_saves() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    update_at(&path, |state| {
        state.paused = true;
        Ok(())
    })
    .unwrap();
    assert!(load_at(&path).unwrap().paused);
}

#[test]
fn note_wake_keeps_one_hour() {
    let mut state = MindState {
        recent_wakes: vec![10, 3000, 4000],
        ..MindState::default()
    };
    state.note_wake(4000 + 600);
    assert_eq!(state.recent_wakes, vec![3000, 4000, 4600]);
}

#[test]
fn corrupt_state_is_an_error() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    std::fs::write(&path, "{not json").unwrap();
    assert!(load_at(&path).is_err());
}

#[test]
fn concurrent_updates_do_not_lose_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let writers: Vec<_> = (0..8)
        .map(|n| {
            let path = path.clone();
            std::thread::spawn(move || {
                update_at(&path, |state| {
                    state
                        .ignored
                        .insert(std::path::PathBuf::from(format!("/p/{n}")));
                    Ok(())
                })
                .unwrap();
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    assert_eq!(load_at(&path).unwrap().ignored.len(), 8);
}
