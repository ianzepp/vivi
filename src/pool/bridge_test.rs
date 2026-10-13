use super::*;

#[test]
fn the_label_names_the_pool_and_the_job() {
    assert_eq!(label_for("wt", "abc123456789"), "pool-wt-abc123456789");
}

#[test]
fn an_over_long_pool_is_truncated_but_the_job_id_survives() {
    let label = label_for(&"p".repeat(200), "abc123456789");
    assert_eq!(label.chars().count(), 64);
    assert!(label.starts_with("pool-pppp"), "{label}");
    assert!(label.ends_with("-abc123456789"), "{label}");
}

#[test]
fn a_long_job_id_still_fits_the_cap() {
    let job = "f".repeat(40);
    let label = label_for("poolname", &job);
    assert!(label.chars().count() <= 64, "{label}");
    assert!(label.ends_with(&job), "{label}");
}
