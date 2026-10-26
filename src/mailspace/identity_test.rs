use crate::error::VivariumError;
use crate::mailspace::{Mailspace, PUMP_ROLE, RoleUpdate, config_edit, config_edit::PoolAdd};

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

fn fresh_mailspace() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    (tmp, mailspace)
}

#[test]
fn pump_resolves_without_a_roster_row_on_an_empty_roster() {
    let (tmp, mailspace) = fresh_mailspace();
    assert!(mailspace.config.identities.is_empty());
    let prefix = mailspace.address_prefix().to_string();
    let legacy = mailspace.legacy_domain();
    for value in [
        "pump".to_string(),
        "PUMP".to_string(),
        " Pump ".to_string(),
        "pump@local".to_string(),
        format!("pump@{prefix}.local"),
        format!("PUMP@{prefix}.LOCAL"),
        format!("pump@{legacy}"),
    ] {
        assert_eq!(
            mailspace.resolve_identity(&value).unwrap(),
            PUMP_ROLE,
            "{value}"
        );
    }
    let error = mailspace
        .resolve_identity("pump@example.com")
        .unwrap_err()
        .to_string();
    assert!(error.contains("external"), "{error}");
    // Resolution rewrote nothing: the roster stays empty and a reopened
    // mailspace resolves the same built-in.
    let reopened = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    assert!(reopened.config.identities.is_empty());
    assert_eq!(reopened.resolve_identity("Pump").unwrap(), PUMP_ROLE);
    let raw = std::fs::read_to_string(tmp.path().join(".vivi/mailspace.toml")).unwrap();
    assert!(!raw.contains("[[identities]]"), "{raw}");
}

#[test]
fn pump_is_owned_locally_but_never_mind_operator_or_hand() {
    let (_tmp, mailspace) = fresh_mailspace();
    assert_eq!(
        mailspace.identity_owning_account("pump").as_deref(),
        Some(PUMP_ROLE)
    );
    assert_eq!(
        mailspace.identity_owning_account("Pump").as_deref(),
        Some(PUMP_ROLE)
    );
    assert!(mailspace.is_local_account("pump"));
    assert!(!mailspace.is_unowned_account("pump"));
    assert!(!mailspace.identity_is_mind("pump"));
    assert!(!mailspace.identity_is_operator("pump"));
    assert!(!mailspace.identity_is_hand("pump"));
    assert!(!mailspace.identity_may_close_for_all("pump"));
}

#[test]
fn the_builtin_pump_wins_over_a_configured_collision_without_rewriting_it() {
    let tmp = tempfile::tempdir().unwrap();
    let _ = Mailspace::init(Some(tmp.path())).unwrap();
    let path = tmp.path().join(".vivi/mailspace.toml");
    let raw = std::fs::read_to_string(&path).unwrap().replace(
        "identities = []\n",
        "[[identities]]\nname = \"pump\"\nkind = \"mind\"\n",
    );
    std::fs::write(&path, &raw).unwrap();

    let mailspace = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();

    assert_eq!(mailspace.resolve_identity("pump").unwrap(), PUMP_ROLE);
    assert!(!mailspace.identity_is_mind("pump"));
    assert!(!mailspace.identity_is_operator("pump"));
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(
        raw.contains("kind = \"mind\""),
        "the row is left as found: {raw}"
    );
}

#[test]
fn reserved_pump_refuses_creation_rename_metadata_and_charter() {
    let (tmp, mut mailspace) = fresh_mailspace();
    mailspace.add_role("mind", RoleUpdate::default()).unwrap();
    let refused = |error: VivariumError| {
        assert!(
            matches!(error, VivariumError::ReservedIdentity(_)),
            "{error}"
        );
    };

    refused(mailspace.add_identity("pump").unwrap_err());
    refused(
        mailspace
            .add_role(
                "PUMP",
                RoleUpdate {
                    kind: Some(Some(String::from("hand"))),
                    ..RoleUpdate::default()
                },
            )
            .unwrap_err(),
    );
    refused(mailspace.rename_identity("mind", "pump").unwrap_err());
    refused(mailspace.rename_identity("pump", "runner").unwrap_err());
    refused(
        mailspace
            .set_role("pump", RoleUpdate::default())
            .unwrap_err(),
    );
    refused(mailspace.set_charter("pump", "own rules\n").unwrap_err());
    refused(
        mailspace
            .set_charter("PUMP@local", "own rules\n")
            .unwrap_err(),
    );

    assert!(!tmp.path().join(".vivi/charters/pump.md").exists());
    let raw = std::fs::read_to_string(tmp.path().join(".vivi/mailspace.toml")).unwrap();
    assert!(!raw.contains("name = \"pump\""), "{raw}");
}

#[test]
fn ordinary_roles_and_operator_provisioning_are_unchanged_by_the_reserved_pump() {
    let (tmp, mut mailspace) = fresh_mailspace();
    mailspace
        .add_role(
            "hand",
            RoleUpdate {
                kind: Some(Some(String::from("hand"))),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    mailspace.ensure_operator_role().unwrap();

    assert_eq!(
        mailspace.role_view("operator").unwrap().kind.as_deref(),
        Some("operator")
    );
    assert!(mailspace.identity_is_operator("operator"));
    assert_eq!(mailspace.resolve_identity("hand").unwrap(), "hand");
    assert!(mailspace.identity_is_hand("hand"));
    assert_eq!(
        mailspace.address_for("pump"),
        format!("pump@{}", mailspace.address_domain())
    );
    // The built-in has no seat: it is not a configurable role.
    assert!(mailspace.role_view("pump").is_err());
    assert!(
        mailspace
            .config
            .identities
            .iter()
            .all(|role| role.name != PUMP_ROLE)
    );
    let reopened = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    assert_eq!(reopened.resolve_identity("pump").unwrap(), PUMP_ROLE);
    assert!(reopened.identity_is_operator("operator"));
    assert!(!reopened.identity_is_operator("pump"));
}
