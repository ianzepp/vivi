//! The boot pump's walk of a project-relative local pool root.

use super::*;

fn config(text: &str) -> MailspaceConfig {
    toml::from_str(text).unwrap()
}

fn plant(dir: &Path, mailspace: &str, pool: &str, slot: &str) {
    std::fs::create_dir_all(dir.join("marks")).unwrap();
    std::fs::write(dir.join("marks/vivi_pool"), format!("{pool}\n")).unwrap();
    std::fs::write(
        dir.join("marks/vivi_pool_mailspace"),
        format!("{mailspace}\n"),
    )
    .unwrap();
    std::fs::write(dir.join("marks/vivi_pool_slot"), format!("{slot}\n")).unwrap();
    std::fs::write(dir.join("marks/base_ref"), "next\n").unwrap();
    std::fs::write(dir.join("marks/base_sha"), "abc1234\n").unwrap();
}

#[test]
fn scan_walks_the_project_pool_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().canonicalize().unwrap();
    let slot = project.join("pool/here/slot-1");
    plant(
        &slot,
        "aeff60b0-a1c4-4eaa-b688-7afcff51c659",
        "here",
        "1",
    );
    let config = config(
        "name = \"demo\"\nid = \"aeff60b0-a1c4-4eaa-b688-7afcff51c659\"\n\
         [pools.here]\nprovider = \"local\"\nslots = 2\n\
         [pools.here.local]\nroot = \"pool\"\n",
    );
    let marks = scan(&config, &project);
    assert_eq!(marks.len(), 1, "{marks:?}");
    assert_eq!(marks[0].pool, "here");
    assert_eq!(marks[0].id, "here/slot-1");
    assert_eq!(marks[0].dir, slot);
    assert_eq!(marks[0].base_ref.as_deref(), Some("next"));
    assert_eq!(marks[0].base_sha.as_deref(), Some("abc1234"));
}

#[test]
fn scan_keeps_a_sibling_pool_out_of_its_own_listing() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().canonicalize().unwrap();
    plant(&project.join("pool/here/slot-1"), "key", "here", "1");
    plant(
        &project.join("pool/here-fetch/slot-1"),
        "key",
        "here-fetch",
        "1",
    );
    let config = config(
        "name = \"demo\"\nid = \"key\"\n[pools.here]\nprovider = \"local\"\nslots = 2\n\
         [pools.here.local]\nroot = \"pool\"\n",
    );
    let marks = scan(&config, &project);
    let ids: Vec<&str> = marks.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["here/slot-1"]);
}
