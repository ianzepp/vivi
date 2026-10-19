use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{Action, ListView};

#[test]
fn list_keys_select_rows_open_tail_and_quit() {
    let mut view = ListView::default();
    assert_eq!(
        view.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), 12),
        Action::Down
    );
    assert_eq!(view.selected, 1);
    assert_eq!(
        view.key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE), 12),
        Action::NextPage
    );
    assert_eq!(view.selected, 11);
    assert_eq!(
        view.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 12),
        Action::EnterTail
    );
    assert_eq!(
        view.key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE), 12),
        Action::Quit
    );
}
