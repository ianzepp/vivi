use super::*;
use crate::mailspace::config_edit::{self, PoolAdd};

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
fn a_fresh_mailspace_gets_the_three_flow_roles_with_their_charters() {
    let tmp = tempfile::tempdir().unwrap();
    let _ = Mailspace::init(Some(tmp.path())).unwrap();
    add_local_pool(tmp.path(), "workers");
    let mut mailspace = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    mailspace.ensure_flow_roles().unwrap();
    for name in ["researcher", "admitter", "slicer"] {
        let view = mailspace.role_view(name).unwrap();
        assert_eq!(view.kind.as_deref(), Some(name));
        assert_eq!(view.pool.as_deref(), Some("workers"));
        assert_eq!(view.read_only, Some(true));
        assert_eq!(Some(view.charter.as_str()), shipped_charter(name));
        assert!(view.charter.contains("ANSWER: <word>"));
    }
}

#[test]
fn an_existing_charter_is_never_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let _ = Mailspace::init(Some(tmp.path())).unwrap();
    let mut mailspace = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    mailspace.add_role("slicer", RoleUpdate::default()).unwrap();
    mailspace.set_charter("slicer", "my own slicer").unwrap();
    mailspace.ensure_flow_roles().unwrap();
    assert_eq!(
        mailspace.role_view("slicer").unwrap().charter,
        "my own slicer"
    );
    assert_eq!(
        mailspace.role_view("admitter").unwrap().charter,
        shipped_charter("admitter").unwrap()
    );
}

#[test]
fn shipped_charter_covers_the_six_shipped_roles() {
    for name in [
        "pump-planner",
        "merge",
        "auditor",
        "researcher",
        "admitter",
        "slicer",
    ] {
        assert!(
            shipped_charter(name).is_some_and(|text| !text.is_empty()),
            "{name}"
        );
    }
    assert!(shipped_charter("hand").is_none());
}
