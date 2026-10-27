//! Tests for [`crate::mind::actions`].

use std::path::PathBuf;

use super::*;
use crate::mind::authority::Ledger;
use crate::mind::config::{AuthorityStage, Budgets};

const NOW: u64 = 1_000;

fn watched() -> Vec<PathBuf> {
    vec![PathBuf::from("/w/app")]
}

fn run(stage: AuthorityStage, reply: &str) -> (Vec<Planned>, Ledger) {
    let budgets = Budgets {
        per_need_actions: 1,
        ..Budgets::default()
    };
    let gate = Gate {
        stage,
        paused: false,
        budgets: &budgets,
        now: NOW,
    };
    let mut ledger = Ledger::new();
    let requests = parse_envelope(reply).unwrap();
    (plan(requests, &gate, &watched(), &mut ledger), ledger)
}

const REQUEUE: &str = "Looked at it.\n+++\n[[action]]\nkind = \"requeue\"\nproject = \"/w/app\"\nneed = \"3f9a1c2e\"\nstep = \"b71d04aa\"\nnote = \"flaky gate\"\n+++\n";

#[test]
fn no_envelope_is_no_action() {
    assert!(parse_envelope("all quiet").unwrap().is_empty());
    assert!(parse_envelope("+++ only one fence").unwrap().is_empty());
}

#[test]
fn a_toml_tagged_opening_fence_is_an_envelope() {
    let reply =
        "Escalating.\n+++ TOML\n[[action]]\nkind = \"escalate\"\nproject = \"/w/app\"\n+++\n";
    let requests = parse_envelope(reply).unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].kind, Action::Escalate);
    assert!(
        parse_envelope("+++ yaml\nkind = 1\n+++")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn last_envelope_wins_and_bad_toml_is_reported() {
    let reply = "+++\n[[action]]\nkind = \"escalate\"\n+++\nlater\n+++\n[[action]]\nkind = \"read-status\"\n+++";
    let requests = parse_envelope(reply).unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].kind, Action::ReadStatus);
    assert!(parse_envelope("+++\n[[action]]\nkind = \"rm-rf\"\n+++").is_err());
    assert!(parse_envelope("+++\n[[action]]\nkind = \"escalate\"\nshell = \"x\"\n+++").is_err());
}

#[test]
fn observe_refuses_a_requeue_and_unstick_permits_it() {
    let (planned, ledger) = run(AuthorityStage::Observe, REQUEUE);
    assert!(matches!(
        planned[0].verdict,
        Err(Refusal::Gate(Denial::Stage { .. }))
    ));
    assert!(ledger.is_empty());
    let (planned, ledger) = run(AuthorityStage::Unstick, REQUEUE);
    assert_eq!(planned[0].verdict, Ok(()));
    assert_eq!(ledger["3f9a1c2e"], vec![NOW]);
}

#[test]
fn two_requeues_of_one_need_spend_the_budget_in_one_envelope() {
    let body = REQUEUE.trim_end().trim_end_matches("+++");
    let twice = format!(
        "{body}[[action]]\nkind = \"requeue\"\nproject = \"/w/app\"\nneed = \"3f9a1c2e\"\nstep = \"c0ffee00\"\n+++\n"
    );
    let (planned, _) = run(AuthorityStage::Unstick, &twice);
    assert_eq!(planned.len(), 2);
    assert_eq!(planned[0].verdict, Ok(()));
    assert!(matches!(
        planned[1].verdict,
        Err(Refusal::Gate(Denial::Budget { .. }))
    ));
}

#[test]
fn unwatched_projects_and_injected_handles_are_refused() {
    let other = REQUEUE.replace("/w/app", "/etc");
    let (planned, _) = run(AuthorityStage::Unstick, &other);
    assert_eq!(planned[0].verdict, Err(Refusal::UnwatchedProject));
    let injected = REQUEUE.replace("b71d04aa", "b71d; rm -rf /");
    let (planned, _) = run(AuthorityStage::Unstick, &injected);
    assert_eq!(planned[0].verdict, Err(Refusal::BadHandle("step")));
    let flag = REQUEUE.replace("b71d04aa", "--dry-run");
    let (planned, _) = run(AuthorityStage::Unstick, &flag);
    assert_eq!(planned[0].verdict, Err(Refusal::BadHandle("step")));
}

#[test]
fn missing_fields_are_refused() {
    let no_step = "+++\n[[action]]\nkind = \"answer\"\nproject = \"/w/app\"\nneed = \"n1\"\nword = \"done\"\n+++";
    let (planned, _) = run(AuthorityStage::Unstick, no_step);
    assert_eq!(planned[0].verdict, Err(Refusal::MissingField("step")));
    let no_project = "+++\n[[action]]\nkind = \"unlock\"\nneed = \"n1\"\n+++";
    let (planned, _) = run(AuthorityStage::Unstick, no_project);
    assert_eq!(planned[0].verdict, Err(Refusal::MissingField("project")));
}

#[test]
fn empty_fields_are_absent_fields() {
    // A live model filled every listed field, writing `need = ""` on a
    // read-status; that was refused as "need is not a plain handle".
    let reply = "+++\n[[action]]\nkind = \"read-status\"\nproject = \"/w/app\"\nneed = \"\"\nstep = \" \"\nword = \"\"\nnote = \"look\"\n+++";
    let (planned, _) = run(AuthorityStage::Observe, reply);
    assert_eq!(planned[0].verdict, Ok(()));
    assert_eq!(planned[0].request.need, None);
    assert_eq!(planned[0].request.step, None);
    assert_eq!(planned[0].request.word, None);
    let no_project = "+++\n[[action]]\nkind = \"unlock\"\nproject = \"\"\nneed = \"n1\"\n+++";
    let (planned, _) = run(AuthorityStage::Unstick, no_project);
    assert_eq!(planned[0].verdict, Err(Refusal::MissingField("project")));
}

#[test]
fn long_notes_and_floods_are_refused() {
    let long = REQUEUE.replace("flaky gate", &"x".repeat(MAX_NOTE + 1));
    let (planned, _) = run(AuthorityStage::Unstick, &long);
    assert_eq!(planned[0].verdict, Err(Refusal::NoteTooLong));
    let one = "[[action]]\nkind = \"escalate\"\n";
    let flood = format!("+++\n{}+++", one.repeat(MAX_ACTIONS + 1));
    let (planned, _) = run(AuthorityStage::Observe, &flood);
    assert_eq!(planned[MAX_ACTIONS].verdict, Err(Refusal::TooMany));
    assert_eq!(planned[0].verdict, Ok(()));
}

#[test]
fn argv_maps_onto_existing_pump_commands() {
    let requests = parse_envelope(REQUEUE).unwrap();
    assert_eq!(
        requests[0].argv().unwrap(),
        ["pump", "requeue", "b71d04aa", "--project", "/w/app"]
    );
    let answer = ActionRequest {
        kind: Action::Answer,
        project: Some("/w/app".into()),
        need: Some("n1".into()),
        step: Some("s1".into()),
        word: Some("failed".into()),
        note: None,
        build: None,
        to: None,
        subject: None,
        body: None,
        done_when: Vec::new(),
        target: None,
        write_scope: Vec::new(),
        priority: None,
        cap: None,
        role: None,
        pool: None,
        model: None,
        band: None,
        reasoning: None,
        offset: None,
        limit: None,
        file: None,
        lines: None,
        filter: None,
    };
    assert_eq!(
        answer.argv().unwrap(),
        ["pump", "answer", "s1", "failed", "--project", "/w/app"]
    );
    let letter = ActionRequest {
        kind: Action::Escalate,
        ..answer
    };
    assert_eq!(letter.argv(), None);
}

#[test]
fn dry_run_records_and_runs_nothing() {
    let requests = parse_envelope(REQUEUE).unwrap();
    let mut actuator = DryRun::default();
    let line = actuator.apply(&requests[0]).unwrap();
    assert!(line.starts_with("would run vivi pump requeue"), "{line}");
    assert_eq!(actuator.applied.len(), 1);
}

#[test]
fn a_dry_run_letter_names_its_project_need_and_note() {
    let request = ActionRequest {
        kind: Action::Escalate,
        project: Some("/w/app".into()),
        need: Some("3f9a1c2e".into()),
        step: None,
        word: None,
        note: Some("locked at the operator gate".into()),
        build: None,
        to: None,
        subject: None,
        body: None,
        done_when: Vec::new(),
        target: None,
        write_scope: Vec::new(),
        priority: None,
        cap: None,
        role: None,
        pool: None,
        model: None,
        band: None,
        reasoning: None,
        offset: None,
        limit: None,
        file: None,
        lines: None,
        filter: None,
    };
    let line = DryRun::default().apply(&request).unwrap();
    assert_eq!(
        line,
        "would send Escalate (letter or stub) project=/w/app need=3f9a1c2e note=\"locked at the operator gate\""
    );
}

const ROLLBACK: &str = "+++\n[[action]]\nkind = \"rollback-edge\"\nbuild = \"1a2b3c4d5e6f\"\n+++\n";

#[test]
fn rollback_edge_is_hub_wide_and_opens_at_unstick() {
    let (planned, ledger) = run(AuthorityStage::Unstick, ROLLBACK);
    assert_eq!(planned[0].verdict, Ok(()));
    assert!(ledger.is_empty());
    let (planned, _) = run(AuthorityStage::Observe, ROLLBACK);
    assert!(matches!(planned[0].verdict, Err(Refusal::Gate(_))));
}

#[test]
fn rollback_edge_needs_a_plain_build() {
    let missing = "+++\n[[action]]\nkind = \"rollback-edge\"\n+++\n";
    let (planned, _) = run(AuthorityStage::Unstick, missing);
    assert_eq!(planned[0].verdict, Err(Refusal::MissingField("build")));
    let bad = "+++\n[[action]]\nkind = \"rollback-edge\"\nbuild = \"../../etc\"\n+++\n";
    let (planned, _) = run(AuthorityStage::Unstick, bad);
    assert_eq!(planned[0].verdict, Err(Refusal::BadHandle("build")));
}

#[test]
fn every_added_field_parses_with_its_declared_type() {
    let reply = concat!(
        "+++\n[[action]]\nkind = \"escalate\"\nproject = \"/w/app\"\n",
        "to = \"operator\"\nsubject = \"status\"\nbody = \"the body\"\n",
        "done_when = [\"tests pass\", \"docs updated\"]\n",
        "target = \"/w/app/plan.md\"\nwrite_scope = [\"src/a.rs\", \"docs\"]\n",
        "priority = 2\ncap = 3\n",
        "role = \"hand\"\npool = \"studio\"\nmodel = \"m1\"\nband = \"b\"\nreasoning = \"low\"\n",
        "offset = 0\nlimit = 10\nfile = \"src/a.rs\"\nlines = 5\nfilter = \"todo\"\n+++\n",
    );
    let request = &parse_envelope(reply).unwrap()[0];
    assert_eq!(request.to.as_deref(), Some("operator"));
    assert_eq!(request.subject.as_deref(), Some("status"));
    assert_eq!(request.body.as_deref(), Some("the body"));
    assert_eq!(request.done_when, ["tests pass", "docs updated"]);
    assert_eq!(request.target.as_deref(), Some("/w/app/plan.md"));
    assert_eq!(request.write_scope, ["src/a.rs", "docs"]);
    assert_eq!(request.priority, Some(2));
    assert_eq!(request.cap, Some(3));
    assert_eq!(request.role.as_deref(), Some("hand"));
    assert_eq!(request.pool.as_deref(), Some("studio"));
    assert_eq!(request.model.as_deref(), Some("m1"));
    assert_eq!(request.band.as_deref(), Some("b"));
    assert_eq!(request.reasoning.as_deref(), Some("low"));
    assert_eq!(request.offset, Some(0));
    assert_eq!(request.limit, Some(10));
    assert_eq!(request.file.as_deref(), Some("src/a.rs"));
    assert_eq!(request.lines, Some(5));
    assert_eq!(request.filter.as_deref(), Some("todo"));
}

#[test]
fn an_envelope_without_the_new_fields_leaves_them_absent() {
    let request = &parse_envelope("+++\n[[action]]\nkind = \"escalate\"\n+++\n").unwrap()[0];
    assert_eq!(request.to, None);
    assert_eq!(request.subject, None);
    assert_eq!(request.body, None);
    assert!(request.done_when.is_empty());
    assert_eq!(request.target, None);
    assert!(request.write_scope.is_empty());
    assert_eq!(request.priority, None);
    assert_eq!(request.cap, None);
    assert_eq!(request.role, None);
    assert_eq!(request.pool, None);
    assert_eq!(request.model, None);
    assert_eq!(request.band, None);
    assert_eq!(request.reasoning, None);
    assert_eq!(request.offset, None);
    assert_eq!(request.limit, None);
    assert_eq!(request.file, None);
    assert_eq!(request.lines, None);
    assert_eq!(request.filter, None);
}

#[test]
fn numeric_fields_are_integer_only_and_keep_negatives() {
    let reply = "+++\n[[action]]\nkind = \"escalate\"\npriority = 0\ncap = 0\n\
                 offset = -1\nlimit = -2\nlines = -3\n+++\n";
    let request = &parse_envelope(reply).unwrap()[0];
    assert_eq!(request.priority, Some(0));
    assert_eq!(request.cap, Some(0));
    // Negatives survive for the typed range validation; no unsigned wrap.
    assert_eq!(request.offset, Some(-1));
    assert_eq!(request.limit, Some(-2));
    assert_eq!(request.lines, Some(-3));
    let float = "+++\n[[action]]\nkind = \"escalate\"\npriority = 1.5\n+++\n";
    assert!(parse_envelope(float).is_err());
}

#[test]
fn blank_new_fields_and_blank_collection_entries_are_absent() {
    let reply = concat!(
        "+++\n[[action]]\nkind = \"escalate\"\n",
        "to = \" \"\nsubject = \"\"\nbody = \"   \"\ntarget = \"\\t\"\n",
        "done_when = [\"\", \"  \", \"tests pass\"]\n",
        "write_scope = [\"src/a.rs\", \" \"]\n",
        "role = \"\"\npool = \" \"\nmodel = \"\"\nband = \" \"\nreasoning = \"\"\n",
        "file = \" \"\nfilter = \"\"\n+++\n",
    );
    let request = &parse_envelope(reply).unwrap()[0];
    assert_eq!(request.to, None);
    assert_eq!(request.subject, None);
    assert_eq!(request.body, None);
    assert_eq!(request.target, None);
    assert_eq!(request.done_when, ["tests pass"]);
    assert_eq!(request.write_scope, ["src/a.rs"]);
    assert_eq!(request.role, None);
    assert_eq!(request.pool, None);
    assert_eq!(request.model, None);
    assert_eq!(request.band, None);
    assert_eq!(request.reasoning, None);
    assert_eq!(request.file, None);
    assert_eq!(request.filter, None);
    // A collection of only blank entries is absent, not a default entry.
    let blank = "+++\n[[action]]\nkind = \"escalate\"\
                 \ndone_when = [\"\", \" \"]\nwrite_scope = [\"\"]\n+++\n";
    let request = &parse_envelope(blank).unwrap()[0];
    assert!(request.done_when.is_empty());
    assert!(request.write_scope.is_empty());
}

#[test]
fn nonblank_body_prose_is_not_rewritten() {
    let reply = "+++\n[[action]]\nkind = \"escalate\"\
                 \nbody = \"  keep this  \"\ndone_when = [\"  keep \"]\n+++\n";
    let request = &parse_envelope(reply).unwrap()[0];
    assert_eq!(request.body.as_deref(), Some("  keep this  "));
    assert_eq!(request.done_when, ["  keep "]);
}
