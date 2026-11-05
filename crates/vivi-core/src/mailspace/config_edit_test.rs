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
fn command_and_target_edits_keep_comments_and_clear_only_the_requested_key() {
    let raw = r#"
# roster
[[identities]]
name = "hand"
# keep-hand
command = "vivi board --graph"
"#;
    let mut doc = raw.parse().unwrap();
    patch_identity(
        &mut doc,
        "hand",
        &IdentityPatch {
            command: Some(Some("sh -c 'vivi step'\n  --apply slice:1".into())),
            target: Some(Some("slice:d366cf32-1".into())),
            ..IdentityPatch::default()
        },
    )
    .unwrap();
    let after = doc.to_string();
    assert!(after.contains("# roster"), "{after}");
    assert!(after.contains("# keep-hand"), "{after}");
    assert!(after.contains("command = \"sh -c 'vivi step'\\n  --apply slice:1\""), "{after}");
    assert!(after.contains("target = \"slice:d366cf32-1\""), "{after}");

    patch_identity(
        &mut doc,
        "hand",
        &IdentityPatch {
            target: Some(None),
            ..IdentityPatch::default()
        },
    )
    .unwrap();
    let after = doc.to_string();
    assert!(after.contains("# keep-hand"), "{after}");
    assert!(after.contains("command = \"sh -c"), "{after}");
    assert!(!after.contains("slice:d366cf32-1"), "{after}");
}
