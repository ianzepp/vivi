//! Tests for [`crate::mind::actions`].

use std::path::PathBuf;

use super::*;
use crate::mind::authority::Ledger;
use crate::mind::config::{AuthorityStage, Budgets};

const NOW: u64 = 1_000;

fn watched() -> Vec<PathBuf> {
    vec![PathBuf::from("/w/app")]
}

/// An action request with every optional field absent.
fn request(kind: Action) -> ActionRequest {
    ActionRequest {
        kind,
        project: None,
        need: None,
        step: None,
        word: None,
        note: None,
        entry: None,
        status: None,
        limit: None,
        file: None,
        lines: None,
        filter: None,
        flow: None,
        pool: None,
        before: None,
        priority: None,
        role: None,
        model: None,
        cap: None,
        band: None,
        reasoning: None,
        to: None,
        subject: None,
        body: None,
        done_when: Vec::new(),
        target: None,
        write_scope: Vec::new(),
    }
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
fn every_new_scalar_and_list_field_parses() {
    let reply = "+++\n[[action]]\nkind = \"read-status\"\nproject = \"/w/app\"\nentry = \"e1\"\nstatus = \"open\"\nlimit = 25\nfile = \"log\"\nlines = 80\nfilter = \"flaky\"\nflow = \"ship\"\npool = \"hands\"\nbefore = \"msg_01\"\npriority = 2\nrole = \"hand\"\nmodel = \"m\"\ncap = 3\nband = \"mid\"\nreasoning = \"low\"\nto = \"operator\"\nsubject = \"status\"\nbody = \"all quiet\"\ndone_when = [\"gate green\"]\ntarget = \"src/lib.rs\"\nwrite_scope = [\"src/lib.rs\", \"src/main.rs\"]\n+++";
    let requests = parse_envelope(reply).unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.entry.as_deref(), Some("e1"));
    assert_eq!(request.status.as_deref(), Some("open"));
    assert_eq!(request.limit, Some(25));
    assert_eq!(request.file.as_deref(), Some("log"));
    assert_eq!(request.lines, Some(80));
    assert_eq!(request.filter.as_deref(), Some("flaky"));
    assert_eq!(request.flow.as_deref(), Some("ship"));
    assert_eq!(request.pool.as_deref(), Some("hands"));
    assert_eq!(request.before.as_deref(), Some("msg_01"));
    assert_eq!(request.priority, Some(2));
    assert_eq!(request.role.as_deref(), Some("hand"));
    assert_eq!(request.model.as_deref(), Some("m"));
    assert_eq!(request.cap, Some(3));
    assert_eq!(request.band.as_deref(), Some("mid"));
    assert_eq!(request.reasoning.as_deref(), Some("low"));
    assert_eq!(request.to.as_deref(), Some("operator"));
    assert_eq!(request.subject.as_deref(), Some("status"));
    assert_eq!(request.body.as_deref(), Some("all quiet"));
    assert_eq!(request.done_when, vec![String::from("gate green")]);
    assert_eq!(request.target.as_deref(), Some("src/lib.rs"));
    assert_eq!(
        request.write_scope,
        vec![String::from("src/lib.rs"), String::from("src/main.rs")]
    );
}

#[test]
fn signed_numeric_fields_accept_negative_values() {
    let reply = "+++\n[[action]]\nkind = \"read-status\"\nproject = \"/w/app\"\nlimit = -1\nlines = -5\npriority = -10\ncap = -3\n+++";
    let requests = parse_envelope(reply).unwrap();
    assert_eq!(requests[0].limit, Some(-1));
    assert_eq!(requests[0].lines, Some(-5));
    assert_eq!(requests[0].priority, Some(-10));
    assert_eq!(requests[0].cap, Some(-3));
    // The typed refusals that name these fields land later; today's policy
    // still passes the read through.
    let (planned, _) = run(AuthorityStage::Observe, reply);
    assert_eq!(planned[0].verdict, Ok(()));
}

#[test]
fn non_integer_numbers_and_unknown_fields_are_refused_at_parse() {
    assert!(parse_envelope("+++\n[[action]]\nkind = \"read-status\"\nlimit = 1.5\n+++").is_err());
    assert!(
        parse_envelope("+++\n[[action]]\nkind = \"read-status\"\npriority = \"high\"\n+++")
            .is_err()
    );
    assert!(parse_envelope("+++\n[[action]]\nkind = \"escalate\"\nshell = \"x\"\n+++").is_err());
}

#[test]
fn blank_normalization_covers_every_string_field_but_keeps_list_members() {
    let reply = "+++\n[[action]]\nkind = \"read-status\"\nproject = \"/w/app\"\nneed = \"\"\nstep = \" \"\nword = \"\\t\"\nnote = \"\"\nentry = \"\"\nstatus = \"\"\nfile = \"\"\nfilter = \"\"\nflow = \"\"\npool = \"\"\nbefore = \"\"\nrole = \"\"\nmodel = \"\"\nband = \"\"\nreasoning = \"\"\nto = \"\"\nsubject = \"\"\nbody = \"\"\ntarget = \"\"\ndone_when = [\"\", \"gate green\"]\nwrite_scope = [\" \"]\n+++";
    let (planned, _) = run(AuthorityStage::Observe, reply);
    assert_eq!(planned[0].verdict, Ok(()));
    let request = &planned[0].request;
    for field in [
        &request.need,
        &request.step,
        &request.word,
        &request.note,
        &request.entry,
        &request.status,
        &request.file,
        &request.filter,
        &request.flow,
        &request.pool,
        &request.before,
        &request.role,
        &request.model,
        &request.band,
        &request.reasoning,
        &request.to,
        &request.subject,
        &request.body,
        &request.target,
    ] {
        assert_eq!(*field, None);
    }
    // A blank list member is kept, not silently dropped.
    assert_eq!(
        request.done_when,
        vec![String::new(), String::from("gate green")]
    );
    assert_eq!(request.write_scope, vec![String::from(" ")]);
}

#[test]
fn the_new_refusals_display_their_field() {
    assert_eq!(
        Refusal::InvalidField("limit").to_string(),
        "limit has an invalid value"
    );
    assert_eq!(
        Refusal::UnexpectedField("priority").to_string(),
        "priority does not apply to this action"
    );
    assert_eq!(
        Refusal::FilingTooLarge("body").to_string(),
        "body is too large to file"
    );
}

#[test]
fn argv_maps_onto_existing_pump_commands() {
    let requests = parse_envelope(REQUEUE).unwrap();
    assert_eq!(
        requests[0].argv().unwrap(),
        ["pump", "requeue", "b71d04aa", "--project", "/w/app"]
    );
    let answer = ActionRequest {
        project: Some("/w/app".into()),
        need: Some("n1".into()),
        step: Some("s1".into()),
        word: Some("failed".into()),
        ..request(Action::Answer)
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
        project: Some("/w/app".into()),
        need: Some("3f9a1c2e".into()),
        note: Some("locked at the operator gate".into()),
        ..request(Action::Escalate)
    };
    let line = DryRun::default().apply(&request).unwrap();
    assert_eq!(
        line,
        "would send Escalate (letter or stub) project=/w/app need=3f9a1c2e note=\"locked at the operator gate\""
    );
}
