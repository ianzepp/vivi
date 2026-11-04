use super::*;
use crate::doctor::Status;

const ID: &str = "12345678-1234-1234-1234-123456789abc";

fn config(tables: &str) -> MailspaceConfig {
    toml::from_str(&format!("name = 'test'\nid = '{ID}'\n{tables}")).unwrap()
}

fn mark(root: &Path, dir_space: &str, pool: &str, space_mark: Option<&str>) {
    let marks = root.join(dir_space).join(pool).join("slot-1/marks");
    fs::create_dir_all(&marks).unwrap();
    fs::write(marks.join("vivi_pool"), pool).unwrap();
    if let Some(space) = space_mark {
        fs::write(marks.join("vivi_pool_mailspace"), space).unwrap();
    }
}

#[test]
fn no_pools_are_skipped() {
    let result = inspect(&config(""), Path::new("/unused"));
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].status, Status::Skipped);
}

#[test]
fn invalid_settings_fail_without_echoing_values() {
    let result = inspect(
        &config("[pools.a]\nprovider = 'SECRET'\nslots = 2"),
        Path::new("/unused"),
    );
    assert!(result.iter().any(|f| f.status == Status::Fail));
    assert!(!format!("{result:?}").contains("SECRET"));
}

#[test]
fn remote_slots_are_explicitly_unverified() {
    let result = inspect(
        &config(
            "[pools.remote]\nprovider = 'ssh'\nslots = 3\n[pools.remote.ssh]\nhost = 'private-host'",
        ),
        Path::new("/unused"),
    );
    let summary = result.iter().find(|f| f.name == "pool remote").unwrap();
    assert!(summary.detail.contains("capacity 3"));
    let ownership = result
        .iter()
        .find(|f| f.name == "ownership remote")
        .unwrap();
    assert_eq!(ownership.status, Status::Skipped);
    assert!(ownership.detail.contains("offline"));
    assert!(!format!("{result:?}").contains("private-host"));
}

#[test]
fn shared_roots_and_overlapping_prefixes_warn() {
    let config = config(
        "[pools.fast]\nprovider = 'ssh'\nslots = 1\n[pools.fast.ssh]\nhost = 'same'\n[pools.fast-extra]\nprovider = 'ssh'\nslots = 1\n[pools.fast-extra.ssh]\nhost = 'same'",
    );
    let result = inspect(&config, Path::new("/unused"));
    assert!(
        result
            .iter()
            .any(|f| f.name == "shared scope" && f.status == Status::Warn)
    );
    assert!(
        result
            .iter()
            .any(|f| f.name == "overlapping prefixes" && f.status == Status::Warn)
    );
}

#[test]
fn different_hosts_do_not_share_a_root() {
    let config = config(
        "[pools.a]\nprovider = 'ssh'\nslots = 1\n[pools.a.ssh]\nhost = 'one'\n[pools.b]\nprovider = 'ssh'\nslots = 1\n[pools.b.ssh]\nhost = 'two'",
    );
    assert!(
        !inspect(&config, Path::new("/unused"))
            .iter()
            .any(|f| f.name == "shared scope")
    );
}

#[test]
fn foreign_and_missing_marks_are_not_healthy() {
    let tmp = tempfile::tempdir().unwrap();
    let id = PoolId::new("test", ID, "a");
    mark(tmp.path(), "current", "a", Some(&id.mailspace));
    mark(tmp.path(), "other", "a", Some("elsewhere-87654321"));
    mark(tmp.path(), "legacy", "a", Some("test"));
    mark(tmp.path(), "unknown", "a", None);
    mark(tmp.path(), "unrelated", "b", Some("elsewhere"));
    let facts = scan_root(tmp.path(), &id).unwrap();
    assert_eq!((facts.own, facts.foreign, facts.unknown), (1, 2, 1));
    let result = ownership_finding("a", &facts);
    assert_eq!(result.status, Status::Warn);
    assert!(result.detail.contains("2 owned by another mailspace"));
    assert!(!result.detail.contains("legacy"));
    assert!(!result.detail.contains("elsewhere"));
}

#[test]
fn missing_and_invalid_roots_are_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let id = PoolId::new("test", ID, "a");
    assert_eq!(
        ownership(&id, &tmp.path().join("missing")).status,
        Status::Warn
    );
    let file = tmp.path().join("file");
    fs::write(&file, "not a root").unwrap();
    assert_eq!(ownership(&id, &file).status, Status::Warn);
}

#[test]
fn known_owned_slots_are_ok_without_claiming_availability() {
    let tmp = tempfile::tempdir().unwrap();
    let id = PoolId::new("test", ID, "a");
    mark(tmp.path(), "current", "a", Some(&id.mailspace));
    let result = ownership(&id, tmp.path());
    assert_eq!(result.status, Status::Ok);
    assert!(result.detail.contains("1 owned"));
    assert!(!result.detail.contains("free"));
}

#[test]
fn local_root_matches_runtime_cwd_resolution() {
    let config = config("[pools.a]\nprovider = 'local'\nslots = 1");
    assert_eq!(
        local_root(&config.pools["a"], Path::new("/cwd")),
        Path::new("/cwd/.vivi/pool")
    );
}

#[test]
fn read_only_inspection_does_not_create_identity_or_ignore_file() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join(".vivi")).unwrap();
    let path = tmp.path().join(".vivi/mailspace.toml");
    let original = "name = 'legacy'\n";
    fs::write(&path, original).unwrap();
    let result = findings(Some(tmp.path()));
    assert_eq!(result[0].status, Status::Skipped);
    assert_eq!(fs::read_to_string(path).unwrap(), original);
    assert!(!tmp.path().join(".vivi/mailspace.id").exists());
    assert!(!tmp.path().join(".vivi/.gitignore").exists());
}

#[test]
fn broken_config_does_not_echo_secrets() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join(".vivi")).unwrap();
    fs::write(
        tmp.path().join(".vivi/mailspace.toml"),
        "secret = 'VERY-PRIVATE\n",
    )
    .unwrap();
    let result = findings(Some(tmp.path()));
    assert_eq!(result[0].status, Status::Fail);
    assert!(!format!("{result:?}").contains("VERY-PRIVATE"));
}

#[test]
fn conflicting_or_unreadable_identity_warns() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join(".vivi")).unwrap();
    fs::write(
        tmp.path().join(".vivi/mailspace.id"),
        "87654321-1234-1234-1234-123456789abc",
    )
    .unwrap();
    assert!(read_identity(tmp.path(), &mut config("")).is_err());
    fs::write(tmp.path().join(".vivi/mailspace.id"), "invalid").unwrap();
    assert!(read_identity(tmp.path(), &mut config("")).is_err());
}

#[test]
fn restored_identity_is_read_without_writing() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join(".vivi")).unwrap();
    fs::write(tmp.path().join(".vivi/mailspace.id"), ID).unwrap();
    let mut config = config("");
    config.id = None;
    read_identity(tmp.path(), &mut config).unwrap();
    assert_eq!(config.id.as_deref(), Some(ID));
    assert!(!tmp.path().join(".vivi/mailspace.toml").exists());
}

#[test]
fn symlink_or_scan_limit_is_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink("/unused", tmp.path().join("linked")).unwrap();
    let id = PoolId::new("test", ID, "a");
    assert_eq!(ownership(&id, tmp.path()).status, Status::Warn);
    let mut remaining = 0;
    assert!(directories(tmp.path(), &mut remaining).is_err());
}

#[test]
fn missing_stable_identity_never_reports_ownership_ok() {
    let mut config = config("[pools.a]\nprovider = 'local'\nslots = 1");
    config.id = None;
    let result = inspect(&config, Path::new("/unused"));
    let ownership = result.iter().find(|f| f.name == "ownership a").unwrap();
    assert_eq!(ownership.status, Status::Warn);
    assert!(ownership.detail.contains("unknown"));
}

#[test]
fn oversized_and_linked_marks_are_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let id = PoolId::new("test", ID, "a");
    mark(tmp.path(), "one", "a", Some(&"x".repeat(513)));
    let mark_path = tmp.path().join("one/a/slot-1/marks/vivi_pool_mailspace");
    assert_eq!(ownership(&id, tmp.path()).status, Status::Warn);
    fs::remove_file(&mark_path).unwrap();
    std::os::unix::fs::symlink("/unused", &mark_path).unwrap();
    assert_eq!(ownership(&id, tmp.path()).status, Status::Warn);
}
