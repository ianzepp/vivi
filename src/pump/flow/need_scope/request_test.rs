//! Tests for classifying scope requests: target resolution, preservation of
//! standing scope, and atomic classification of the requested additions.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use tempfile::tempdir;

use super::effective_scopes;
use super::{RequestTarget, ScopeClassification, ScopeContext, ScopeRequest, classify};
use crate::letter::fixture::{need_body_with, task_body_with};
use crate::letter::work::{NeedFront, StepFront, TaskFront};
use crate::pump::config::{PumpConfig, PumpTarget};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::need_scope::{
    RulingDecision, ScopeIssue, ScopeRuling, decode, effective_target_scope, encode,
    ruling_note_body,
};
use crate::pump::flow::slice_dispatch::SliceMail;

/// The pump's reply role: the only sender whose scope rulings count.
const REPLY_FROM: &str = "mind";

/// A need thread whose root letter carries `write_scope` and `target`.
fn a_need(mail: &mut FakeMail, write_scope: &[&str], target: Option<&str>) -> String {
    let front = NeedFront {
        write_scope: write_scope.iter().map(|path| (*path).to_string()).collect(),
        target: target.map(str::to_string),
        ..NeedFront::new(vec!["done".to_string()])
    };
    mail.send_step("mind", "hand", "need", &need_body_with(&front, ""), &[])
        .expect("need thread")
}

/// An open slice task of `need` whose front matter names `target`.
fn a_slice_task(mail: &mut FakeMail, need: &str, run: u32, target: Option<&str>) -> String {
    let front = TaskFront {
        target: target.map(str::to_string),
        step: Some(StepFront {
            need: need.to_string(),
            run,
            node: Some(format!("slice:{run}")),
            sentinel: false,
        }),
        ..TaskFront::default()
    };
    mail.send_step("pump", "hand", "slice", &task_body_with(&front, ""), &[])
        .expect("slice task")
}

/// A valid grant ruling for `need`.
fn a_grant(need: &str, target: &str, paths: &[&str]) -> ScopeRuling {
    ScopeRuling {
        need: need.to_string(),
        target: target.to_string(),
        run: 2,
        task: "t0009".to_string(),
        paths: paths.iter().map(|path| (*path).to_string()).collect(),
        reason: "held by no open slice".to_string(),
        decision: RulingDecision::Grant,
        issue: None,
    }
}

/// Reply on `need` with the scope-ruling note for `ruling`, as the reply role.
fn grant_via_note(mail: &mut FakeMail, need: &str, ruling: &ScopeRuling, source_job: &str) {
    let body = ruling_note_body(ruling, source_job).expect("note body");
    mail.reply(need, &body).expect("reply");
}

/// A verifiable checkout with a real `src/` tree.
fn a_checkout() -> tempfile::TempDir {
    let dir = tempdir().expect("temp checkout should create");
    fs::create_dir_all(dir.path().join("src")).expect("tree should create");
    fs::write(dir.path().join("src/lib.rs"), "fn main() {}\n").expect("file should write");
    dir
}

/// One configured target: `allow` is the allow list (`None` is
/// repository-wide) and `checkout` the resolved integration checkout.
fn a_target(allow: Option<&[&str]>, deny: &[&str], checkout: Option<&Path>) -> PumpTarget {
    PumpTarget {
        scope_allow: allow
            .map(|entries| entries.iter().map(|entry| String::from(*entry)).collect()),
        scope_deny: deny.iter().map(|entry| String::from(*entry)).collect(),
        integration_repo: checkout.map(|path| path.to_string_lossy().into_owned()),
        ..PumpTarget::default()
    }
}

/// A pump config with the named targets; empty `targets` keeps the legacy
/// `default` target only.
fn a_config(targets: &[(&str, PumpTarget)]) -> PumpConfig {
    let targets = targets
        .iter()
        .map(|(id, target)| (String::from(*id), target.clone()))
        .collect::<BTreeMap<_, _>>();
    PumpConfig {
        targets,
        ..PumpConfig::default()
    }
}

/// Classify `request` for `config` rooted at `project`, with `branch` as the
/// need's branch row target.
fn decide(
    mail: &FakeMail,
    config: &PumpConfig,
    project: &Path,
    branch: Option<&str>,
    request: &ScopeRequest,
) -> ScopeClassification {
    let context = ScopeContext {
        config,
        project,
        branch_target: branch,
        reply_from: REPLY_FROM,
    };
    classify(mail, &context, request).expect("classification")
}

/// A planner or admitter request naming `target`.
fn a_role_request(need: &str, run: u32, task: &str, target: &str, paths: &[&str]) -> ScopeRequest {
    ScopeRequest {
        need: need.to_string(),
        task: task.to_string(),
        run,
        target: RequestTarget::Named(target.to_string()),
        paths: paths.iter().map(|path| String::from(*path)).collect(),
    }
}

/// A hand request: no target of its own, the slice task's is used.
fn a_hand_request(need: &str, run: u32, task: &str, paths: &[&str]) -> ScopeRequest {
    ScopeRequest {
        target: RequestTarget::SliceTask,
        ..a_role_request(need, run, task, "", paths)
    }
}

#[test]
fn a_role_request_grants_inside_target_policy() {
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/pump/"], None);
    let checkout = a_checkout();
    let config = a_config(&[(
        "web",
        a_target(Some(&["src/"]), &["src/secret/"], Some(checkout.path())),
    )]);
    let task = a_slice_task(&mut mail, &need, 2, Some("web"));
    let verdict = decide(
        &mail,
        &config,
        checkout.path(),
        None,
        &a_role_request(&need, 2, &task, "web", &["src/new.rs"]),
    );
    assert_eq!(verdict.target, "web");
    assert_eq!(verdict.paths, ["src/new.rs"]);
    assert_eq!(verdict.added, ["src/new.rs"]);
    assert_eq!(verdict.decision, RulingDecision::Grant);
    assert_eq!(verdict.issue, None);
    assert!(verdict.is_grant());
    // The classification settles into a valid ruling record.
    let ruling = verdict.ruling(&need, 2, &task);
    assert_eq!(
        decode(&encode(&ruling).expect("encode")).expect("decode"),
        ruling
    );
}

#[test]
fn an_unknown_role_target_needs_an_operator() {
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/"], None);
    let checkout = a_checkout();
    let config = a_config(&[("web", a_target(Some(&["src/"]), &[], Some(checkout.path())))]);
    let task = a_slice_task(&mut mail, &need, 2, Some("web"));
    let verdict = decide(
        &mail,
        &config,
        checkout.path(),
        None,
        &a_role_request(&need, 2, &task, "ghost", &["src/new.rs"]),
    );
    assert_eq!(verdict.decision, RulingDecision::OperatorRequired);
    assert_eq!(verdict.issue, Some(ScopeIssue::UnknownTarget));
    assert_eq!(verdict.target, "ghost");
    // The whole request is left for the operator; nothing is authorized.
    assert_eq!(verdict.added, ["src/new.rs"]);
    let ruling = verdict.ruling(&need, 2, &task);
    let decoded = decode(&encode(&ruling).expect("encode")).expect("decode");
    assert_eq!(decoded.issue, Some(ScopeIssue::UnknownTarget));
    assert_eq!(decoded.decision, RulingDecision::OperatorRequired);
}

#[test]
fn a_blank_role_target_needs_an_operator() {
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/"], None);
    let checkout = a_checkout();
    let config = a_config(&[("web", a_target(Some(&["src/"]), &[], Some(checkout.path())))]);
    let task = a_slice_task(&mut mail, &need, 2, Some("web"));
    let verdict = decide(
        &mail,
        &config,
        checkout.path(),
        None,
        &a_role_request(&need, 2, &task, "   ", &["src/new.rs"]),
    );
    assert_eq!(verdict.decision, RulingDecision::OperatorRequired);
    assert_eq!(verdict.issue, Some(ScopeIssue::UnknownTarget));
    assert_eq!(verdict.target, "");
    assert_eq!(verdict.reason, "the request names no target");
}

#[test]
fn a_request_target_that_disagrees_with_the_need_needs_an_operator() {
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/"], Some("vivi"));
    let checkout = a_checkout();
    let config = a_config(&[
        ("web", a_target(Some(&["src/"]), &[], Some(checkout.path()))),
        ("vivi", a_target(None, &[], None)),
    ]);
    let task = a_slice_task(&mut mail, &need, 2, Some("vivi"));
    let verdict = decide(
        &mail,
        &config,
        checkout.path(),
        None,
        &a_role_request(&need, 2, &task, "web", &["src/new.rs"]),
    );
    assert_eq!(verdict.decision, RulingDecision::OperatorRequired);
    assert_eq!(verdict.issue, Some(ScopeIssue::TargetMismatch));
    assert_eq!(
        verdict.reason,
        "request target `web` disagrees with the need's target `vivi`"
    );
}

#[test]
fn a_request_target_that_disagrees_with_the_branch_needs_an_operator() {
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/"], None);
    let checkout = a_checkout();
    let config = a_config(&[
        ("web", a_target(Some(&["src/"]), &[], Some(checkout.path()))),
        ("vivi", a_target(None, &[], None)),
    ]);
    let task = a_slice_task(&mut mail, &need, 2, Some("web"));
    let verdict = decide(
        &mail,
        &config,
        checkout.path(),
        Some("vivi"),
        &a_role_request(&need, 2, &task, "web", &["src/new.rs"]),
    );
    assert_eq!(verdict.decision, RulingDecision::OperatorRequired);
    assert_eq!(verdict.issue, Some(ScopeIssue::TargetMismatch));
    assert_eq!(
        verdict.reason,
        "request target `web` disagrees with the need's branch target `vivi`"
    );
}

#[test]
fn a_known_target_before_branch_selection_stands_for_this_request() {
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/"], None);
    let checkout = a_checkout();
    let config = a_config(&[(
        "web",
        a_target(Some(&["docs/"]), &[], Some(checkout.path())),
    )]);
    let task = a_slice_task(&mut mail, &need, 2, Some("web"));
    // Neither the need nor a branch has selected a repository: the explicit
    // known target classifies against that target's own policy and checkout.
    let verdict = decide(
        &mail,
        &config,
        checkout.path(),
        None,
        &a_role_request(&need, 2, &task, "web", &["docs/new.md"]),
    );
    assert_eq!(verdict.decision, RulingDecision::Grant);
    assert_eq!(verdict.target, "web");
    assert_eq!(verdict.added, ["docs/new.md"]);
    // The ruling records the target, so the grant applies to that target
    // only and never creates a branch for it.
    let ruling = verdict.ruling(&need, 2, &task);
    assert_eq!(ruling.target, "web");
    assert_eq!(
        decode(&encode(&ruling).expect("encode")).expect("decode"),
        ruling
    );
}

#[test]
fn a_hand_request_takes_the_slice_task_target() {
    let mut mail = FakeMail::new(REPLY_FROM);
    // The need's own delegation stays out of src/, so web's deny list and
    // not the delegation decides the requested file.
    let need = a_need(&mut mail, &["docs/"], None);
    let checkout = a_checkout();
    let config = a_config(&[(
        "web",
        a_target(Some(&["src/"]), &["src/secret/"], Some(checkout.path())),
    )]);
    let task = a_slice_task(&mut mail, &need, 2, Some("web"));
    // The slice task targets web, so web's deny list decides the request.
    let verdict = decide(
        &mail,
        &config,
        checkout.path(),
        None,
        &a_hand_request(&need, 2, &task, &["src/secret/key.txt"]),
    );
    assert_eq!(verdict.decision, RulingDecision::OperatorRequired);
    assert_eq!(verdict.issue, Some(ScopeIssue::Denied));
    assert_eq!(verdict.target, "web");
    // Nothing is filtered out of the denied set.
    assert_eq!(verdict.added, ["src/secret/key.txt"]);
}

#[test]
fn a_hand_request_without_a_task_target_needs_an_operator() {
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/"], None);
    let checkout = a_checkout();
    let config = a_config(&[("web", a_target(Some(&["src/"]), &[], Some(checkout.path())))]);
    let task = a_slice_task(&mut mail, &need, 2, None);
    let verdict = decide(
        &mail,
        &config,
        checkout.path(),
        None,
        &a_hand_request(&need, 2, &task, &["src/new.rs"]),
    );
    assert_eq!(verdict.decision, RulingDecision::OperatorRequired);
    assert_eq!(verdict.issue, Some(ScopeIssue::UnknownTarget));
    assert_eq!(verdict.reason, "the slice task names no target");
}

#[test]
fn a_hand_request_for_an_unconfigured_task_target_needs_an_operator() {
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/"], None);
    let checkout = a_checkout();
    let config = a_config(&[("web", a_target(Some(&["src/"]), &[], Some(checkout.path())))]);
    let task = a_slice_task(&mut mail, &need, 2, Some("ghost"));
    let verdict = decide(
        &mail,
        &config,
        checkout.path(),
        None,
        &a_hand_request(&need, 2, &task, &["src/new.rs"]),
    );
    assert_eq!(verdict.decision, RulingDecision::OperatorRequired);
    assert_eq!(verdict.issue, Some(ScopeIssue::UnknownTarget));
    assert_eq!(verdict.target, "ghost");
}

#[test]
fn grants_are_isolated_per_target() {
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/"], None);
    grant_via_note(
        &mut mail,
        &need,
        &a_grant(&need, "vivi", &["docs/v.md"]),
        "job-1",
    );
    let web_checkout = a_checkout();
    let vivi_checkout = a_checkout();
    let config = a_config(&[
        (
            "web",
            a_target(Some(&["docs/"]), &[], Some(web_checkout.path())),
        ),
        (
            "vivi",
            a_target(Some(&["docs/", "src/"]), &[], Some(vivi_checkout.path())),
        ),
    ]);
    let task = a_slice_task(&mut mail, &need, 2, Some("web"));
    // The vivi grant does not reach web: the same path is fresh there and
    // web's own policy decides it.
    let for_web = decide(
        &mail,
        &config,
        web_checkout.path(),
        None,
        &a_role_request(&need, 2, &task, "web", &["docs/v.md"]),
    );
    assert_eq!(for_web.decision, RulingDecision::Grant);
    assert_eq!(for_web.added, ["docs/v.md"]);
    // For vivi the grant is already held: nothing new is decided.
    let for_vivi = decide(
        &mail,
        &config,
        web_checkout.path(),
        None,
        &a_role_request(&need, 2, &task, "vivi", &["docs/v.md"]),
    );
    assert_eq!(for_vivi.decision, RulingDecision::Grant);
    assert!(for_vivi.added.is_empty());
    assert!(for_vivi.reason.contains("already"));
}

#[test]
fn an_empty_original_scope_holds_nothing_and_the_policy_decides() {
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &[], None);
    grant_via_note(
        &mut mail,
        &need,
        &a_grant(&need, "web", &["docs/held.md"]),
        "job-1",
    );
    let checkout = a_checkout();
    let config = a_config(&[(
        "web",
        a_target(Some(&["docs/"]), &[], Some(checkout.path())),
    )]);
    // The reader keeps an empty delegation empty: grants widen a delegation,
    // they are not one, so the brief renders no group at all.
    assert!(
        effective_scopes(&mail, &need, REPLY_FROM, &[String::from("web")])
            .expect("scopes")
            .is_empty()
    );
    // The classifier therefore holds nothing: the policy decides the whole
    // request.
    let task = a_slice_task(&mut mail, &need, 2, Some("web"));
    let verdict = decide(
        &mail,
        &config,
        checkout.path(),
        None,
        &a_role_request(&need, 2, &task, "web", &["docs/new.md"]),
    );
    assert_eq!(verdict.decision, RulingDecision::Grant);
    assert_eq!(verdict.added, ["docs/new.md"]);
}

#[test]
fn prior_delegation_is_retained_when_more_is_added() {
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/"], None);
    grant_via_note(
        &mut mail,
        &need,
        &a_grant(&need, "web", &["docs/old.md"]),
        "job-1",
    );
    let checkout = a_checkout();
    let config = a_config(&[(
        "web",
        a_target(Some(&["docs/", "src/"]), &[], Some(checkout.path())),
    )]);
    let task = a_slice_task(&mut mail, &need, 2, Some("web"));
    let verdict = decide(
        &mail,
        &config,
        checkout.path(),
        None,
        &a_role_request(&need, 2, &task, "web", &["docs/new.md"]),
    );
    assert_eq!(verdict.decision, RulingDecision::Grant);
    assert_eq!(verdict.added, ["docs/new.md"]);
    // The new grant lands as a ruling note; the earlier delegation and the
    // earlier grant are both still there.
    grant_via_note(&mut mail, &need, &verdict.ruling(&need, 2, &task), "job-2");
    let scopes =
        effective_scopes(&mail, &need, REPLY_FROM, &[String::from("web")]).expect("scopes");
    assert_eq!(scopes.len(), 1);
    assert_eq!(scopes[0].original, ["src/"]);
    assert_eq!(scopes[0].granted, ["docs/old.md", "docs/new.md"]);
    assert_eq!(
        effective_target_scope(&mail, &need, "web", REPLY_FROM).expect("scope"),
        [
            "src/".to_string(),
            "docs/old.md".to_string(),
            "docs/new.md".to_string()
        ]
    );
}

#[test]
fn mixed_additions_are_refused_whole() {
    let mut mail = FakeMail::new(REPLY_FROM);
    // The delegation stays out of src/, so both src/ members are fresh.
    let need = a_need(&mut mail, &["docs/"], None);
    let checkout = a_checkout();
    let config = a_config(&[(
        "web",
        a_target(Some(&["src/"]), &["src/secret/"], Some(checkout.path())),
    )]);
    let task = a_slice_task(&mut mail, &need, 2, Some("web"));
    // One denied member refuses the complete set; no subset is authorized.
    let verdict = decide(
        &mail,
        &config,
        checkout.path(),
        None,
        &a_role_request(
            &need,
            2,
            &task,
            "web",
            &["src/new.rs", "src/secret/key.txt"],
        ),
    );
    assert_eq!(verdict.decision, RulingDecision::OperatorRequired);
    assert_eq!(verdict.issue, Some(ScopeIssue::Denied));
    assert_eq!(verdict.added, ["src/new.rs", "src/secret/key.txt"]);
    // One allow-list miss refuses the complete set too: `tests/` is fresh
    // and sits outside web's allow list, and `docs/` is already held.
    let verdict = decide(
        &mail,
        &config,
        checkout.path(),
        None,
        &a_role_request(&need, 2, &task, "web", &["src/new.rs", "tests/x.md"]),
    );
    assert_eq!(verdict.decision, RulingDecision::OperatorRequired);
    assert_eq!(verdict.issue, Some(ScopeIssue::OutsideAllow));
}

#[test]
fn an_unresolvable_checkout_needs_an_operator() {
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/"], None);
    let project = tempdir().expect("temp project should create");
    // No integration repo configured: the target's default checkout under
    // the pump home does not exist, so nothing on disk can be verified.
    let config = a_config(&[("web", a_target(Some(&["src/"]), &[], None))]);
    let task = a_slice_task(&mut mail, &need, 2, Some("web"));
    let verdict = decide(
        &mail,
        &config,
        project.path(),
        None,
        &a_role_request(&need, 2, &task, "web", &["web/new.rs"]),
    );
    assert_eq!(verdict.decision, RulingDecision::OperatorRequired);
    assert_eq!(verdict.issue, Some(ScopeIssue::UnverifiablePath));
}

#[test]
fn paths_are_normalized_and_invalid_paths_need_an_operator() {
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/"], None);
    let checkout = a_checkout();
    let config = a_config(&[("web", a_target(Some(&["src/"]), &[], Some(checkout.path())))]);
    let task = a_slice_task(&mut mail, &need, 2, Some("web"));
    let verdict = decide(
        &mail,
        &config,
        checkout.path(),
        None,
        &a_role_request(
            &need,
            2,
            &task,
            "web",
            &["./src/new.rs", "src/new.rs", "../escape"],
        ),
    );
    assert_eq!(verdict.paths, ["src/new.rs", "../escape"]);
    assert_eq!(verdict.decision, RulingDecision::OperatorRequired);
    assert_eq!(verdict.issue, Some(ScopeIssue::InvalidPath));
}

#[test]
fn a_fully_held_request_grants_nothing_new_and_records_no_ruling() {
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/"], None);
    let checkout = a_checkout();
    let config = a_config(&[("web", a_target(None, &[], Some(checkout.path())))]);
    let task = a_slice_task(&mut mail, &need, 2, Some("web"));
    // The delegation already covers the request: standing scope is preserved
    // untouched, whatever today's policy says.
    let verdict = decide(
        &mail,
        &config,
        checkout.path(),
        None,
        &a_role_request(&need, 2, &task, "web", &["src/deep/file.rs"]),
    );
    assert_eq!(verdict.decision, RulingDecision::Grant);
    assert_eq!(verdict.added, Vec::<String>::new());
    assert_eq!(
        verdict.reason,
        format!("every requested path is already delegated or granted on {need}")
    );
    // A grant that adds nothing has no ruling to record: the record would
    // name no paths, and encoding refuses it.
    let ruling = verdict.ruling(&need, 2, &task);
    assert!(encode(&ruling).is_err());
}

#[test]
fn effective_scopes_group_by_target_and_separate_delegation_from_grants() {
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/"], None);
    grant_via_note(
        &mut mail,
        &need,
        &a_grant(&need, "web", &["docs/w.md"]),
        "job-1",
    );
    grant_via_note(
        &mut mail,
        &need,
        &a_grant(&need, "vivi", &["src/v.rs"]),
        "job-2",
    );
    let scopes = effective_scopes(
        &mail,
        &need,
        REPLY_FROM,
        &[
            String::from("web"),
            String::from("vivi"),
            String::from("ghost"),
        ],
    )
    .expect("scopes");
    // A need that names no target delegates to every target it is read
    // for, so `ghost` renders the delegation with no grants of its own.
    assert_eq!(scopes.len(), 3);
    assert_eq!(scopes[0].target, "web");
    assert_eq!(scopes[0].original, ["src/"]);
    assert_eq!(scopes[0].granted, ["docs/w.md"]);
    assert_eq!(
        scopes[0].paths(),
        ["src/".to_string(), "docs/w.md".to_string()]
    );
    assert_eq!(scopes[1].target, "vivi");
    assert_eq!(
        scopes[1].paths(),
        ["src/".to_string(), "src/v.rs".to_string()]
    );
    assert_eq!(scopes[2].target, "ghost");
    assert_eq!(scopes[2].original, ["src/"]);
    assert_eq!(scopes[2].granted, Vec::<String>::new());
}

#[test]
fn an_explicit_need_target_keeps_other_targets_out_of_the_brief() {
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/"], Some("vivi"));
    grant_via_note(
        &mut mail,
        &need,
        &a_grant(&need, "web", &["docs/w.md"]),
        "job-1",
    );
    grant_via_note(
        &mut mail,
        &need,
        &a_grant(&need, "vivi", &["src/v.rs"]),
        "job-2",
    );
    let scopes = effective_scopes(
        &mail,
        &need,
        REPLY_FROM,
        &[String::from("web"), String::from("vivi")],
    )
    .expect("scopes");
    assert_eq!(scopes.len(), 1);
    assert_eq!(scopes[0].target, "vivi");
    assert_eq!(scopes[0].original, ["src/"]);
    assert_eq!(scopes[0].granted, ["src/v.rs"]);
}

#[test]
fn a_wide_policy_default_is_not_an_existing_grant() {
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/"], None);
    grant_via_note(
        &mut mail,
        &need,
        &a_grant(&need, "web", &["docs/w.md"]),
        "job-1",
    );
    let scopes =
        effective_scopes(&mail, &need, REPLY_FROM, &[String::from("web")]).expect("scopes");
    // The target's policy is repository-wide, and none of that appears: the
    // brief shows only the delegation and the grants the need actually holds.
    assert_eq!(scopes.len(), 1);
    assert_eq!(scopes[0].original, ["src/"]);
    assert_eq!(scopes[0].granted, ["docs/w.md"]);
    assert!(!scopes[0].is_empty());
}
