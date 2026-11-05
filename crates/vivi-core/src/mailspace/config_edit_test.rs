//! Comment-preserving identity and pool edits.

use super::{IdentityPatch, PoolAdd, insert_identity, patch_identity, upsert_pool};

#[test]
fn identity_and_pool_edits_keep_comments_and_only_write_set_keys() {
    let raw = r#"
# roster
name = "demo"

[[identities]]
name = "hand"
kind = "hand"
# keep-hand
"#;
    let mut doc = raw.parse().unwrap();
    patch_identity(
        &mut doc,
        "hand",
        &IdentityPatch {
            pool: Some(Some("glm".into())),
            read_only: Some(Some(false)),
            ..IdentityPatch::default()
        },
    )
    .unwrap();
    insert_identity(
        &mut doc,
        "gate",
        &IdentityPatch {
            kind: Some(Some("gate".into())),
            pool: Some(Some("glm".into())),
            read_only: Some(Some(true)),
            ..IdentityPatch::default()
        },
    )
    .unwrap();
    upsert_pool(
        &mut doc,
        &PoolAdd {
            name: "glm".into(),
            provider: "local".into(),
            slots: 4,
            worktree_of: Some("~/src".into()),
            repo: None,
            image: None,
            root: None,
        },
    )
    .unwrap();
    let after = doc.to_string();
    assert!(after.contains("# roster"), "{after}");
    assert!(after.contains("# keep-hand"), "{after}");
    assert!(after.contains("pool = \"glm\""), "{after}");
    assert!(after.contains("[pools.glm]"), "{after}");
    assert!(after.contains("worktree_of"), "{after}");
    assert!(!after.contains("image"), "{after}");
    assert!(!after.contains("repo"), "{after}");
}

#[test]
fn command_and_target_patches_keep_multiline_command_and_refuse_inherited_clears() {
    let raw = "[[identities]]\nname = 'hand'\ncommand = 'old'\ntarget = 'task-1'\n";
    let mut doc = raw.parse().unwrap();
    let command = "printf '%s\\n' \"$1\"\n  --flag 'two words'\n";
    patch_identity(
        &mut doc,
        "hand",
        &IdentityPatch {
            command: Some(Some(String::from(command))),
            target: Some(None),
            ..IdentityPatch::default()
        },
    )
    .unwrap();
    let parsed: toml::Value = doc.to_string().parse().unwrap();
    let table = parsed["identities"].as_array().unwrap()[0]
        .as_table()
        .unwrap();
    assert_eq!(table["command"].as_str(), Some(command));
    assert!(table.get("target").is_none());

    let mut inherited: toml_edit::DocumentMut = "name = 'proj'\n".parse().unwrap();
    let error = super::upsert_identity(
        &mut inherited,
        "hand",
        &IdentityPatch {
            command: Some(None),
            ..IdentityPatch::default()
        },
    )
    .unwrap_err();
    assert!(matches!(error, vivi_error::VivariumError::Message(_)));
    assert!(!inherited.to_string().contains("identities"));
}
