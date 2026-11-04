use super::{DoctorReport, Finding, Status, Verdict};

#[test]
fn verdict_is_worst_of_non_skipped() {
    let fail_and_warn = DoctorReport::from_findings(vec![
        Finding::warn("a", "warn", "warning", "fix"),
        Finding::fail("a", "fail", "failure", "fix"),
    ]);
    assert_eq!(fail_and_warn.verdict, Verdict::Fail);

    let warn_ok_skipped = DoctorReport::from_findings(vec![
        Finding::warn("a", "warn", "warning", "fix"),
        Finding::ok("a", "ok", "fine"),
        Finding::skipped("a", "skipped", "not run"),
    ]);
    assert_eq!(warn_ok_skipped.verdict, Verdict::Warn);

    let only_ok_and_skipped = DoctorReport::from_findings(vec![
        Finding::ok("a", "ok", "fine"),
        Finding::skipped("a", "skipped", "not run"),
    ]);
    assert_eq!(only_ok_and_skipped.verdict, Verdict::Ok);
    assert_eq!(DoctorReport::from_findings(Vec::new()).verdict, Verdict::Ok);
}

#[test]
fn findings_rank_fail_then_warn_then_ok_then_skipped() {
    let report = DoctorReport::from_findings(vec![
        Finding::ok("a", "ok-first", "fine"),
        Finding::skipped("a", "skip-first", "not run"),
        Finding::fail("a", "fail-first", "failure", "fix"),
        Finding::warn("a", "warn-first", "warning", "fix"),
        Finding::fail("a", "fail-second", "failure", "fix"),
        Finding::ok("a", "ok-second", "fine"),
        Finding::skipped("a", "skip-second", "not run"),
        Finding::warn("a", "warn-second", "warning", "fix"),
    ]);
    let ordered: Vec<_> = report
        .findings
        .iter()
        .map(|finding| (finding.status, finding.name.as_str()))
        .collect();
    assert_eq!(
        ordered,
        vec![
            (Status::Fail, "fail-first"),
            (Status::Fail, "fail-second"),
            (Status::Warn, "warn-first"),
            (Status::Warn, "warn-second"),
            (Status::Ok, "ok-first"),
            (Status::Ok, "ok-second"),
            (Status::Skipped, "skip-first"),
            (Status::Skipped, "skip-second"),
        ]
    );
}

#[test]
fn verdict_word_roundtrip() {
    assert_eq!(Verdict::Ok.word(), "ok");
    assert_eq!(Verdict::Warn.word(), "warn");
    assert_eq!(Verdict::Fail.word(), "fail");
}
