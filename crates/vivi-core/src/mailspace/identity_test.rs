use crate::mailspace::{Mailspace, RoleUpdate, config_edit, config_edit::PoolAdd};

#[test]
fn ensure_merge_role_binds_the_first_local_pool() {
    let tmp = tempfile::tempdir().unwrap();
    let mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    config_edit::apply(&mailspace.config_path(), |doc| {
        config_edit::upsert_pool(
            doc,
            &PoolAdd {
                name: String::from("workers"),
                provider: String::from("local"),
                slots: 2,
                worktree_of: None,
                repo: None,
                image: None,
                root: None,
            },
        )
    })
    .unwrap();
    let mut mailspace = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    mailspace.ensure_merge_role().unwrap();
    let view = mailspace.role_view("merge").unwrap();
    assert_eq!(view.kind.as_deref(), Some("merge"));
    assert_eq!(view.pool.as_deref(), Some("workers"));
}

#[test]
fn ensure_merge_role_leaves_pool_unset_without_a_local_pool() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.ensure_merge_role().unwrap();
    let view = mailspace.role_view("merge").unwrap();
    assert_eq!(view.kind.as_deref(), Some("merge"));
    assert!(view.pool.is_none(), "unexpected pool {:?}", view.pool);
}

#[test]
fn ensure_merge_role_preserves_an_existing_pool_binding() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.ensure_merge_role().unwrap();
    mailspace
        .set_role(
            "merge",
            RoleUpdate {
                pool: Some(Some(String::from("fly"))),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    config_edit::apply(&mailspace.config_path(), |doc| {
        config_edit::upsert_pool(
            doc,
            &PoolAdd {
                name: String::from("workers"),
                provider: String::from("local"),
                slots: 1,
                worktree_of: None,
                repo: None,
                image: None,
                root: None,
            },
        )
    })
    .unwrap();
    let mut mailspace = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    mailspace.ensure_merge_role().unwrap();
    assert_eq!(
        mailspace.role_view("merge").unwrap().pool.as_deref(),
        Some("fly")
    );
}

fn add_local_pool(root: &std::path::Path, name: &str) {
    let path = root.join(".vivi").join("mailspace.toml");
    config_edit::apply(&path, |doc| {
        config_edit::upsert_pool(
            doc,
            &PoolAdd {
                name: name.to_string(),
                provider: String::from("local"),
                slots: 1,
                worktree_of: None,
                repo: None,
                image: None,
                root: None,
            },
        )
    })
    .unwrap();
}

#[test]
fn ensure_pump_planner_role_creates_an_active_planner_with_the_shipped_charter() {
    let tmp = tempfile::tempdir().unwrap();
    let _ = Mailspace::init(Some(tmp.path())).unwrap();
    add_local_pool(tmp.path(), "workers");
    let mut mailspace = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    mailspace.ensure_pump_planner_role().unwrap();
    let view = mailspace.role_view("pump-planner").unwrap();
    assert_eq!(view.kind.as_deref(), Some("planner"));
    assert_eq!(view.status, "active");
    assert_eq!(view.pool.as_deref(), Some("workers"));
    assert_eq!(
        view.charter,
        crate::mailspace::shipped::PUMP_PLANNER_CHARTER
    );
    assert!(view.charter.contains("answer = \"planned\""));
    assert!(!view.charter.contains("PUMP-PLAN-BEGIN"));
}

#[test]
fn ensure_pump_planner_role_takes_the_pool_of_the_hand_role() {
    let tmp = tempfile::tempdir().unwrap();
    let _ = Mailspace::init(Some(tmp.path())).unwrap();
    add_local_pool(tmp.path(), "aaa");
    add_local_pool(tmp.path(), "zzz");
    let path = tmp.path().join(".vivi").join("mailspace.toml");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("\n[pump]\nbranch = \"next\"\nto = \"hand\"\n");
    std::fs::write(&path, text).unwrap();
    let mut mailspace = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    mailspace
        .add_role(
            "hand",
            RoleUpdate {
                kind: Some(Some(String::from("hand"))),
                pool: Some(Some(String::from("zzz"))),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    mailspace.ensure_pump_planner_role().unwrap();
    assert_eq!(
        mailspace.role_view("pump-planner").unwrap().pool.as_deref(),
        Some("zzz")
    );
}

#[test]
fn ensure_pump_planner_role_never_overwrites_a_charter_or_a_pool() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace
        .add_role(
            "pump-planner",
            RoleUpdate {
                kind: Some(Some(String::from("planner"))),
                pool: Some(Some(String::from("mine"))),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    mailspace
        .set_charter("pump-planner", "my own planning rules\n")
        .unwrap();
    add_local_pool(tmp.path(), "workers");
    let mut mailspace = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    mailspace.ensure_pump_planner_role().unwrap();
    mailspace.ensure_pump_planner_role().unwrap();
    let view = mailspace.role_view("pump-planner").unwrap();
    assert_eq!(view.charter, "my own planning rules\n");
    assert_eq!(view.pool.as_deref(), Some("mine"));
    assert_eq!(view.kind.as_deref(), Some("planner"));
}

#[test]
fn ensure_pump_planner_role_fills_a_blank_charter_and_leaves_the_planner_seat_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace
        .add_role("planner", RoleUpdate::default())
        .unwrap();
    mailspace
        .set_charter("planner", "Report to the Mind.\n")
        .unwrap();
    mailspace
        .add_role("pump-planner", RoleUpdate::default())
        .unwrap();
    mailspace.set_charter("pump-planner", "  \n").unwrap();
    mailspace.ensure_pump_planner_role().unwrap();
    let view = mailspace.role_view("pump-planner").unwrap();
    assert_eq!(
        view.charter,
        crate::mailspace::shipped::PUMP_PLANNER_CHARTER
    );
    assert_eq!(view.kind.as_deref(), Some("planner"));
    assert_eq!(
        mailspace.role_view("planner").unwrap().charter,
        "Report to the Mind.\n"
    );
}

#[test]
fn role_named_matches_name_and_alias_ignoring_case() {
    let config: crate::mailspace::MailspaceConfig =
        toml::from_str("name = 'test'\n[[identities]]\nname = 'Slicer'\naliases = ['cutter']\n")
            .unwrap();
    for name in ["slicer", "SLICER", "Cutter"] {
        assert_eq!(
            config.role_named(name).map(|role| role.name.as_str()),
            Some("Slicer")
        );
    }
    assert!(config.role_named("unknown").is_none());
}
