use super::*;

#[test]
fn scan_walks_only_the_project_pool_directory() {
    let project = tempfile::tempdir().unwrap();
    let config: MailspaceConfig = toml::from_str(
        r#"
name = "proj"
id = "aeff60b0-a1c4-4eaa-b688-7afcff51c659"

[pools.here]
provider = "local"
slots = 1

[pools.here.local]
root = "pool"
"#,
    )
    .unwrap();
    let pool_root = project.path().join("pool");
    let slot = pool_root.join("here/slot-1");
    write_slot(&slot, "here", "aeff60b0-a1c4-4eaa-b688-7afcff51c659");
    let sibling = pool_root.join("here-fetch/slot-1");
    write_slot(
        &sibling,
        "here-fetch",
        "aeff60b0-a1c4-4eaa-b688-7afcff51c659",
    );

    let marks = scan(&config, project.path());
    assert_eq!(marks.len(), 1);
    assert_eq!(marks[0].id, "here/slot-1");
    assert_eq!(marks[0].dir, slot);
    assert_eq!(marks[0].task.as_deref(), Some("taskhand1"));
    assert_eq!(marks[0].claimed_at, Some(400));
}

fn write_slot(dir: &Path, pool: &str, mailspace: &str) {
    std::fs::create_dir_all(dir.join("marks")).unwrap();
    std::fs::create_dir_all(dir.join(CHECKOUT)).unwrap();
    std::fs::write(dir.join("marks/vivi_pool"), format!("{pool}\n")).unwrap();
    std::fs::write(
        dir.join("marks/vivi_pool_mailspace"),
        format!("{mailspace}\n"),
    )
    .unwrap();
    std::fs::write(dir.join("marks/vivi_pool_slot"), "1\n").unwrap();
    std::fs::write(dir.join("marks/vivi_pool_state"), "ready\n").unwrap();
    std::fs::write(dir.join("claim"), "task=taskhand1\nclaimed=400\nnonce=aa\n").unwrap();
}
