//! Tests for operator withdraws, from queued request through the pump tick.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use crate::cli::PumpWithdrawCommand;
use crate::pump::config::PumpConfig;
use crate::pump::control::project_home as resolve_project_home;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::need;
use crate::pump::state::{NeedRecord, State, Status, Unit};
use crate::pump::store::{DB_FILENAME, PumpStore, WithdrawRequest};
use crate::pump::withdraw::{apply_pending, run};

const T0: u64 = 1_000_000;
const BASE: &str = "1111111111111111111111111111111111111111";
const TIP: &str = "2222222222222222222222222222222222222222";

fn setup() -> (TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.set_now(T0);
    (dir, ctx, host)
}

fn request(ctx: &Ctx, need: &str, reason: &str) {
    let mut store = PumpStore::open(&ctx.home.join(DB_FILENAME)).unwrap();
    store.add_withdraw_request(need, reason, T0).unwrap();
}

fn pending(ctx: &Ctx) -> Vec<WithdrawRequest> {
    PumpStore::open_read_only(&ctx.home.join(DB_FILENAME))
        .unwrap()
        .unwrap()
        .pending_withdraw_requests()
        .unwrap()
}

fn add_need(st: &mut State, handle: &str, units: &[(&str, Status)]) {
    st.needs.insert(
        handle.to_owned(),
        NeedRecord {
            verdict: String::from("units"),
            units: units.iter().map(|(uid, _)| (*uid).to_owned()).collect(),
            revision: 1,
            ..NeedRecord::default()
        },
    );
    for (uid, status) in units {
        st.units.insert(
            (*uid).to_owned(),
            Unit {
                need: handle.to_owned(),
                target: String::from("default"),
                revision: 1,
                status: *status,
                ..Unit::default()
            },
        );
    }
}

fn status(st: &State, uid: &str) -> Status {
    st.units[uid].status
}

fn exceptions(ctx: &Ctx) -> String {
    fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap_or_default()
}

fn log(host: &FakeHost) -> String {
    host.logs().join("\n")
}

/// A need whose units are all merged and whose branch is frozen, so the audit
/// step would pick it up.
fn frozen_need(st: &mut State, handle: &str) {
    add_need(st, handle, &[("f1", Status::Merged)]);
    need::reserve(st, "default", handle, 1, BASE).unwrap();
    need::record_create_job(st, "default", handle, "init").unwrap();
    need::record_created(st, "default", handle, "init", BASE, BASE).unwrap();
    need::record_tip(st, "default", handle, 1, BASE, TIP).unwrap();
    need::freeze(st, "default", handle, 1, TIP).unwrap();
}

#[test]
fn applying_a_request_withdraws_unfinished_units_and_closes_the_need() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    add_need(
        &mut st,
        "pumpfix-g1",
        &[
            ("pending", Status::Pending),
            ("stuck", Status::Stuck),
            ("delivered", Status::Delivered),
            ("merged", Status::Merged),
            ("nodefect", Status::NoDefect),
            ("replaced", Status::Replaced),
        ],
    );
    add_need(&mut st, "other", &[("o1", Status::Pending)]);
    request(&ctx, "pumpfix-g1", "15-era repair need");

    apply_pending(&ctx, &mut st);

    assert_eq!(status(&st, "pending"), Status::Withdrawn);
    assert_eq!(status(&st, "stuck"), Status::Withdrawn);
    assert_eq!(status(&st, "delivered"), Status::Withdrawn);
    assert_eq!(status(&st, "merged"), Status::Merged);
    assert_eq!(status(&st, "nodefect"), Status::NoDefect);
    assert_eq!(status(&st, "replaced"), Status::Replaced);
    assert_eq!(status(&st, "o1"), Status::Pending);
    assert_eq!(st.closed_needs, vec![String::from("pumpfix-g1")]);
    assert!(pending(&ctx).is_empty());
    assert!(log(&host).contains("withdrew need pumpfix-g1: 3 unit(s) (15-era repair need)"));
    assert!(log(&host).contains("mail need is not closed"));
    assert!(exceptions(&ctx).contains("need-withdrawn pumpfix-g1"));
}

#[test]
fn an_unknown_need_is_dropped_with_a_log_line() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    add_need(&mut st, "n1", &[("u1", Status::Pending)]);
    request(&ctx, "nope", "typo");

    apply_pending(&ctx, &mut st);

    assert_eq!(status(&st, "u1"), Status::Pending);
    assert!(st.closed_needs.is_empty());
    assert!(pending(&ctx).is_empty());
    assert!(log(&host).contains("withdraw nope dropped: unknown need"));
    assert!(!exceptions(&ctx).contains("need-withdrawn"));
}

#[test]
fn a_withdrawn_need_gets_no_audit_and_no_dispatch_exception() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    frozen_need(&mut st, "pumpfix-q1");
    assert!(
        need::binding_for(&st, "default", "pumpfix-q1")
            .unwrap()
            .is_some()
    );

    // Before the withdraw the frozen need is audited every tick and the
    // dispatch fails, which is the 16.0.3 failure this command ends.
    drop(crate::pump::need_pipeline::dispatch_audits(&ctx, &mut st));
    assert!(exceptions(&ctx).contains("need-audit-dispatch"));
    fs::remove_file(ctx.home.join("exceptions.txt")).unwrap();

    request(&ctx, "pumpfix-q1", "no mail thread");
    apply_pending(&ctx, &mut st);
    let queued = crate::pump::need_pipeline::dispatch_audits(&ctx, &mut st).unwrap();

    assert!(queued.is_empty());
    assert!(!exceptions(&ctx).contains("need-audit-dispatch"));
    assert!(st.jobs.values().all(|job| job.kind != "need-audit"));
    // The branch record is dropped; the remote branch is left for the operator.
    assert!(need::get(&st, "default", "pumpfix-q1").unwrap().is_none());
    assert!(log(&host).contains("left for manual deletion"));
    assert_eq!(st.closed_needs, vec![String::from("pumpfix-q1")]);
}

#[test]
fn applying_the_same_need_twice_closes_it_once() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    add_need(&mut st, "n1", &[("u1", Status::Stuck)]);
    request(&ctx, "n1", "first");
    request(&ctx, "n1", "second");

    apply_pending(&ctx, &mut st);

    assert_eq!(st.closed_needs, vec![String::from("n1")]);
    assert!(pending(&ctx).is_empty());
}

fn mailspace_project() -> TempDir {
    let dir = tempdir().unwrap();
    let vivi = dir.path().join(".vivi");
    fs::create_dir_all(&vivi).unwrap();
    fs::write(
        vivi.join("mailspace.toml"),
        "name = \"withdraw\"\n\n[pump]\nbranch = \"next\"\n",
    )
    .unwrap();
    dir
}

fn project_home(dir: &TempDir) -> PathBuf {
    resolve_project_home(dir.path()).unwrap()
}

fn command(needs: &[&str], reason: &str) -> PumpWithdrawCommand {
    PumpWithdrawCommand {
        needs: needs.iter().map(|need| (*need).to_owned()).collect(),
        reason: reason.to_owned(),
        project: None,
    }
}

#[test]
fn run_withdraw_records_one_request_per_need() {
    let dir = mailspace_project();
    let mut store = PumpStore::open(&project_home(&dir).join(DB_FILENAME)).unwrap();
    store.save(&State::default()).unwrap();

    run(dir.path(), &command(&["pumpfix-g1", "pumpfix-q2"], "stale")).unwrap();

    let rows = PumpStore::open_read_only(&project_home(&dir).join(DB_FILENAME))
        .unwrap()
        .unwrap()
        .pending_withdraw_requests()
        .unwrap();
    let recorded: Vec<(&str, &str)> = rows
        .iter()
        .map(|row| (row.need.as_str(), row.reason.as_str()))
        .collect();
    assert_eq!(
        recorded,
        vec![("pumpfix-g1", "stale"), ("pumpfix-q2", "stale")]
    );
}

#[test]
fn run_withdraw_refuses_a_blank_reason_and_a_missing_database() {
    let dir = mailspace_project();
    assert!(run(dir.path(), &command(&["n1"], "  ")).is_err());
    let error = run(dir.path(), &command(&["n1"], "why")).unwrap_err();
    assert!(error.to_string().contains("no pump.db"));
}
