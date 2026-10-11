use std::fs;

use super::*;
use crate::mailspace::Mailspace;

#[test]
fn the_content_is_exactly_the_agreed_two_comment_lines_and_a_star() {
    assert_eq!(
        IGNORE_CONTENT,
        "# Machine-local vivi state (config with the mailspace id, charters, keys, store,\n\
         # blobs, logs). Nothing in this directory is committed.\n*\n"
    );
}

#[test]
fn init_creates_it_and_an_open_finds_it_already_there() {
    let tmp = tempfile::tempdir().unwrap();
    Mailspace::init(Some(tmp.path())).unwrap();
    let file = tmp.path().join(".vivi/.gitignore");

    assert_eq!(fs::read_to_string(&file).unwrap(), IGNORE_CONTENT);
    assert!(!ensure_ignore_file(&tmp.path().join(".vivi")), "not again");
    fs::remove_file(&file).unwrap();
    assert!(ensure_ignore_file(&tmp.path().join(".vivi")), "once");
    assert!(!ensure_ignore_file(&tmp.path().join(".vivi")));
}

#[test]
fn an_existing_file_and_a_missing_directory_are_left_alone_without_error() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join(".gitignore"), "mine\n").unwrap();

    assert!(!ensure_ignore_file(tmp.path()));
    assert_eq!(
        fs::read_to_string(tmp.path().join(".gitignore")).unwrap(),
        "mine\n"
    );
    assert!(!ensure_ignore_file(&tmp.path().join("no/such/dir")));
}

#[test]
fn a_directory_that_is_not_a_repository_has_no_tracked_config() {
    let tmp = tempfile::tempdir().unwrap();
    Mailspace::init(Some(tmp.path())).unwrap();

    assert!(tracked_config_warning(tmp.path(), ".vivi/mailspace.toml").is_none());
    assert!(tracked_config_warning(&tmp.path().join("gone"), ".vivi/mailspace.toml").is_none());
}
