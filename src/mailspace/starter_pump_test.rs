use super::STARTER_PUMP;
use crate::mailspace::Mailspace;

fn config_text(root: &std::path::Path) -> String {
    std::fs::read_to_string(root.join(".vivi").join("mailspace.toml")).unwrap()
}

#[test]
fn the_starter_table_parses_and_is_written_with_its_comments() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Mailspace::init(Some(tmp.path())).unwrap();
    assert!(space.config.pump.is_none());
    space.ensure_starter_pump().unwrap();
    let pump = space.config.pump.as_ref().expect("pump after starter");
    assert!(pump.committer_name.is_empty());
    let reopened = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    assert!(reopened.config.pump.is_some());
    let text = config_text(tmp.path());
    assert!(text.contains("# Starter pump configuration."), "{text}");
    assert!(!text.contains("[pump.gate]"), "{text}");
}

#[test]
fn ensure_starter_pump_is_idempotent_and_keeps_the_rest_of_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Mailspace::init(Some(tmp.path())).unwrap();
    space.ensure_operator_role().unwrap();
    space.ensure_starter_pump().unwrap();
    let once = config_text(tmp.path());
    space.ensure_starter_pump().unwrap();
    assert_eq!(config_text(tmp.path()), once);
    assert_eq!(once.matches("[pump]").count(), 1, "{once}");
    assert!(once.contains("name = \"operator\""), "{once}");
}

#[test]
fn ensure_starter_pump_leaves_an_existing_pump_table_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Mailspace::init(Some(tmp.path())).unwrap();
    let path = tmp.path().join(".vivi").join("mailspace.toml");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("\n[pump]\ncommitter_name = \"kept\"\n");
    std::fs::write(&path, text).unwrap();
    let mut space2 = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    space2.ensure_starter_pump().unwrap();
    assert_eq!(space2.config.pump.as_ref().unwrap().committer_name, "kept");
    assert!(!config_text(tmp.path()).contains("Starter pump"));
    drop(space);
}

#[test]
fn config_exists_follows_the_config_file() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(!Mailspace::config_exists(Some(tmp.path())).unwrap());
    Mailspace::init(Some(tmp.path())).unwrap();
    assert!(Mailspace::config_exists(Some(tmp.path())).unwrap());
}

#[test]
fn the_starter_text_names_every_required_value() {
    for key in ["committer_name", "committer_email"] {
        assert!(STARTER_PUMP.contains(key), "{key}");
    }
}
