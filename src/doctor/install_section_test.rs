use std::path::PathBuf;

use super::Finding;
use crate::doctor::Status;
use crate::doctor::install_section::findings;
use crate::skills::SkillCopyStatus;
use crate::update::CheckOutcome;

#[test]
fn available_release_warns_with_update_fix() {
    let out = findings("14.1.0", Some(Ok(&CheckOutcome::Available)), &[]);
    assert_eq!(out[0], Finding::ok("install", "binary", "vivi 14.1.0"));
    assert_eq!(
        out[1],
        Finding::warn(
            "release",
            "release",
            "a newer public release exists",
            "vivi update"
        )
    );
}

#[test]
fn release_error_degrades_to_skipped() {
    let out = findings("14.1.0", Some(Err("github unreachable".to_string())), &[]);
    assert_eq!(out[1].status, Status::Skipped);
    assert_eq!(
        out[1],
        Finding::skipped(
            "release",
            "release",
            "release check unavailable: github unreachable"
        )
    );

    let not_run = findings("14.1.0", None, &[]);
    assert_eq!(
        not_run[1],
        Finding::skipped("release", "release", "release check not run")
    );

    let current = findings("14.1.0", Some(Ok(&CheckOutcome::UpToDate)), &[]);
    assert_eq!(current[1].status, Status::Ok);
    let ahead = findings("14.1.0", Some(Ok(&CheckOutcome::Ahead)), &[]);
    assert_eq!(ahead[1].status, Status::Ok);
}

#[test]
fn stale_skills_warn_and_up_to_date_ok() {
    let targets = vec![
        (
            PathBuf::from("/agents/skills"),
            SkillCopyStatus::Stale(vec!["tugboat".into(), "vivi".into()]),
        ),
        (PathBuf::from("/other/skills"), SkillCopyStatus::UpToDate),
        (PathBuf::from("/bare"), SkillCopyStatus::NotInstalled),
    ];
    let out = findings("14.1.0", None, &targets);

    assert_eq!(
        out[2],
        Finding::warn(
            "skills",
            "skills",
            "/agents/skills: stale copies: tugboat, vivi",
            "vivi skills install /agents/skills --force"
        )
    );
    assert_eq!(
        out[3],
        Finding::ok("skills", "skills", "/other/skills: up to date")
    );
    assert_eq!(
        out[4],
        Finding::skipped("skills", "skills", "/bare: no catalog skill present")
    );
}

#[test]
fn no_targets_skips() {
    let out = findings("14.1.0", None, &[]);
    assert_eq!(
        out[2],
        Finding::skipped("skills", "skills", "no installed skills recorded")
    );
}
