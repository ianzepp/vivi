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
