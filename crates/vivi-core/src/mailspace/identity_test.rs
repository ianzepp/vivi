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
#[test]
fn role_command_target_fields_deserialize_with_legacy_defaults() {
    let legacy: crate::mailspace::MailspaceConfig =
        toml::from_str("name = 'test'\n[[identities]]\nname = 'hand'\n").unwrap();
    let role = &legacy.identities[0];
    assert!(role.command.is_none(), "legacy row must default to None");
    assert!(role.target.is_none(), "legacy row must default to None");

    let configured: crate::mailspace::MailspaceConfig = toml::from_str(
        "name = 'test'\n[[identities]]\nname = 'hand'\n\
         command = 'vivi board --graph'\ntarget = 'slice:d366cf32-1'\n",
    )
    .unwrap();
    let role = &configured.identities[0];
    assert_eq!(role.command.as_deref(), Some("vivi board --graph"));
    assert_eq!(role.target.as_deref(), Some("slice:d366cf32-1"));

    let tmp = tempfile::tempdir().unwrap();
    Mailspace::init(Some(tmp.path())).unwrap();
    let path = tmp.path().join(".vivi").join("mailspace.toml");
    let mut raw: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut row = toml::map::Map::new();
    row.insert(String::from("name"), toml::Value::from("hand"));
    row.insert(
        String::from("command"),
        toml::Value::from("vivi board --graph"),
    );
    row.insert(
        String::from("target"),
        toml::Value::from("slice:d366cf32-1"),
    );
    raw["identities"] = toml::Value::Array(vec![toml::Value::Table(row)]);
    std::fs::write(&path, toml::to_string_pretty(&raw).unwrap()).unwrap();
    let mut mailspace = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    let json = serde_json::to_value(mailspace.role_view("hand").unwrap()).unwrap();
    assert_eq!(json["command"], "vivi board --graph");
    assert_eq!(json["target"], "slice:d366cf32-1");

    mailspace.add_role("plain", RoleUpdate::default()).unwrap();
    let json = serde_json::to_value(mailspace.role_view("plain").unwrap()).unwrap();
    assert!(json["command"].is_null());
    assert!(json["target"].is_null());
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
