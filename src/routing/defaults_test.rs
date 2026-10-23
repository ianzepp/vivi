use super::{default_read_only, resolve_read_only};

#[test]
fn flow_kinds_default_to_read_only() {
    for kind in ["researcher", "admitter", "slicer", "Slicer", "ADMITTER"] {
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
