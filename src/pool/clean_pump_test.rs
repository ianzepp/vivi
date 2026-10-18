use super::*;
use crate::mailspace::Mailspace;
use crate::pool::clean_rule::write_baseline;
use crate::pump::state::{State, Status, Unit};
use std::fs;

fn unit(status: Status, task: &str, branch: &str) -> Unit {
    Unit {
        status,
        task: Some(task.into()),
        branch: Some(branch.into()),
        target: "radix".into(),
        ..Unit::default()
    }
}

#[test]
fn merged_is_done_and_does_not_wait_for_main() {
    assert!(is_done(Status::Merged));
    assert!(is_done(Status::NoDefect));
    assert!(is_done(Status::Replaced));
    assert!(is_done(Status::Withdrawn));
    assert!(!is_done(Status::Delivered));
    assert!(!is_done(Status::Merging));
    assert!(!is_done(Status::Inflight));
    assert!(!is_done(Status::Pending));
}

#[test]
fn unit_for_matches_task_or_branch() {
    let mut st = State::default();
    st.units
        .insert("u1".into(), unit(Status::Merged, "abcd1234", "unit/u1-a1"));
    assert_eq!(
        unit_for(&st, Some("abcd1234"), None).map(|u| u.status),
        Some(Status::Merged)
    );
    assert_eq!(
        unit_for(&st, None, Some("unit/u1-a1"))
            .unwrap()
            .task
            .as_deref(),
        Some("abcd1234")
    );
    assert!(unit_for(&st, Some("deadbeef"), None).is_none());
}

#[test]
fn a_merging_unit_holds_its_target() {
    let mut st = State::default();
    st.units
        .insert("u1".into(), unit(Status::Merging, "t", "unit/u1-a1"));
    assert!(target_is_merging(&st, "radix"));
    assert!(!target_is_merging(&st, "default"));
}

#[test]
fn pump_has_no_on_main_status() {
    // Status::Merged is merge-lane / integration-branch success. Landing on
    // main is a later promote (a promotions table), not a unit status.
    // Work-lane cleanup keys on Merged and must not wait for main.
    for name in ["promoted", "on-main", "main"] {
        assert!(
            matches!(Status::from_name(name), Status::Other(_)),
            "{name}"
        );
    }
    assert_eq!(Status::Merged.as_str(), "merged");
    assert!(!is_done(Status::from_name("promoted")));
}

fn pump_space(toml: &str) -> (tempfile::TempDir, Mailspace) {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".vivi")).unwrap();
    fs::write(dir.path().join(".vivi/mailspace.toml"), toml).unwrap();
    let space = Mailspace::discover(Some(dir.path())).unwrap();
    (dir, space)
}

#[test]
fn merge_lane_build_output_obeys_the_2x_rule() {
    let (_tmp, space) =
        pump_space("name = \"t\"\n\n[pump]\nbranch = \"next\"\nhome = \"pump-home\"\n");
    let checkout = space.root.join("pump-home/merge-lanes/radix/1");
    fs::create_dir_all(checkout.join("src")).unwrap();
    fs::write(checkout.join("src/lib.rs"), "ok\n").unwrap();
    fs::create_dir_all(checkout.join("target")).unwrap();
    fs::write(checkout.join("target/blob"), vec![0_u8; 50]).unwrap();
    write_baseline(&checkout, 10).unwrap();
    let (acts, skips) = reclaim_lanes(&space, None, false).unwrap();
    assert!(skips.is_empty(), "{skips:?}");
    assert!(acts.iter().any(|a| a.kind == ActionKind::LaneBuild));
    assert!(!checkout.join("target/blob").exists());
    assert_eq!(
        fs::read_to_string(checkout.join("src/lib.rs")).unwrap(),
        "ok\n"
    );
}

#[test]
fn a_merging_target_keeps_its_lane_build() {
    let (_tmp, space) =
        pump_space("name = \"t\"\n\n[pump]\nbranch = \"next\"\nhome = \"pump-home\"\n");
    let checkout = space.root.join("pump-home/merge-lanes/radix/1");
    fs::create_dir_all(checkout.join("target")).unwrap();
    fs::write(checkout.join("target/blob"), vec![0_u8; 50]).unwrap();
    write_baseline(&checkout, 10).unwrap();
    let mut st = State::default();
    st.units
        .insert("u1".into(), unit(Status::Merging, "t", "unit/u1-a1"));
    let (acts, skips) = reclaim_lanes(&space, Some(&st), false).unwrap();
    assert!(acts.is_empty(), "{acts:?}");
    assert!(skips.iter().any(|s| s.reason.contains("mid-batch")));
    assert!(checkout.join("target/blob").is_file());
}
