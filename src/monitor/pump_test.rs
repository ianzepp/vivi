use super::super::model::RowState;
use super::row;

#[test]
fn pump_header_points_to_log_when_present_and_is_idle_without_a_lock() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    std::fs::write(home.join("log.txt"), "tick\n").unwrap();
    let row = row("alpha", home, 1000).unwrap();
    assert_eq!(row.kind, super::super::model::RowKind::Pump);
    assert_eq!(row.state, RowState::Finished("idle".into()));
    assert_eq!(row.tail.as_deref(), Some(home.join("log.txt").as_path()));
}
