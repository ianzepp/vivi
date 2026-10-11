use std::collections::BTreeMap;

use super::*;
use crate::pool::fake_provider::{Fake, claimed, slot};
use crate::pool::record::DispatchRecord;
use crate::pool::slot::{META_CLAIMED, META_STATE};

const NOW: u64 = 100_000;
const PROVIDER: &str = "Provider (Fake)";

fn config(slots: u32) -> PoolConfig {
    toml::from_str(&format!(
        r#"
repo = "o/r"
slots = {slots}
image = "debian:12"
build_dir = "target"
[fly]
app = "a"
[github]
app_id = 1
private_key = "k.pem"
"#
    ))
    .unwrap()
}

fn view_ok() -> GithubView {
    GithubView {
        openssl: Some(Ok("OpenSSL 3".into())),
        key: Some(Ok("k.pem".into())),
        token: Some(Ok(())),
        branch: Some(Ok("main".into())),
        protection: Some(Ok(true)),
        behind: BTreeMap::from([("a".repeat(40), Ok(3))]),
    }
}

fn open_dispatch(task: &str, machine: &str, at: u64, budget: u64) -> OpenDispatch {
    OpenDispatch {
        task: task.into(),
        record: DispatchRecord {
            slot: "p-slot-1".into(),
            machine: machine.into(),
            job: "j1".into(),
            model: "m/x".into(),
            from: "main".into(),
            branch: "factory/a".into(),
            start: "f".repeat(40),
            budget,
            at,
        },
    }
}

fn fake_with(machines: Vec<crate::pool::provider::MachineRecord>) -> Fake {
    let fake = Fake::default();
    fake.doctor_checks
        .replace(vec![Check::ok(PROVIDER, "login", "logged in")]);
    for m in machines {
        fake.add(m);
    }
    fake
}

fn report(fake: &Fake, cfg: &PoolConfig, open: &[OpenDispatch], view: GithubView) -> Report {
    let github = move |_: &PoolConfig, _: &[String]| view.clone();
    run(&Inputs {
        provider: fake,
        config: cfg,
        mailspace_name: "p",
        open,
        now: NOW,
        github: &github,
    })
}

fn find<'a>(report: &'a Report, name: &str) -> &'a Check {
    report
        .checks
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no check {name} in {:#?}", report.checks))
}

fn assert_read_only(fake: &Fake) {
    assert!(fake.created.borrow().is_empty());
    assert!(fake.destroyed.borrow().is_empty());
    assert!(fake.calls.borrow().is_empty());
    assert!(fake.files.borrow().is_empty());
    assert!(fake.leases.borrow().is_empty());
}

#[test]
fn a_healthy_pool_passes_with_every_check_ok() {
    let fake = fake_with(vec![
        slot("m1", "p-slot-1", "stopped", "p", 1),
        slot("m2", "p-slot-2", "stopped", "p", 2),
    ]);
    let r = report(&fake, &config(2), &[], view_ok());
    assert!(r.passed(), "{}", r.render());
    assert_eq!(r.counts().warn + r.counts().fail + r.counts().skipped, 0);
    assert!(
        find(&r, "slot p-slot-1")
            .detail
            .contains("3 commits behind")
    );
    assert!(find(&r, "branch protection").detail.contains("not checked"));
    assert_read_only(&fake);
}

#[test]
fn an_empty_pool_warns_with_the_create_command_and_still_passes() {
    let fake = fake_with(vec![]);
    let r = report(&fake, &config(2), &[], view_ok());
    let count = find(&r, "slot count");
    assert_eq!(count.status, Status::Warn);
    assert_eq!(count.fix.as_deref(), Some("vivi pool create"));
    assert!(r.passed());
}

#[test]
fn too_many_slots_warns() {
    let fake = fake_with(vec![
        slot("m1", "p-slot-1", "stopped", "p", 1),
        slot("m2", "p-slot-2", "stopped", "p", 2),
    ]);
    let r = report(&fake, &config(1), &[], view_ok());
    assert_eq!(find(&r, "slot count").status, Status::Warn);
    assert!(find(&r, "slot count").detail.contains("2 of 1"));
}

#[test]
fn a_failed_provider_check_skips_the_slot_checks_but_not_the_other_sides() {
    let fake = fake_with(vec![slot("m1", "p-slot-1", "stopped", "p", 1)]);
    fake.doctor_checks.replace(vec![Check::fail(
        PROVIDER,
        "login",
        "not logged in",
        "fly auth login",
    )]);
    let r = report(&fake, &config(1), &[], view_ok());
    assert!(!r.passed());
    assert_eq!(find(&r, "slots").status, Status::Skipped);
    assert_eq!(find(&r, "token").status, Status::Ok);
    assert_eq!(find(&r, "config").status, Status::Ok);
    assert_eq!(
        *fake.lists.borrow(),
        0,
        "no machine listing after a failed login"
    );
}

#[test]
fn a_listing_failure_is_a_fail_not_a_skip() {
    let r = {
        // The Fake cannot fail a listing, so the pool checks take the `Err`.
        let facts = doctor_pool::PoolFacts {
            config: Ok(()),
            max: 1,
            slots: Some(Err("boom".into())),
            open: &[],
            now: NOW,
            github: &view_ok(),
        };
        Report {
            checks: doctor_pool::checks(&facts),
        }
    };
    assert_eq!(find(&r, "slots").status, Status::Fail);
    assert!(
        find(&r, "slots")
            .fix
            .as_deref()
            .unwrap()
            .contains("vivi pool status")
    );
}

#[test]
fn no_token_skips_branch_protection_and_behind() {
    let mut view = view_ok();
    view.token = Some(Err("HTTP 401".into()));
    view.branch = None;
    view.protection = None;
    view.behind.clear();
    let fake = fake_with(vec![slot("m1", "p-slot-1", "stopped", "p", 1)]);
    let r = report(&fake, &config(1), &[], view);
    assert_eq!(find(&r, "token").status, Status::Fail);
    assert!(find(&r, "token").fix.as_deref().unwrap().contains("app_id"));
    assert_eq!(find(&r, "default branch").status, Status::Skipped);
    assert_eq!(find(&r, "branch protection").status, Status::Skipped);
    assert!(find(&r, "slot p-slot-1").detail.contains("behind: unknown"));
}

#[test]
fn a_missing_key_fails_and_skips_the_token() {
    let view = GithubView {
        openssl: Some(Ok("OpenSSL 3".into())),
        key: Some(Err("cannot read k.pem".into())),
        ..GithubView::default()
    };
    let r = report(&fake_with(vec![]), &config(1), &[], view);
    assert_eq!(find(&r, "app key").status, Status::Fail);
    assert!(
        find(&r, "app key")
            .fix
            .as_deref()
            .unwrap()
            .contains("private_key")
    );
    assert_eq!(find(&r, "token").status, Status::Skipped);
}

#[test]
fn an_unprotected_default_branch_warns_and_an_unreadable_one_is_not_checked() {
    let mut view = view_ok();
    view.protection = Some(Ok(false));
    let r = report(&fake_with(vec![]), &config(1), &[], view.clone());
    assert_eq!(find(&r, "branch protection").status, Status::Warn);
    view.protection = Some(Err("HTTP 403".into()));
    let r = report(&fake_with(vec![]), &config(1), &[], view);
    let check = find(&r, "branch protection");
    assert_eq!(check.status, Status::Skipped);
    assert!(check.detail.starts_with("not checked"));
    assert!(r.passed());
}

#[test]
fn a_running_slot_with_no_dispatch_is_a_leak_and_nothing_is_repaired() {
    let fake = fake_with(vec![slot("m1", "p-slot-1", "started", "p", 1)]);
    let r = report(&fake, &config(1), &[], view_ok());
    let leak = find(&r, "leaked runners");
    assert_eq!(leak.status, Status::Fail);
    assert_eq!(leak.fix.as_deref(), Some("vivi pool reconcile"));
    assert!(leak.detail.contains("p-slot-1"));
    assert!(!r.passed());
    assert_read_only(&fake);
    assert_eq!(fake.state_of("m1"), "started");
}

#[test]
fn a_running_slot_with_an_open_dispatch_is_not_a_leak() {
    let mut machine = claimed("m1", "p-slot-1", "started", "p", "t1");
    machine
        .metadata
        .insert(META_CLAIMED.into(), (NOW - 60).to_string());
    let fake = fake_with(vec![machine]);
    let open = [open_dispatch("t1", "m1", NOW - 60, 600)];
    let r = report(&fake, &config(1), &open, view_ok());
    assert_eq!(find(&r, "leaked runners").status, Status::Ok);
    assert_eq!(find(&r, "overdue").status, Status::Ok);
    assert!(find(&r, "slot p-slot-1").detail.contains("60s of 600s"));
    assert!(r.passed());
}

#[test]
fn a_fresh_claim_that_is_still_being_set_up_is_not_a_leak() {
    let mut machine = claimed("m1", "p-slot-1", "started", "p", "t1");
    machine
        .metadata
        .insert(META_CLAIMED.into(), (NOW - 20).to_string());
    let r = report(&fake_with(vec![machine]), &config(1), &[], view_ok());
    assert_eq!(find(&r, "leaked runners").status, Status::Ok);
    assert_eq!(find(&r, "stale claims").status, Status::Ok);
}

#[test]
fn an_open_dispatch_whose_machine_is_gone_is_an_orphan() {
    let fake = fake_with(vec![slot("m1", "p-slot-1", "stopped", "p", 1)]);
    let open = [open_dispatch("t9", "m-gone", NOW - 30, 600)];
    let r = report(&fake, &config(1), &open, view_ok());
    let orphan = find(&r, "orphaned dispatches");
    assert_eq!(orphan.status, Status::Fail);
    assert!(orphan.detail.contains("t9") && orphan.detail.contains("m-gone"));
    let fix = orphan.fix.as_deref().unwrap();
    assert!(fix.starts_with("vivi pool reconcile"), "{fix}");
    assert!(fix.contains("records each as lost and closes it"), "{fix}");
}

#[test]
fn only_a_failure_outside_the_repository_and_pool_groups_is_a_provider_failure() {
    let mut r = Report {
        checks: vec![
            Check::fail(doctor_repo::GROUP, "token", "d", "f"),
            Check::fail(doctor_pool::GROUP, "config", "d", "f"),
            Check::warn("Provider (Fake)", "login", "d", "f"),
        ],
    };
    assert!(!r.provider_failed());
    r.checks
        .push(Check::fail("Provider (Fake)", "login", "d", "f"));
    assert!(r.provider_failed());
}

#[test]
fn a_timed_check_shows_its_duration_in_text_and_json() {
    let r = Report {
        checks: vec![
            Check::ok("g", "step", "fine").timed(std::time::Duration::from_millis(12_340)),
            Check::ok("g", "plain", "fine"),
        ],
    };
    assert!(r.render().contains("fine (12.3s)"));
    assert!(!r.render().contains("plain  fine ("));
    let json: serde_json::Value = serde_json::from_str(&r.to_json().unwrap()).unwrap();
    assert_eq!(json["checks"][0]["millis"], 12_340);
    assert!(json["checks"][1].get("millis").is_none());
}

#[test]
fn a_claim_with_no_dispatch_is_stale_and_its_age_is_shown() {
    let mut machine = claimed("m1", "p-slot-1", "stopped", "p", "t1");
    machine
        .metadata
        .insert(META_CLAIMED.into(), (NOW - 3600).to_string());
    let r = report(&fake_with(vec![machine]), &config(1), &[], view_ok());
    let stale = find(&r, "stale claims");
    assert_eq!(stale.status, Status::Warn);
    assert!(stale.detail.contains("t1") && stale.detail.contains("60m ago"));
    assert_eq!(stale.fix.as_deref(), Some("vivi pool reconcile"));
    assert!(r.passed());
}

#[test]
fn a_busy_slot_past_its_budget_is_overdue() {
    let mut machine = claimed("m1", "p-slot-1", "started", "p", "t1");
    machine
        .metadata
        .insert(META_CLAIMED.into(), (NOW - 900).to_string());
    let open = [open_dispatch("t1", "m1", NOW - 900, 600)];
    let r = report(&fake_with(vec![machine]), &config(1), &open, view_ok());
    let late = find(&r, "overdue");
    assert_eq!(late.status, Status::Warn);
    assert!(late.detail.contains("900s of 600s"));
    assert!(late.fix.is_some());
}

#[test]
fn slot_states_map_to_outcomes_with_fixes() {
    let mut unseeded = slot("m1", "p-slot-1", "stopped", "p", 1);
    unseeded.metadata.remove(META_STATE);
    let mut seeding = slot("m2", "p-slot-2", "started", "p", 2);
    seeding.metadata.remove(META_STATE);
    let finished = claimed("m3", "p-slot-3", "stopped", "p", "t3");
    let r = report(
        &fake_with(vec![unseeded, seeding, finished]),
        &config(3),
        &[],
        view_ok(),
    );
    let bad = find(&r, "slot p-slot-1");
    assert_eq!(bad.status, Status::Fail);
    assert!(
        bad.fix
            .as_deref()
            .unwrap()
            .contains("vivi pool destroy --slot p-slot-1 --yes")
    );
    assert_eq!(find(&r, "slot p-slot-2").status, Status::Warn);
    let waiting = find(&r, "slot p-slot-3");
    assert_eq!(waiting.status, Status::Warn);
    assert_eq!(waiting.fix.as_deref(), Some("vivi pool reconcile"));
}

#[test]
fn an_invalid_config_fails_its_check_and_the_rest_still_runs() {
    let mut cfg = config(1);
    cfg.repo = "widgets".into();
    let r = report(&fake_with(vec![]), &cfg, &[], view_ok());
    let check = find(&r, "config");
    assert_eq!(check.status, Status::Fail);
    assert!(check.detail.contains("owner/name"));
    assert!(check.fix.as_deref().unwrap().contains("mailspace.toml"));
    assert_eq!(find(&r, "token").status, Status::Ok);
}

#[test]
fn every_warn_and_fail_names_a_fix_and_only_those_do() {
    let mut machine = slot("m1", "p-slot-1", "started", "p", 1);
    machine.metadata.remove(META_STATE);
    let stale = claimed("m2", "p-slot-2", "stopped", "p", "t2");
    let mut view = view_ok();
    view.protection = Some(Ok(false));
    let open = [open_dispatch("t9", "m-gone", NOW - 30, 600)];
    let r = report(&fake_with(vec![machine, stale]), &config(3), &open, view);
    for check in &r.checks {
        let needs_fix = matches!(check.status, Status::Warn | Status::Fail);
        assert_eq!(check.fix.is_some(), needs_fix, "{check:?}");
    }
    assert!(r.counts().warn > 0 && r.counts().fail > 0);
}

#[test]
fn json_has_the_status_counts_and_one_entry_per_check() {
    let fake = fake_with(vec![slot("m1", "p-slot-1", "started", "p", 1)]);
    let r = report(&fake, &config(1), &[], view_ok());
    let json: serde_json::Value = serde_json::from_str(&r.to_json().unwrap()).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["counts"]["fail"], 1);
    let checks = json["checks"].as_array().unwrap();
    assert_eq!(checks.len(), r.checks.len());
    let leak = checks
        .iter()
        .find(|c| c["name"] == "leaked runners")
        .unwrap();
    assert_eq!(leak["group"], "Pool (Vivi)");
    assert_eq!(leak["status"], "fail");
    assert_eq!(leak["fix"], "vivi pool reconcile");
    let login = checks.iter().find(|c| c["name"] == "login").unwrap();
    assert_eq!(login["status"], "ok");
    assert!(login.get("fix").is_none());
    let skipped = Check::skipped("g", "n", "why");
    assert_eq!(serde_json::to_value(&skipped).unwrap()["status"], "skipped");
}

#[test]
fn text_groups_the_checks_under_three_headings_and_prints_fixes() {
    let fake = fake_with(vec![slot("m1", "p-slot-1", "started", "p", 1)]);
    let text = report(&fake, &config(1), &[], view_ok()).render();
    let provider = text.find("Provider (Fake)").unwrap();
    let repo = text.find("Repository (GitHub)").unwrap();
    let pool = text.find("Pool (Vivi)").unwrap();
    assert!(provider < repo && repo < pool);
    assert!(text.contains("  fail     leaked runners"));
    assert!(text.contains("fix: vivi pool reconcile"));
    assert!(text.trim_end().ends_with("skipped"));
}

#[test]
fn a_passing_report_with_warnings_has_no_failures() {
    let r = Report {
        checks: vec![Check::warn("g", "n", "d", "f"), Check::ok("g", "m", "d")],
    };
    assert!(r.passed());
    let r = Report {
        checks: vec![Check::skipped("g", "n", "d")],
    };
    assert!(r.passed());
    let r = Report {
        checks: vec![Check::fail("g", "n", "d", "f")],
    };
    assert!(!r.passed());
}

#[test]
fn behind_text_covers_every_case() {
    let mut view = GithubView::default();
    view.behind.insert("a".into(), Ok(0));
    view.behind.insert("b".into(), Ok(1));
    view.behind.insert("c".into(), Ok(7));
    view.behind.insert("d".into(), Err("HTTP 404".into()));
    assert_eq!(
        doctor_repo::behind_text(&view, "a"),
        "at the default branch tip"
    );
    assert_eq!(
        doctor_repo::behind_text(&view, "b"),
        "1 commit behind the default branch tip"
    );
    assert_eq!(
        doctor_repo::behind_text(&view, "c"),
        "7 commits behind the default branch tip"
    );
    assert!(doctor_repo::behind_text(&view, "d").contains("HTTP 404"));
    assert!(doctor_repo::behind_text(&view, "e").contains("unknown"));
}
