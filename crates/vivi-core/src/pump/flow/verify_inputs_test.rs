//! Tests for the brief input readers in [`super`].

use std::sync::Arc;

use serde_json::json;
use tempfile::{TempDir, tempdir};

use super::{
    SlicerBase, SlicerSource, admitted_plan, audit_context, branch_range, capped, cto_ruling,
    latest_of_class, need_thread, requirements, rung_report, slicer_base,
    whole_message_requirements,
};
use crate::pump::config::{PumpConfig, PumpTarget};
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::store::step_payload::StepPayloadRow;
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};
use vivi_error::VivariumError;

const NEED: &str = "need-1";
const BASE: &str = "1111111111111111111111111111111111111111";
const TIP: &str = "2222222222222222222222222222222222222222";
const BRANCH: &str = "vivi/need/need-1";
/// A main tip.
const MAIN_TIP: &str = "1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a";
/// Main after an independent commit landed on it.
const MAIN_LATER: &str = "2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b";
/// A published need branch tip.
const NEED_TIP: &str = "3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c";
/// The `radix` target's main tip.
const PEER_MAIN_TIP: &str = "4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d";
/// An older recorded base of the need branch.
const OLD_BASE: &str = "5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e";
/// A `radix` need branch tip.
const PEER_TIP: &str = "6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f";

struct Fixture {
    _dir: TempDir,
    ctx: Ctx,
    host: Arc<FakeHost>,
    store: PumpStore,
}

fn fixture() -> Fixture {
    fixture_with(PumpConfig::default())
}

fn fixture_with(cfg: PumpConfig) -> Fixture {
    let dir = tempdir().expect("tempdir");
    let (ctx, host) = test_ctx(dir.path(), cfg);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    Fixture {
        _dir: dir,
        ctx,
        host,
        store,
    }
}

fn payload(task: &str, run: u32, class: &str, body: serde_json::Value) -> StepPayloadRow {
    StepPayloadRow {
        need: NEED.into(),
        run,
        node: class.into(),
        class: class.into(),
        task: task.into(),
        payload: body,
    }
}

fn step(task: &str, run: u32, verdict: Option<&str>) -> StepRow {
    StepRow {
        run,
        node: "plan".into(),
        task: task.into(),
        state: StepState::Done,
        verdict: verdict.map(str::to_string),
    }
}

fn record_plan(f: &Fixture, task: &str, run: u32, verdict: &str, text: &str) {
    f.store
        .insert_step(NEED, &step(task, run, Some(verdict)))
        .unwrap();
    f.store
        .record_step_payload(&payload(task, run, "planner", json!({ "body": text })))
        .unwrap();
}

#[test]
fn admitted_plan_is_the_planned_plan_not_an_earlier_revision() {
    let f = fixture();
    record_plan(&f, "t1", 1, "revised", "plan one");
    record_plan(&f, "t2", 1, "planned", "plan two");
    record_plan(&f, "t3", 2, "revised", "plan three");
    assert_eq!(
        admitted_plan(&f.store, NEED).unwrap().as_deref(),
        Some("plan two")
    );
    assert_eq!(admitted_plan(&f.store, "other").unwrap(), None);
}

#[test]
fn branch_range_errors_without_a_need_branch_row() {
    let f = fixture();
    let err = branch_range(&f.ctx, &f.store, NEED).unwrap_err();
    assert!(err.to_string().contains(NEED), "{err}");
}

#[test]
fn branch_range_reads_the_row_and_the_pushed_tip() {
    let f = fixture();
    f.store
        .upsert_need_branch(&NeedBranchRow {
            need: NEED.into(),
            branch: "vivi/need/need-1".into(),
            base: Some(BASE.into()),
            status: "live".into(),
            run: 1,
            target: "default".into(),
            priors: vec![],
        })
        .unwrap();
    f.host.reply(
        "ls-remote origin refs/heads/vivi/need/",
        0,
        &format!("{TIP}\trefs/heads/vivi/need/need-1\n"),
    );
    let range = branch_range(&f.ctx, &f.store, NEED).unwrap();
    assert_eq!(range.branch, "vivi/need/need-1");
    assert_eq!(range.base.as_deref(), Some(BASE));
    assert_eq!(range.tip.as_deref(), Some(TIP));
    assert_eq!(range.target, "default");
}

#[test]
fn latest_of_class_picks_the_newest() {
    let rows = vec![
        payload("a", 1, "auditor", json!({"n": 1})),
        payload("b", 1, "rung-need", json!({})),
        payload("c", 2, "auditor", json!({"n": 2})),
    ];
    assert_eq!(latest_of_class(&rows, "auditor").unwrap().task, "c");
    assert_eq!(latest_of_class(&rows, "rung-need").unwrap().task, "b");
    assert!(latest_of_class(&rows, "judge").is_none());
}

#[test]
fn rung_report_lists_exit_sorted_failing_ids_and_tail() {
    let row = payload(
        "r",
        1,
        "rung-need",
        json!({"exit": 101, "failing": ["b::two", "a::one"], "tail": "boom"}),
    );
    assert_eq!(
        rung_report(&row),
        "exit: 101\nfailing: a::one, b::two\ntail:\nboom"
    );
    let bare = payload("r", 1, "rung-need", json!({}));
    assert_eq!(rung_report(&bare), "exit: none\nfailing: none\ntail:\n");
}

#[test]
fn capped_keeps_the_tail_on_a_char_boundary() {
    assert_eq!(capped("short", 10), "short");
    assert_eq!(capped("abcdefghij", 4), "[...]\nghij");
    // "é" is two bytes: a cut inside it moves forward to the next boundary.
    let text = "aaéé";
    let out = capped(text, 3);
    assert_eq!(out, "[...]\né");
}

fn thread_json() -> String {
    serde_json::to_string(&json!([
        {
            "handle": NEED,
            "message_id": "message-1",
            "from": "operator@local",
            "subject": "Need",
            "body": vivi_mail::letter::fixture::need_body(&["Build the thing"], "")
        },
        {
            "handle": "reply-1",
            "message_id": "message-2",
            "from": "operator@local",
            "subject": "Need amendment",
            "body": "also keep the CLI shape"
        }
    ]))
    .unwrap()
}

#[test]
fn need_thread_parses_a_need_show_reply() {
    let f = fixture();
    f.host.reply("need show", 0, &thread_json());
    let messages = need_thread(&f.ctx, NEED).unwrap();
    assert_eq!(messages.len(), 2);
    let reqs = requirements(&f.ctx, NEED, &messages).unwrap();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].id, "need-1-d1");
}

#[test]
fn a_front_matter_root_gives_one_requirement_per_done_when_entry() {
    let f = fixture();
    let root = vivi_mail::letter::fixture::need_body(&["first thing", "second thing"], "prose");
    let messages: Vec<serde_json::Value> = vec![
        json!({"handle": NEED, "from": "operator@local", "body": root}),
        json!({"handle": "reply-1", "from": "operator@local", "body": "also keep the CLI shape"}),
    ];
    let reqs = requirements(&f.ctx, NEED, &messages).unwrap();
    let ids: Vec<&str> = reqs.iter().map(|req| req.id.as_str()).collect();
    assert_eq!(ids, ["need-1-d1", "need-1-d2", "reply-1-r0"]);
    assert_eq!(reqs[0].text, "first thing");
    assert_eq!(reqs[1].text, "second thing");
}

#[test]
fn a_root_without_front_matter_is_an_error_naming_the_handle() {
    let f = fixture();
    let messages: Vec<serde_json::Value> = vec![
        json!({"handle": NEED, "from": "operator@local", "body": "Build the thing"}),
        json!({"handle": "reply-1", "from": "operator@local", "body": "more"}),
    ];
    let err = requirements(&f.ctx, NEED, &messages).unwrap_err();
    assert!(err.to_string().contains(NEED), "{err}");
    assert!(err.to_string().contains("front matter"), "{err}");
}

#[test]
fn need_thread_errors_on_an_empty_array_and_on_failure() {
    let f = fixture();
    f.host.reply("need show", 0, "[]");
    let err = need_thread(&f.ctx, NEED).unwrap_err();
    assert!(err.to_string().contains("no root message"), "{err}");
    f.host.reply("need show", 1, "boom");
    let err = need_thread(&f.ctx, NEED).unwrap_err();
    assert!(
        err.to_string().contains("cannot read full need thread"),
        "{err}"
    );
}

fn put(f: &Fixture, task: &str, run: u32, class: &str, body: serde_json::Value) {
    f.store
        .record_step_payload(&payload(task, run, class, body))
        .unwrap();
}

#[test]
fn audit_context_reads_the_earlier_run_when_this_run_has_no_audit() {
    let f = fixture();
    put(
        &f,
        "a1",
        1,
        "auditor",
        json!({ "front": { "answer": "block_range" } }),
    );
    put(&f, "r1", 1, "rung", json!({ "exit": 1 }));
    put(&f, "s2", 2, "slicer", json!({}));
    let (run, rows) = audit_context(&f.store, NEED, 2).unwrap();
    assert_eq!(run, 1);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.run == 1));
}

#[test]
fn audit_context_keeps_a_run_that_audited() {
    let f = fixture();
    put(&f, "a1", 1, "auditor", json!({}));
    put(&f, "a2", 2, "auditor", json!({}));
    let (run, rows) = audit_context(&f.store, NEED, 2).unwrap();
    assert_eq!(run, 2);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].task, "a2");
}

#[test]
fn audit_context_without_any_audit_gives_the_asked_run() {
    let f = fixture();
    put(&f, "s1", 1, "slicer", json!({}));
    put(&f, "s2", 2, "slicer", json!({}));
    let (run, rows) = audit_context(&f.store, NEED, 2).unwrap();
    assert_eq!(run, 2);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].task, "s2");
}

#[test]
fn cto_ruling_is_the_earlier_runs_answer_reason_and_new_fact() {
    let f = fixture();
    put(
        &f,
        "c1",
        1,
        "cto",
        json!({ "front": { "answer": "rework", "reason": "R", "new_fact": "X" } }),
    );
    let ruling = cto_ruling(&f.store, NEED, 2).unwrap().unwrap();
    assert_eq!(ruling, "answer: rework\nreason: R\nnew_fact: X");
    assert_eq!(cto_ruling(&f.store, NEED, 1).unwrap(), None);
}

#[test]
fn a_need_without_done_when_is_audited_against_its_own_messages() {
    let f = fixture();
    let ctx = &f.ctx;
    let thread = serde_json::json!([{
        "handle": "need-1",
        "message_id": "mind-root",
        "from": "operator@local",
        "subject": "Need",
        "body": "Rename the en pack rows and every en source together."
    }, {
        "handle": "need-1-reply",
        "message_id": "pump-reply",
        "from": "mind@local",
        "subject": "Pump plan",
        "body": "pump: planned at revision 1"
    }]);
    let messages = thread.as_array().unwrap().clone();
    let requirements = whole_message_requirements(ctx, "need-1", &messages);
    assert_eq!(requirements.len(), 1, "{requirements:?}");
    assert_eq!(requirements[0].id, "need-1-r0");
    assert_eq!(
        requirements[0].text,
        "Deliver what need message need-1 asks for (full text in the original need thread)."
    );
    assert!(!requirements[0].text.contains("Rename the en pack rows"));
}

#[test]
fn a_long_multi_line_message_still_yields_one_short_single_line_requirement() {
    let f = fixture();
    let ctx = &f.ctx;
    let body = format!("first line\n{}", "long detail\n".repeat(1000));
    let messages = vec![serde_json::json!({
        "handle": "need-9",
        "message_id": "mind-root",
        "from": "operator@local",
        "subject": "Need",
        "body": body,
    })];

    let requirements = whole_message_requirements(ctx, "need-9", &messages);

    assert_eq!(requirements.len(), 1);
    assert!(!requirements[0].text.contains('\n'));
    assert!(requirements[0].text.len() < 200);
}

#[test]
fn a_blank_wrapped_prose_message_gets_no_requirement() {
    let f = fixture();
    let ctx = &f.ctx;
    let messages = vec![
        serde_json::json!({"handle": "need-5", "message_id": "m1", "from": "operator@local",
            "subject": "Need", "body": "+++\n+++\n\n"}),
        serde_json::json!({"handle": "need-5-a", "message_id": "m2", "from": "operator@local",
            "subject": "Need", "body": "+++\n+++\n\nAlso rename the rows."}),
    ];

    let requirements = whole_message_requirements(ctx, "need-5", &messages);

    assert_eq!(requirements.len(), 1, "{requirements:?}");
    assert_eq!(requirements[0].id, "need-5-a-r0");
}

/// Two explicit targets with distinct remotes and mains.
fn two_targets() -> PumpConfig {
    let mut cfg = PumpConfig::default();
    cfg.targets.insert(
        String::from("default"),
        PumpTarget {
            remote: String::from("origin"),
            main: String::from("main"),
            ..PumpTarget::default()
        },
    );
    cfg.targets.insert(
        String::from("radix"),
        PumpTarget {
            remote: String::from("peer"),
            main: String::from("peer/main"),
            ..PumpTarget::default()
        },
    );
    cfg
}

fn put_row(f: &Fixture, status: &str, base: Option<&str>, target: &str) {
    f.store
        .upsert_need_branch(&NeedBranchRow {
            need: NEED.into(),
            branch: BRANCH.into(),
            base: base.map(str::to_owned),
            status: status.into(),
            run: 1,
            target: target.into(),
            priors: vec![],
        })
        .unwrap();
}

/// Answer `ls-remote <remote> ...` with `sha` on `name`.
fn remote_ref(f: &Fixture, remote: &str, name: &str, sha: &str) {
    f.host.reply(
        &format!("ls-remote {remote} refs/heads/{name}"),
        0,
        &format!("{sha}\trefs/heads/{name}\n"),
    );
}

fn no_remote_ref(f: &Fixture, remote: &str, name: &str) {
    f.host
        .reply(&format!("ls-remote {remote} refs/heads/{name}"), 0, "");
}

/// Answer `vivi need show` with a root letter declaring `target`.
fn need_show(f: &Fixture, target: Option<&str>) {
    let front = vivi_mail::letter::work::NeedFront {
        target: target.map(str::to_owned),
        ..vivi_mail::letter::work::NeedFront::new(vec![String::from("Ship it")])
    };
    let body = vivi_mail::letter::fixture::need_body_with(&front, "");
    let thread = json!([{
        "handle": NEED,
        "message_id": "root-1",
        "from": "operator@local",
        "subject": "Need",
        "body": body,
    }]);
    f.host
        .reply("need show", 0, &serde_json::to_string(&thread).unwrap());
}

fn assert_error(result: Result<SlicerBase, VivariumError>, label: &str, want_config: bool) {
    match result {
        Err(VivariumError::Config(_)) if want_config => {}
        Err(VivariumError::Message(_)) if !want_config => {}
        Err(VivariumError::Other(_)) if !want_config => {}
        other => panic!("{label}: expected a resolution error, got {other:?}"),
    }
}

#[test]
fn slicer_base_preserves_published_need_work() {
    // A published tip with a commit outside main is the base, and main moving
    // independently does not change the answer.
    for main in [MAIN_TIP, MAIN_LATER] {
        let f = fixture_with(two_targets());
        put_row(&f, "live", Some(OLD_BASE), "default");
        remote_ref(&f, "origin", "main", main);
        remote_ref(&f, "origin", BRANCH, NEED_TIP);
        f.host.reply("merge-base --is-ancestor", 1, "");
        let base = slicer_base(&f.ctx, &f.store, NEED, None).unwrap();
        assert_eq!(
            base,
            SlicerBase {
                target: String::from("default"),
                sha: String::from(NEED_TIP),
                source: SlicerSource::NeedBranch,
                reference: format!("refs/heads/{BRANCH}"),
                fallback_reason: None,
            }
        );
        let commands = f.host.commands();
        assert!(
            commands
                .iter()
                .any(|c| c.contains(&format!("fetch origin refs/heads/{BRANCH}"))),
            "{commands:?}"
        );
        assert!(
            commands
                .iter()
                .any(|c| c.contains(&format!("merge-base --is-ancestor {NEED_TIP} {main}"))),
            "{commands:?}"
        );
        // Resolution records nothing: the row keeps its base and status.
        let row = f.store.need_branch(NEED).unwrap().unwrap();
        assert_eq!(row.base.as_deref(), Some(OLD_BASE));
        assert_eq!(row.status, "live");
        assert_eq!(row.target, "default");
    }
}

/// One case where the base is the target's current main.
struct NoWorkCase {
    name: &'static str,
    row: Option<(&'static str, Option<&'static str>)>,
    published: Option<&'static str>,
    main: &'static str,
    ancestry: i32,
}

#[test]
fn slicer_base_uses_current_main_without_need_work() {
    let cases = [
        NoWorkCase {
            name: "no row at all",
            row: None,
            published: None,
            main: MAIN_TIP,
            ancestry: 0,
        },
        NoWorkCase {
            name: "fresh row without a base",
            row: Some(("fresh", None)),
            published: None,
            main: MAIN_TIP,
            ancestry: 0,
        },
        NoWorkCase {
            name: "fresh row with an old base",
            row: Some(("fresh", Some(OLD_BASE))),
            published: None,
            main: MAIN_TIP,
            ancestry: 0,
        },
        NoWorkCase {
            name: "pruned row with an old base",
            row: Some(("pruned", Some(OLD_BASE))),
            published: None,
            main: MAIN_TIP,
            ancestry: 0,
        },
        NoWorkCase {
            name: "live row whose ref is unpublished",
            row: Some(("live", Some(OLD_BASE))),
            published: None,
            main: MAIN_TIP,
            ancestry: 0,
        },
        NoWorkCase {
            name: "published tip still at its recorded base",
            row: Some(("live", Some(NEED_TIP))),
            published: Some(NEED_TIP),
            main: MAIN_LATER,
            ancestry: 0,
        },
        NoWorkCase {
            name: "published tip equal to main",
            row: Some(("live", Some(OLD_BASE))),
            published: Some(MAIN_TIP),
            main: MAIN_TIP,
            ancestry: 0,
        },
        NoWorkCase {
            name: "published tip behind main",
            row: Some(("live", Some(OLD_BASE))),
            published: Some(NEED_TIP),
            main: MAIN_LATER,
            ancestry: 0,
        },
    ];
    for case in cases {
        let f = fixture_with(two_targets());
        if let Some((status, base)) = case.row {
            put_row(&f, status, base, "default");
        }
        need_show(&f, None);
        remote_ref(&f, "origin", "main", case.main);
        match case.published {
            Some(sha) => remote_ref(&f, "origin", BRANCH, sha),
            None => no_remote_ref(&f, "origin", BRANCH),
        }
        f.host.reply("merge-base --is-ancestor", case.ancestry, "");
        let base = slicer_base(&f.ctx, &f.store, NEED, None)
            .unwrap_or_else(|error| panic!("{}: {error}", case.name));
        assert_eq!(base.target, "default", "{}", case.name);
        assert_eq!(base.sha, case.main, "{}", case.name);
        assert_eq!(base.source, SlicerSource::TargetMain, "{}", case.name);
        assert_eq!(base.reference, "refs/heads/main", "{}", case.name);
        assert!(base.fallback_reason.is_some(), "{}", case.name);
        assert_eq!(
            case.row.is_some(),
            f.store.need_branch(NEED).unwrap().is_some()
        );
    }
}

#[test]
fn slicer_base_resolves_the_correct_target() {
    // A stored non-primary target reads that target's remote and main.
    let f = fixture_with(two_targets());
    put_row(&f, "live", Some(OLD_BASE), "radix");
    remote_ref(&f, "peer", "peer/main", PEER_MAIN_TIP);
    remote_ref(&f, "peer", BRANCH, PEER_TIP);
    f.host.reply("merge-base --is-ancestor", 1, "");
    let base = slicer_base(&f.ctx, &f.store, NEED, None).unwrap();
    assert_eq!(base.target, "radix");
    assert_eq!(base.sha, PEER_TIP);
    assert_eq!(base.source, SlicerSource::NeedBranch);
    let commands = f.host.commands();
    assert!(
        commands.iter().all(|c| !c.contains("origin")),
        "{commands:?}"
    );

    // A slice hint names the target when no row exists, ahead of the root need.
    let f = fixture_with(two_targets());
    need_show(&f, Some("default"));
    remote_ref(&f, "peer", "peer/main", PEER_MAIN_TIP);
    let base = slicer_base(&f.ctx, &f.store, NEED, Some("radix")).unwrap();
    assert_eq!(base.target, "radix");
    assert_eq!(base.sha, PEER_MAIN_TIP);
    assert_eq!(base.reference, "refs/heads/peer/main");

    // The root need's declared target comes next.
    let f = fixture_with(two_targets());
    need_show(&f, Some("radix"));
    remote_ref(&f, "peer", "peer/main", PEER_MAIN_TIP);
    remote_ref(&f, "origin", "main", MAIN_TIP);
    let base = slicer_base(&f.ctx, &f.store, NEED, None).unwrap();
    assert_eq!(base.target, "radix");
    assert_eq!(base.sha, PEER_MAIN_TIP);

    // Nothing names a target: the config's primary one.
    let f = fixture_with(two_targets());
    need_show(&f, None);
    remote_ref(&f, "origin", "main", MAIN_TIP);
    let base = slicer_base(&f.ctx, &f.store, NEED, None).unwrap();
    assert_eq!(base.target, "default");
    assert_eq!(base.sha, MAIN_TIP);

    // Legacy configuration falls back to `default` on the workspace main.
    let f = fixture_with(PumpConfig::default());
    need_show(&f, None);
    remote_ref(&f, "origin", "main", MAIN_TIP);
    let base = slicer_base(&f.ctx, &f.store, NEED, None).unwrap();
    assert_eq!(base.target, "default");
    assert_eq!(base.sha, MAIN_TIP);
    assert_eq!(base.source, SlicerSource::TargetMain);
    assert_eq!(base.reference, "refs/heads/main");
}

#[test]
fn slicer_base_does_not_hide_resolution_failures() {
    // An unknown hint target, and a row stored on an unknown target.
    let f = fixture_with(two_targets());
    assert_error(
        slicer_base(&f.ctx, &f.store, NEED, Some("nowhere")),
        "unknown hint",
        true,
    );
    let f = fixture_with(two_targets());
    put_row(&f, "live", Some(OLD_BASE), "nowhere");
    assert_error(
        slicer_base(&f.ctx, &f.store, NEED, None),
        "unknown row target",
        true,
    );

    // Several explicit targets with no primary and no target named anywhere.
    let mut cfg = PumpConfig::default();
    for (id, remote) in [("radix", "peer"), ("other", "more")] {
        cfg.targets.insert(
            String::from(id),
            PumpTarget {
                remote: String::from(remote),
                main: format!("{remote}/main"),
                ..PumpTarget::default()
            },
        );
    }
    let f = fixture_with(cfg);
    need_show(&f, None);
    assert_error(
        slicer_base(&f.ctx, &f.store, NEED, None),
        "ambiguous targets",
        true,
    );

    // A thread read that fails, and a root the typed need parser refuses.
    let f = fixture_with(two_targets());
    f.host.reply("need show", 1, "boom");
    assert_error(
        slicer_base(&f.ctx, &f.store, NEED, None),
        "thread read",
        false,
    );
    let f = fixture_with(two_targets());
    let legacy = json!([{ "handle": NEED, "body": "no front matter here" }]);
    f.host
        .reply("need show", 0, &serde_json::to_string(&legacy).unwrap());
    assert_error(
        slicer_base(&f.ctx, &f.store, NEED, None),
        "root decode",
        false,
    );

    // A main ref that cannot be read, and a main ref that does not exist.
    let f = fixture_with(two_targets());
    f.host.reply("ls-remote origin refs/heads/main", 2, "boom");
    assert_error(
        slicer_base(&f.ctx, &f.store, NEED, Some("default")),
        "ls-remote failure",
        false,
    );
    let f = fixture_with(two_targets());
    no_remote_ref(&f, "origin", "main");
    assert_error(
        slicer_base(&f.ctx, &f.store, NEED, Some("default")),
        "absent main",
        false,
    );

    // Published need work whose snapshots cannot be fetched or compared.
    let f = fixture_with(two_targets());
    put_row(&f, "live", Some(OLD_BASE), "default");
    remote_ref(&f, "origin", "main", MAIN_TIP);
    remote_ref(&f, "origin", BRANCH, NEED_TIP);
    f.host.reply("fetch", 1, "no route");
    assert_error(
        slicer_base(&f.ctx, &f.store, NEED, None),
        "failed fetch",
        false,
    );
    let f = fixture_with(two_targets());
    put_row(&f, "live", Some(OLD_BASE), "default");
    remote_ref(&f, "origin", "main", MAIN_TIP);
    remote_ref(&f, "origin", BRANCH, NEED_TIP);
    f.host
        .reply("merge-base --is-ancestor", 128, "fatal: bad object");
    assert_error(
        slicer_base(&f.ctx, &f.store, NEED, None),
        "inconclusive ancestry",
        false,
    );

    // A row whose branch is still being moved, and an unknown status.
    for status in ["renaming", "deleting", "bogus"] {
        let f = fixture_with(two_targets());
        put_row(&f, status, Some(OLD_BASE), "default");
        assert_error(slicer_base(&f.ctx, &f.store, NEED, None), status, false);
        // The failed resolution left the row untouched.
        let row = f.store.need_branch(NEED).unwrap().unwrap();
        assert_eq!(row.status, status);
        assert_eq!(row.base.as_deref(), Some(OLD_BASE));
    }
}
