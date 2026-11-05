use std::fs;
use std::path::Path;

use crate::mailspace::{Mailspace, RoleUpdate, config_edit, config_edit::PoolAdd};
use vivi_error::VivariumError;

/// A project holding the starter `[pump]` table, whose implicit `default`
/// target is the only valid role target as long as no explicit target exists.
fn pumped() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let (mailspace, _) = Mailspace::init_with_defaults(Some(tmp.path())).unwrap();
    (tmp, mailspace)
}

/// Declare one `[pump.targets.<id>]` repository target in the project file.
fn add_target(root: &Path, id: &str) {
    let path = root.join(".vivi/mailspace.toml");
    let mut raw = fs::read_to_string(&path).unwrap();
    raw.push_str(&format!("\n[pump.targets.{id}]\npath = \"{id}\"\n"));
    fs::write(&path, raw).unwrap();
}

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

#[test]
fn role_command_and_target_default_to_none_on_old_toml() {
    let config: crate::mailspace::MailspaceConfig =
        toml::from_str("name = 'test'\n[[identities]]\nname = 'hand'\nkind = 'hand'\n").unwrap();
    let role = &config.identities[0];
    assert!(role.command.is_none());
    assert!(role.target.is_none());
}

#[test]
fn role_command_and_target_round_trip_through_the_config_file() {
    let (tmp, mut mailspace) = pumped();
    let command = concat!(
        "  printf '%s\\n' \"$1\" \\\n",
        "    --flag 'two words'  \n",
        "echo done\n"
    );
    mailspace
        .add_role(
            "hand",
            RoleUpdate {
                command: Some(Some(command.into())),
                target: Some(Some(String::from("default"))),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    let reopened = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    let view = reopened.role_view("hand").unwrap();
    assert_eq!(view.command.as_deref(), Some(command));
    assert_eq!(view.target.as_deref(), Some("default"));
}

#[test]
fn role_command_and_target_set_and_clear_individually() {
    let (tmp, mut mailspace) = pumped();
    mailspace.add_identity("hand").unwrap();

    mailspace
        .set_role(
            "hand",
            RoleUpdate {
                command: Some(Some(String::from("echo command"))),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    let view = mailspace.role_view("hand").unwrap();
    assert_eq!(view.command.as_deref(), Some("echo command"));
    assert!(view.target.is_none());

    mailspace
        .set_role(
            "hand",
            RoleUpdate {
                target: Some(Some(String::from("default"))),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    let view = mailspace.role_view("hand").unwrap();
    assert_eq!(view.command.as_deref(), Some("echo command"));
    assert_eq!(view.target.as_deref(), Some("default"));

    mailspace
        .set_role(
            "hand",
            RoleUpdate {
                command: Some(None),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    let view = mailspace.role_view("hand").unwrap();
    assert!(view.command.is_none());
    assert_eq!(view.target.as_deref(), Some("default"));

    mailspace
        .set_role(
            "hand",
            RoleUpdate {
                target: Some(None),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    let view = mailspace.role_view("hand").unwrap();
    assert!(view.command.is_none());
    assert!(view.target.is_none());

    let reopened = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    let view = reopened.role_view("hand").unwrap();
    assert!(view.command.is_none());
    assert!(view.target.is_none());
}

#[test]
fn blank_or_nul_command_is_refused_without_mutation() {
    let (tmp, mut mailspace) = pumped();
    mailspace
        .add_role(
            "hand",
            RoleUpdate {
                command: Some(Some(String::from("echo keep"))),
                target: Some(Some(String::from("default"))),
                ..RoleUpdate::default()
            },
        )
        .unwrap();

    for bad in ["   \n\t ", "echo ok\0bad"] {
        let error = mailspace
            .set_role(
                "hand",
                RoleUpdate {
                    command: Some(Some(String::from(bad))),
                    target: Some(Some(String::from("default"))),
                    ..RoleUpdate::default()
                },
            )
            .unwrap_err();
        assert!(matches!(error, vivi_error::VivariumError::Message(_)));
        let view = mailspace.role_view("hand").unwrap();
        assert_eq!(view.command.as_deref(), Some("echo keep"));
        assert_eq!(view.target.as_deref(), Some("default"));
    }

    let reopened = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    let view = reopened.role_view("hand").unwrap();
    assert_eq!(view.command.as_deref(), Some("echo keep"));
    assert_eq!(view.target.as_deref(), Some("default"));
}

#[test]
fn inherited_command_and_target_override_and_an_inherited_clear_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().join(".vivi");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(
        base.join("mailspace.toml"),
        "base = true\nname = 'base'\n[[identities]]\nname = 'hand'\nkind = 'hand'\n\
         command = 'echo base'\ntarget = 'default'\n",
    )
    .unwrap();
    let root = tmp.path().join("work/proj");
    let dir = root.join(".vivi");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("mailspace.toml"), "name = 'proj'\n[pump]\n").unwrap();

    let mut mailspace = Mailspace::open_root(root.clone()).unwrap();
    let view = mailspace.role_view("hand").unwrap();
    assert_eq!(view.command.as_deref(), Some("echo base"));
    assert_eq!(view.target.as_deref(), Some("default"));

    for update in [
        RoleUpdate {
            command: Some(None),
            ..RoleUpdate::default()
        },
        RoleUpdate {
            target: Some(None),
            ..RoleUpdate::default()
        },
    ] {
        let error = mailspace.set_role("hand", update).unwrap_err();
        assert!(matches!(error, vivi_error::VivariumError::Message(_)));
        let view = mailspace.role_view("hand").unwrap();
        assert_eq!(view.command.as_deref(), Some("echo base"));
        assert_eq!(view.target.as_deref(), Some("default"));
    }

    mailspace
        .set_role(
            "hand",
            RoleUpdate {
                command: Some(Some(String::from("echo project"))),
                target: Some(Some(String::from("default"))),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    let reopened = Mailspace::open_root(root.clone()).unwrap();
    let view = reopened.role_view("hand").unwrap();
    assert_eq!(view.command.as_deref(), Some("echo project"));
    assert_eq!(view.target.as_deref(), Some("default"));
}

#[test]
fn a_named_target_the_pump_configures_is_accepted_and_written() {
    let tmp = tempfile::tempdir().unwrap();
    Mailspace::init_with_defaults(Some(tmp.path())).unwrap();
    add_target(tmp.path(), "repo");
    let mut mailspace = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    mailspace
        .add_role(
            "hand",
            RoleUpdate {
                target: Some(Some(String::from("repo"))),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    let reopened = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    let view = reopened.role_view("hand").unwrap();
    assert_eq!(view.target.as_deref(), Some("repo"));
}

#[test]
fn the_implicit_default_target_is_refused_once_the_pump_names_targets() {
    let (tmp, mut mailspace) = pumped();
    mailspace.add_identity("hand").unwrap();
    add_target(tmp.path(), "repo");
    let mut mailspace = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    let config = tmp.path().join(".vivi/mailspace.toml");
    let before = fs::read(&config).unwrap();
    let error = mailspace
        .set_role(
            "hand",
            RoleUpdate {
                target: Some(Some(String::from("default"))),
                ..RoleUpdate::default()
            },
        )
        .unwrap_err();
    assert!(matches!(error, VivariumError::Config(_)), "{error}");
    assert_eq!(fs::read(&config).unwrap(), before);
    assert!(mailspace.role_view("hand").unwrap().target.is_none());
}

#[test]
fn an_unknown_target_is_refused_before_any_field_changes() {
    let (tmp, mut mailspace) = pumped();
    mailspace.add_identity("hand").unwrap();
    let config = tmp.path().join(".vivi/mailspace.toml");
    let before = fs::read(&config).unwrap();
    let error = mailspace
        .set_role(
            "hand",
            RoleUpdate {
                model: Some(Some(String::from("a-model"))),
                target: Some(Some(String::from("missing"))),
                ..RoleUpdate::default()
            },
        )
        .unwrap_err();
    assert!(matches!(error, VivariumError::Config(_)), "{error}");
    assert!(error.to_string().contains("missing"), "{error}");
    assert_eq!(fs::read(&config).unwrap(), before);
    let view = mailspace.role_view("hand").unwrap();
    assert!(view.model.is_none(), "{:?}", view.model);
    assert!(view.target.is_none(), "{:?}", view.target);
    let reopened = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    let view = reopened.role_view("hand").unwrap();
    assert!(view.model.is_none(), "{:?}", view.model);
    assert!(view.target.is_none(), "{:?}", view.target);
}

#[test]
fn a_role_target_is_refused_without_a_pump_table() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    let error = mailspace
        .add_role(
            "hand",
            RoleUpdate {
                target: Some(Some(String::from("default"))),
                ..RoleUpdate::default()
            },
        )
        .unwrap_err();
    assert!(matches!(error, VivariumError::Config(_)), "{error}");
    let reopened = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    assert!(reopened.role_view("hand").is_err());
}

#[test]
fn a_target_is_validated_under_both_the_alias_and_the_canonical_name() {
    let (tmp, mut mailspace) = pumped();
    mailspace.add_identity("hand").unwrap();
    mailspace.rename_identity("hand", "dock").unwrap();
    let config = tmp.path().join(".vivi/mailspace.toml");
    let before = fs::read(&config).unwrap();
    for name in ["hand", "dock"] {
        let error = mailspace
            .set_role(
                name,
                RoleUpdate {
                    target: Some(Some(String::from("missing"))),
                    ..RoleUpdate::default()
                },
            )
            .unwrap_err();
        assert!(matches!(error, VivariumError::Config(_)), "{error}");
        assert_eq!(fs::read(&config).unwrap(), before);
    }
    mailspace
        .set_role(
            "hand",
            RoleUpdate {
                target: Some(Some(String::from("default"))),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    let view = mailspace.role_view("dock").unwrap();
    assert_eq!(view.target.as_deref(), Some("default"));
}
