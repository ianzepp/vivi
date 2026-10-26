use super::{default_read_only, resolve_read_only};

#[test]
fn flow_kinds_default_to_read_only() {
    for kind in [
        "researcher",
        "admitter",
        "slicer",
        "Slicer",
        "ADMITTER",
        "head-cto",
    ] {
        assert!(default_read_only(Some(kind)), "{kind}");
    }
}

#[test]
fn planner_still_defaults_to_writable() {
    assert!(!default_read_only(Some("planner")));
}

#[test]
fn resolve_uses_kind_default_for_admitter() {
    assert!(resolve_read_only(false, None, Some("admitter")));
}

#[test]
fn the_shipped_read_only_flow_roles_draw_no_read_only_warning() {
    let config: crate::mailspace::MailspaceConfig = toml::from_str(
        "name = \"t\"\n\
         [[identities]]\nname = \"head-cto\"\nkind = \"head-cto\"\nread_only = true\n",
    )
    .expect("mailspace toml");
    let issues = crate::routing::validate(&config);
    assert!(
        !issues.iter().any(|i| i.message.contains("read_only")),
        "{issues:?}"
    );
}
