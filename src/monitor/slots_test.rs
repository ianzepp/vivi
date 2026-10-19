use std::collections::BTreeMap;

use super::super::events::ProjectEvents;
use super::super::model::RowState;
use super::rows;

#[test]
fn claimed_stopped_local_slot_is_retained_as_uncollected_with_a_tail() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("pool");
    let slot = root.join("alpha").join("lanes").join("slot-1");
    std::fs::create_dir_all(slot.join("marks")).unwrap();
    for (name, value) in [
        ("vivi_pool", "lanes"),
        ("vivi_pool_mailspace", "alpha"),
        ("vivi_pool_slot", "1"),
        ("vivi_pool_state", "ready"),
    ] {
        std::fs::write(slot.join("marks").join(name), format!("{value}\n")).unwrap();
    }
    std::fs::write(slot.join("claim"), "task=hand-12345678\nclaimed=1000\n").unwrap();
    std::fs::create_dir_all(slot.join("job")).unwrap();
    std::fs::write(slot.join("job/agent.out"), "receipt\n").unwrap();
    let pools: BTreeMap<String, crate::pool::config::PoolConfig> =
        toml::from_str("[lanes]\nprovider='local'\nslots=2\n[lanes.local]\nroot='pool'\n").unwrap();
    let (found, warnings) = rows(
        "alpha",
        temp.path(),
        "",
        &pools,
        &ProjectEvents::default(),
        1060,
    );
    assert!(warnings.is_empty());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].task.as_deref(), Some("hand-12345678"));
    assert_eq!(found[0].state, RowState::Finished("uncollected".into()));
    assert_eq!(found[0].running_secs, Some(60));
    assert!(
        found[0]
            .tail
            .as_ref()
            .is_some_and(|path| path.ends_with("job/agent.out"))
    );
}

#[test]
fn free_slots_do_not_create_monitor_rows() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("pool");
    let slot = root.join("alpha").join("lanes").join("slot-1");
    std::fs::create_dir_all(slot.join("marks")).unwrap();
    for (name, value) in [
        ("vivi_pool", "lanes"),
        ("vivi_pool_mailspace", "alpha"),
        ("vivi_pool_slot", "1"),
        ("vivi_pool_state", "ready"),
    ] {
        std::fs::write(slot.join("marks").join(name), format!("{value}\n")).unwrap();
    }
    let pools: BTreeMap<String, crate::pool::config::PoolConfig> =
        toml::from_str("[lanes]\nprovider='local'\nslots=2\n[lanes.local]\nroot='pool'\n").unwrap();
    let (found, _) = rows(
        "alpha",
        temp.path(),
        "",
        &pools,
        &ProjectEvents::default(),
        1000,
    );
    assert!(found.is_empty());
}
