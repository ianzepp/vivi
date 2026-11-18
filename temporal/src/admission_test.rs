use super::*;

fn policy(hot: &[&str], max_backlog_ms: u64, land_ms_per_unit: u64) -> AdmissionPolicy {
    AdmissionPolicy {
        hot_paths: hot.iter().map(|s| s.to_string()).collect(),
        max_backlog_ms,
        land_ms_per_unit,
    }
}

fn req(key: &str, scope: &[&str]) -> AdmitRequest {
    AdmitRequest {
        reply_to: key.to_string(),
        write_scope: scope.iter().map(|s| s.to_string()).collect(),
    }
}

fn keys(granted: &[AdmitRequest]) -> Vec<&str> {
    granted.iter().map(|r| r.reply_to.as_str()).collect()
}

#[test]
fn backpressure_holds_new_slices_over_the_limit() {
    // One hour of landing at three minutes a unit is 20 units.
    let p = policy(&[], 3_600_000, 180_000);
    assert!(!backpressured(&p, 20));
    assert!(backpressured(&p, 21));
    let mut a = Admission::default();
    a.request(req("s1", &["src/a.rs"]));
    assert!(!a.has_grantable(&p, 21));
    assert!(a.grant(&p, 21).is_empty());
    assert_eq!(keys(&a.grant(&p, 20)), vec!["s1"]);
    assert!(a.waiting.is_empty());
}

#[test]
fn zero_limit_turns_backpressure_off() {
    let p = policy(&[], 0, 180_000);
    assert!(!backpressured(&p, 10_000));
}

#[test]
fn overlap_is_by_whole_path_components() {
    assert!(overlaps(
        "crates/radix-mir-clif/src/lib.rs",
        "crates/radix-mir-clif"
    ));
    assert!(overlaps("crates/radix-mir-clif/", "crates/radix-mir-clif"));
    assert!(overlaps("crates", "crates/radix-mir-clif"));
    assert!(!overlaps(
        "crates/radix-mir-clifx/src",
        "crates/radix-mir-clif"
    ));
    assert!(!overlaps("corpus/other", "corpus/reference"));
}

#[test]
fn hot_file_mutex_serializes_overlapping_slices() {
    let p = policy(&["crates/radix-mir-clif", "corpus/reference"], 0, 0);
    let mut a = Admission::default();
    a.request(req("s1", &["crates/radix-mir-clif/src/emit.rs"]));
    a.request(req("s2", &["crates/radix-mir-clif/src/lower.rs"]));
    a.request(req("s3", &["crates/radix-cli/src/main.rs"]));
    assert_eq!(keys(&a.grant(&p, 0)), vec!["s1", "s3"]);
    // s2 waits on s1's hot path, and nothing else frees it.
    assert!(!a.has_grantable(&p, 0));
    a.release("s3");
    assert!(a.grant(&p, 0).is_empty());
    a.release("s1");
    assert_eq!(keys(&a.grant(&p, 0)), vec!["s2"]);
    assert_eq!(a.held.len(), 1);
    assert_eq!(a.held[0].hot, vec!["crates/radix-mir-clif".to_string()]);
}

#[test]
fn a_granted_slice_asking_again_is_granted_again() {
    let p = policy(&["hot"], 1, 1);
    let mut a = Admission::default();
    a.request(req("s1", &["hot/x"]));
    assert_eq!(keys(&a.grant(&p, 0)), vec!["s1"]);
    // Backlogged and the path is held, but by s1 itself.
    a.request(req("s1", &["hot/x"]));
    assert_eq!(keys(&a.grant(&p, 99)), vec!["s1"]);
    assert_eq!(a.held.len(), 1);
}

#[test]
fn duplicate_requests_queue_once() {
    let mut a = Admission::default();
    a.request(req("s1", &[]));
    a.request(req("s1", &[]));
    assert_eq!(a.waiting.len(), 1);
}
