//! Tests for [`crate::pump::recover`]: the request an operator posts, the
//! recovery the pump applies to a need branch held `unverified`, and the CLI
//! half's checks. They run against a real mailspace, so the records and the
//! actions are the ones `vivi pump` shows.

use std::collections::BTreeSet;
use std::fs;

use crate::cli::PumpRecoverCommand;
use crate::mailspace::{PumpActionKind, PumpActionRequest};
use crate::pump::action::apply_pending;
use crate::pump::fixture::{Project, planned, project};
use crate::pump::host::Ctx;
use crate::pump::need::{self, NeedBranchStatus};
use crate::pump::recover::{apply_one, run};
use crate::pump::state::{State, Status};
use crate::pump::store::{DB_FILENAME, PumpStore};

const BASE: &str = "1111111111111111111111111111111111111111";
const OBSERVED: &str = "ea26f971319b7ae5e3fcabab64e5134c2c999ebd";
const MOVED: &str = "3333333333333333333333333333333333333333";

/// A need with one unit in each of merging, delivered and merged, whose
/// branch is held `unverified` at [`OBSERVED`] over the trusted [`BASE`].
fn held_state(need: &str) -> State {
    let mut st = planned(
        need,
        &[
            ("u1", Status::Merging),
            ("u2", Status::Delivered),
            ("u3", Status::Merged),
        ],
    );
    active_branch(&mut st, need);
    need::record_unverified_tip(
        &mut st,
        "default",
        need,
        "merge-job-1",
        OBSERVED,
        "job merge-job-1: runner publication did not match the observed need ref",
    )
    .unwrap();
    st
}

/// Add a second held need, with its own units, to `st`.
fn add_held(st: &mut State, need: &str) {
    let other = planned(need, &[("b1", Status::Delivered)]);
    st.needs.extend(other.needs);
    st.units.extend(other.units);
    active_branch(st, need);
    need::record_unverified_tip(st, "default", need, "merge-job-2", OBSERVED, "held").unwrap();
}

fn active_branch(st: &mut State, need: &str) {
    need::reserve(st, "default", need, 1, BASE).unwrap();
    need::record_create_job(st, "default", need, "init").unwrap();
    need::record_created(st, "default", need, "init", BASE, BASE).unwrap();
}

fn branch(st: &State, need: &str) -> need::NeedBranch {
    need::get(st, "default", need).unwrap().unwrap()
}

/// Make the fake remote report the need ref at `sha`.
fn remote_at(project: &Project, need: &str, sha: &str) {
    project.host.reply(
        "ls-remote",
        0,
        &format!("{sha}\trefs/heads/vivi/need/{need}\n"),
    );
}

fn exceptions(ctx: &Ctx) -> String {
    fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap_or_default()
}

fn logged(project: &Project, text: &str) -> bool {
    project.host.logs().iter().any(|line| line.contains(text))
}

fn command(needs: &[&str], dry_run: bool) -> PumpRecoverCommand {
    PumpRecoverCommand {
        needs: needs.iter().map(|need| (*need).to_owned()).collect(),
        target: None,
        reason: None,
        dry_run,
        project: None,
    }
}

fn save(project: &Project, st: &State) {
    PumpStore::open(&project.home().join(DB_FILENAME))
        .unwrap()
        .save(st)
        .unwrap();
}

fn pending(project: &Project) -> Vec<crate::mailspace::PumpAction> {
    project
        .mailspace
        .pump_pending_actions(&BTreeSet::new())
        .unwrap()
}

#[test]
fn a_recover_request_round_trips_through_its_letter() {
    let project = project();
    let need = project.need("fix auth");
    project.mailspace.pump_send(None, &need, None).unwrap();

    let posted = project
        .mailspace
        .pump_post_action(
            None,
            &need,
            &PumpActionRequest::recover(Some("default"), "the tip is sound"),
        )
        .unwrap();
    project
        .mailspace
        .pump_post_action(None, &need, &PumpActionRequest::recover(None, "any target"))
        .unwrap();

    let actions = pending(&project);
    assert_eq!(actions.len(), 2);
    assert_eq!(actions[0].id, posted.id);
    assert_eq!(actions[0].request.kind, PumpActionKind::Recover);
    assert_eq!(actions[0].request.target.as_deref(), Some("default"));
    assert_eq!(actions[0].request.reason, "the tip is sound");
    assert_eq!(actions[0].need, need);
    assert_eq!(actions[1].request.target, None);
    assert_eq!(actions[1].request.reason, "any target");
}

#[test]
fn an_applied_recover_adopts_the_tip_clears_the_hold_and_requeues_the_units() {
    let project = project();
    let ctx = project.pump_ctx();
    let need = project.need("fix auth");
    project.mailspace.pump_send(None, &need, None).unwrap();
    let mut st = held_state(&need);
    remote_at(&project, &need, OBSERVED);
    let id = project
        .mailspace
        .pump_post_action(None, &need, &PumpActionRequest::recover(None, "bug fixed"))
        .unwrap()
        .id;

    apply_pending(&ctx, &mut st);

    let record = branch(&st, &need);
    assert_eq!(record.status, NeedBranchStatus::Active);
    assert_eq!(record.branch_tip_sha.as_deref(), Some(OBSERVED));
    assert_eq!(record.unverified_base_sha, None);
    assert_eq!(record.observed_unverified_tip_sha, None);
    assert_eq!(record.unverified_job_id, None);
    assert_eq!(record.unverified_detail, None);
    assert_eq!(record.error, None);
    assert_eq!(record.base_sha, BASE, "the branch keeps its base");
    assert_eq!(st.units["u1"].status, Status::Delivered);
    assert_eq!(st.units["u2"].status, Status::Delivered);
    assert_eq!(st.units["u3"].status, Status::Merged);
    let text = exceptions(&ctx);
    assert!(text.contains(&format!("need-recovered {need}")), "{text}");
    assert!(text.contains(&format!("adopted {OBSERVED} over trusted tip {BASE}")));
    assert!(text.contains("2 unit(s)"), "{text}");
    assert!(text.contains("bug fixed"), "{text}");
    assert!(logged(
        &project,
        &format!("need branch recovered default/{need}")
    ));
    assert!(st.board.actions.contains_key(&id), "consumed");
}

#[test]
fn a_recover_is_refused_when_the_remote_ref_moved_and_the_hold_stays() {
    let project = project();
    let ctx = project.pump_ctx();
    let need = project.need("fix auth");
    let mut st = held_state(&need);
    remote_at(&project, &need, MOVED);

    apply_one(&ctx, &mut st, &need, None, "operator recover");

    let record = branch(&st, &need);
    assert_eq!(record.status, NeedBranchStatus::Unverified);
    assert_eq!(record.branch_tip_sha.as_deref(), Some(BASE));
    assert_eq!(
        record.observed_unverified_tip_sha.as_deref(),
        Some(OBSERVED)
    );
    assert_eq!(st.units["u1"].status, Status::Merging, "units untouched");
    let text = exceptions(&ctx);
    assert!(
        text.contains(&format!("need-recover-refused {need}")),
        "{text}"
    );
    assert!(text.contains(MOVED) && text.contains(OBSERVED), "{text}");
    assert!(!text.contains("need-recovered"));
}

#[test]
fn a_recover_is_refused_when_the_remote_ref_cannot_be_read() {
    let project = project();
    let ctx = project.pump_ctx();
    let need = project.need("fix auth");
    let mut st = held_state(&need);
    project
        .host
        .reply("ls-remote", 128, "fatal: unable to access the remote");

    apply_one(&ctx, &mut st, &need, None, "operator recover");

    assert_eq!(branch(&st, &need).status, NeedBranchStatus::Unverified);
    assert!(exceptions(&ctx).contains("need-recover-refused"));
}

#[test]
fn a_branch_that_is_not_unverified_is_skipped_and_logged_with_its_status() {
    let project = project();
    let ctx = project.pump_ctx();
    let need = project.need("fix auth");
    let mut st = planned(&need, &[("u1", Status::Merging)]);
    active_branch(&mut st, &need);
    let before = branch(&st, &need);
    remote_at(&project, &need, OBSERVED);

    apply_one(&ctx, &mut st, &need, None, "operator recover");

    assert_eq!(branch(&st, &need), before);
    assert_eq!(st.units["u1"].status, Status::Merging);
    assert!(logged(
        &project,
        &format!("need-recover-skipped default/{need}: status is active")
    ));
    assert!(!exceptions(&ctx).contains("need-recover"));
}

#[test]
fn a_need_with_no_branch_record_or_another_target_is_skipped() {
    let project = project();
    let ctx = project.pump_ctx();
    let need = project.need("fix auth");
    let mut st = held_state(&need);

    apply_one(&ctx, &mut st, "nope", None, "r");
    apply_one(&ctx, &mut st, &need, Some("elsewhere"), "r");

    assert!(logged(
        &project,
        "need-recover-skipped nope: no need branch record"
    ));
    assert!(logged(
        &project,
        &format!("need-recover-skipped {need}: no need branch record on target elsewhere")
    ));
    assert_eq!(branch(&st, &need).status, NeedBranchStatus::Unverified);
}

#[test]
fn recover_dry_run_records_nothing() {
    let project = project();
    let need = project.need("fix auth");
    project.mailspace.pump_send(None, &need, None).unwrap();
    save(&project, &held_state(&need));

    run(project.root(), &command(&[&need], true)).unwrap();

    assert!(pending(&project).is_empty());
}

#[test]
fn run_recover_posts_one_action_per_need_with_its_target_and_reason() {
    let project = project();
    let first = project.need("fix auth");
    let second = project.need("fix billing");
    project.mailspace.pump_send(None, &first, None).unwrap();
    project.mailspace.pump_send(None, &second, None).unwrap();
    let mut st = held_state(&first);
    add_held(&mut st, &second);
    save(&project, &st);
    let mut cmd = command(&[&first, &second], false);
    cmd.target = Some(String::from("default"));
    cmd.reason = Some(String::from("receipt bug fixed"));

    run(project.root(), &cmd).unwrap();

    let posted: Vec<_> = pending(&project)
        .into_iter()
        .map(|action| {
            (
                action.need,
                action.request.target,
                action.request.reason,
                action.request.kind,
            )
        })
        .collect();
    let reason = String::from("receipt bug fixed");
    let target = Some(String::from("default"));
    assert_eq!(
        posted,
        vec![
            (
                first,
                target.clone(),
                reason.clone(),
                PumpActionKind::Recover
            ),
            (second, target, reason, PumpActionKind::Recover),
        ]
    );
}

#[test]
fn run_recover_refuses_a_need_that_is_not_held_or_has_no_open_record() {
    let project = project();
    let held = project.need("fix auth");
    let active = project.need("fix billing");
    let closed = project.need("fix mail");
    project.mailspace.pump_send(None, &held, None).unwrap();
    project.mailspace.pump_send(None, &active, None).unwrap();
    let mut st = held_state(&held);
    let other = planned(&active, &[("a1", Status::Delivered)]);
    st.needs.extend(other.needs);
    st.units.extend(other.units);
    active_branch(&mut st, &active);
    save(&project, &st);

    let text = run(project.root(), &command(&[&held, &active], false))
        .unwrap_err()
        .to_string();
    assert!(text.contains("has no branch held unverified"), "{text}");
    assert!(text.contains("default is active"), "{text}");

    let text = run(project.root(), &command(&[&closed], false))
        .unwrap_err()
        .to_string();
    assert!(text.contains("no need branch record"), "{text}");

    // The held need's own record is closed: nothing is posted.
    let record = project.mailspace.pump_open_record(&held).unwrap().unwrap();
    project
        .mailspace
        .pump_done(&record.handle, Some("closed"))
        .unwrap();
    let text = run(project.root(), &command(&[&held], false))
        .unwrap_err()
        .to_string();
    assert!(text.contains("has no open pump record"), "{text}");
    assert!(pending(&project).is_empty());
}

#[test]
fn run_recover_refuses_a_blank_reason_and_a_missing_database() {
    let project = project();
    let mut cmd = command(&["n1"], false);
    cmd.reason = Some(String::from("  "));
    assert!(run(project.root(), &cmd).is_err());
    let error = run(project.root(), &command(&["n1"], false)).unwrap_err();
    assert!(error.to_string().contains("no pump.db"));
}
