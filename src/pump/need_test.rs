use crate::pump::need::{
    is_full_sha, quarantine_namespace_prefix, unit_branch, unit_namespace_prefix,
};

#[test]
fn namespaces_are_stable_and_scoped() {
    assert_eq!(unit_branch("u1", 3), "vivi/unit/u1-a3");
    assert_eq!(unit_namespace_prefix(), "vivi/unit/");
    assert_eq!(quarantine_namespace_prefix(), "vivi/quarantine/");
    assert!(is_full_sha(&"a".repeat(40)));
    assert!(!is_full_sha("abc"));
}
