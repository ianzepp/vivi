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
    let one = "[[action]]\nkind = \"escalate\"\nproject = \"/w/app\"\n";
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
        entry: None,
        file: None,
        filter: None,
        flow: None,
        pool: None,
        to: None,
        subject: None,
        body: None,
        target: None,
        role: None,
        model: None,
        band: None,
        reasoning: None,
        done_when: None,
        write_scope: None,
        priority: None,
        cap: None,
        limit: None,
        lines: None,
        offset: None,
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
        entry: None,
        file: None,
        filter: None,
        flow: None,
        pool: None,
        to: None,
        subject: None,
        body: None,
        target: None,
        role: None,
        model: None,
        band: None,
        reasoning: None,
        done_when: None,
        write_scope: None,
        priority: None,
        cap: None,
        limit: None,
        lines: None,
        offset: None,
    };
    let line = DryRun::default().apply(&request).unwrap();
    assert_eq!(
        line,
        "would send Escalate (letter or stub) project=/w/app need=3f9a1c2e note=\"locked at the operator gate\""
    );
}

/// One otherwise valid request per canonical kind, as envelope fields
/// without the `project` line, plus whether the build's implemented stages
/// carry the action out once the project is watched.
const EVERY_ACTION: &[(&str, bool)] = &[
    ("kind = \"read-status\"", true),
    (
        "kind = \"read-pump-log\"\nfile = \"pump.log\"\nlines = 40\nfilter = \"panic\"",
        true,
    ),
    (
        "kind = \"read-pump-history\"\nneed = \"aa11\"\nflow = \"main\"",
        true,
    ),
    ("kind = \"read-need\"\nneed = \"bb22\"", true),
    ("kind = \"read-need-thread\"\nneed = \"cc33\"", true),
    ("kind = \"read-need-units\"\nneed = \"dd44\"", true),
    ("kind = \"read-task-thread\"\nstep = \"ee55\"", true),
    ("kind = \"read-config\"", true),
    ("kind = \"read-pool\"\npool = \"runpod\"", true),
    ("kind = \"letter-to-operator\"", true),
    ("kind = \"escalate\"", true),
    ("kind = \"pause-pump\"", true),
    ("kind = \"resume-pump\"", true),
    ("kind = \"requeue\"\nneed = \"ff66\"\nstep = \"0077\"", true),
    ("kind = \"replan\"\nneed = \"1188\"", true),
    (
        "kind = \"answer\"\nneed = \"2299\"\nstep = \"33aa\"\nword = \"done\"",
        true,
    ),
    ("kind = \"unlock\"\nneed = \"44bb\"", true),
    ("kind = \"reply-need\"\nneed = \"55cc\"", true),
    ("kind = \"file-need\"\nneed = \"66dd\"", true),
    ("kind = \"dispatch-need\"\nneed = \"77ee\"", true),
    (
        "kind = \"set-priority\"\nneed = \"88ff\"\npriority = 1",
        true,
    ),
    ("kind = \"withdraw-need\"\nneed = \"9900\"", true),
    (
        "kind = \"set-role-model\"\nneed = \"bb01\"\nrole = \"tugboat\"\nmodel = \"m1\"",
        true,
    ),
    (
        "kind = \"set-pool-cap\"\nneed = \"bb02\"\npool = \"runpod\"\ncap = 5",
        true,
    ),
    (
        "kind = \"set-pool-band\"\nneed = \"bb03\"\npool = \"runpod\"\nband = \"cheap\"",
        true,
    ),
    ("kind = \"file-fix-need\"\nneed = \"aa00\"", true),
    ("kind = \"upgrade\"\ntarget = \"v1.2.3\"", false),
    ("kind = \"self-release\"\ntarget = \"v1.2.3\"", false),
];

#[test]
fn all_actions_require_watched_projects() {
    assert_eq!(
        EVERY_ACTION.len(),
        28,
        "every canonical action needs a watched-project row"
    );
    for (fields, implemented) in EVERY_ACTION {
        let bare = format!("+++\n[[action]]\n{fields}\n+++");
        let (planned, _) = run(AuthorityStage::FileFixes, &bare);
        assert_eq!(
            planned[0].verdict,
            Err(Refusal::MissingField("project")),
            "{fields}"
        );
        let elsewhere = format!("+++\n[[action]]\nproject = \"/etc\"\n{fields}\n+++");
        let (planned, _) = run(AuthorityStage::FileFixes, &elsewhere);
        assert_eq!(
            planned[0].verdict,
            Err(Refusal::UnwatchedProject),
            "{fields}"
        );
        let watched = format!("+++\n[[action]]\nproject = \"/w/app\"\n{fields}\n+++");
        let (planned, _) = run(AuthorityStage::FileFixes, &watched);
        if *implemented {
            assert_eq!(planned[0].verdict, Ok(()), "{fields}");
        } else {
            assert!(
                matches!(planned[0].verdict, Err(Refusal::Gate(_))),
                "{fields}: {:?}",
                planned[0].verdict
            );
        }
    }
}

#[test]
fn inapplicable_fields_are_refused() {
    // A letter carries no entry, a read no model, a requeue no priority.
    let letter =
        "+++\n[[action]]\nkind = \"letter-to-operator\"\nproject = \"/w/app\"\nentry = \"e1\"\n+++";
    let (planned, _) = run(AuthorityStage::Observe, letter);
    assert_eq!(planned[0].verdict, Err(Refusal::UnexpectedField("entry")));
    let read = "+++\n[[action]]\nkind = \"read-status\"\nproject = \"/w/app\"\nmodel = \"m1\"\n+++";
    let (planned, _) = run(AuthorityStage::Observe, read);
    assert_eq!(planned[0].verdict, Err(Refusal::UnexpectedField("model")));
    let surplus = REQUEUE.trim_end().trim_end_matches("+++");
    let requeue = format!("{surplus}priority = 1\n+++");
    let (planned, _) = run(AuthorityStage::Unstick, &requeue);
    assert_eq!(
        planned[0].verdict,
        Err(Refusal::UnexpectedField("priority"))
    );
    // Unlock alone among the pump controls accepts an entry.
    let unlock = "+++\n[[action]]\nkind = \"unlock\"\nproject = \"/w/app\"\nneed = \"3f9a1c2e\"\nentry = \"u1\"\n+++";
    let (planned, _) = run(AuthorityStage::Unstick, unlock);
    assert_eq!(planned[0].verdict, Ok(()));
    // Blank surplus fields normalized to absent at parse time stay absent:
    // no UnexpectedField, and the parsed request carries nothing.
    let blanked = "+++\n[[action]]\nkind = \"letter-to-operator\"\nproject = \"/w/app\"\nentry = \" \"\nmodel = \"\"\n+++";
    let (planned, _) = run(AuthorityStage::Observe, blanked);
    assert_eq!(planned[0].verdict, Ok(()));
    assert_eq!(planned[0].request.entry, None);
    assert_eq!(planned[0].request.model, None);
}

#[test]
fn new_actions_validate_required_fields() {
    for (fields, missing) in [
        ("kind = \"read-need\"\nlimit = 5", "need"),
        ("kind = \"read-need-thread\"", "need"),
        ("kind = \"read-need-units\"\noffset = 2", "need"),
        ("kind = \"read-task-thread\"\nlimit = 5\noffset = 1", "step"),
        ("kind = \"set-role-model\"\nmodel = \"m1\"", "role"),
        ("kind = \"set-role-model\"\nrole = \"tugboat\"", "model"),
        ("kind = \"set-pool-cap\"\ncap = 5", "pool"),
        ("kind = \"set-pool-cap\"\npool = \"runpod\"", "cap"),
        ("kind = \"set-pool-band\"\nband = \"cheap\"", "pool"),
        ("kind = \"set-pool-band\"\npool = \"runpod\"", "band"),
        ("kind = \"set-priority\"\nneed = \"3f9a1c2e\"", "priority"),
        ("kind = \"upgrade\"", "target"),
        ("kind = \"requeue\"\nneed = \"3f9a1c2e\"", "step"),
        (
            "kind = \"answer\"\nneed = \"3f9a1c2e\"\nstep = \"b71d04aa\"",
            "word",
        ),
    ] {
        let reply = format!("+++\n[[action]]\nproject = \"/w/app\"\n{fields}\n+++");
        let (planned, _) = run(AuthorityStage::FileFixes, &reply);
        assert_eq!(
            planned[0].verdict,
            Err(Refusal::MissingField(missing)),
            "{fields}"
        );
    }
}

const TYPED_REQUEST: &str = "+++\n[[action]]\nkind = \"read-status\"\n\
    entry = \"entry-1\"\nfile = \"log\"\nfilter = \"open\"\nflow = \"main\"\n\
    pool = \"runpod\"\nto = \"operator\"\nsubject = \"status\"\n\
    body = \"all clear\"\ntarget = \"main\"\nrole = \"tugboat\"\n\
    model = \"m1\"\nband = \"cheap\"\nreasoning = \"because\"\n\
    priority = -3\ncap = 5\nlimit = 20\nlines = 40\noffset = 10\n\
    done_when = [\"green\", \"\"]\nwrite_scope = [\"src/\"]\n+++\n";

const BLANK_REQUEST: &str = "+++\n[[action]]\nkind = \"read-status\"\n\
    need = \"\"\nstep = \" \"\nword = \"\"\nnote = \"  \"\n\
    entry = \"\"\nfile = \" \"\nfilter = \"\"\nflow = \" \"\n\
    pool = \"\"\nto = \" \"\nsubject = \"\"\nbody = \" \"\n\
    target = \"\"\nrole = \" \"\nmodel = \"\"\nband = \" \"\n\
    reasoning = \"\"\ndone_when = [\"\", \"green\"]\n\
    write_scope = [\" \"]\n+++\n";

#[test]
fn request_argument_types_and_blank_normalization() {
    let typed = parse_envelope(TYPED_REQUEST).unwrap();
    assert_eq!(typed.len(), 1);
    assert_typed_fields(&typed[0]);

    let blank = parse_envelope(BLANK_REQUEST).unwrap();
    assert_eq!(blank.len(), 1);
    assert_blank_strings_normalize(&blank[0]);

    // Unknown fields are refused by the parser.
    let unknown = "+++\n[[action]]\nkind = \"read-status\"\nshell = \"ls\"\n+++";
    assert!(parse_envelope(unknown).is_err());

    assert_wrong_types_and_ranges_are_refused();
}

fn assert_typed_fields(request: &ActionRequest) {
    let strings = [
        ("entry", &request.entry),
        ("file", &request.file),
        ("filter", &request.filter),
        ("flow", &request.flow),
        ("pool", &request.pool),
        ("to", &request.to),
        ("subject", &request.subject),
        ("body", &request.body),
        ("target", &request.target),
        ("role", &request.role),
        ("model", &request.model),
        ("band", &request.band),
        ("reasoning", &request.reasoning),
    ];
    let expected = [
        "entry-1",
        "log",
        "open",
        "main",
        "runpod",
        "operator",
        "status",
        "all clear",
        "main",
        "tugboat",
        "m1",
        "cheap",
        "because",
    ];
    for ((name, value), want) in strings.iter().zip(expected) {
        assert_eq!(value.as_deref(), Some(want), "field {name}");
    }
    assert_eq!(request.priority, Some(-3));
    assert_eq!(request.cap, Some(5));
    assert_eq!(request.limit, Some(20));
    assert_eq!(request.lines, Some(40));
    assert_eq!(request.offset, Some(10));
    assert_eq!(
        request.done_when,
        Some(vec![String::from("green"), String::new()])
    );
    assert_eq!(request.write_scope, Some(vec![String::from("src/")]));
}

fn assert_blank_strings_normalize(request: &ActionRequest) {
    for (name, value) in [
        ("need", &request.need),
        ("step", &request.step),
        ("word", &request.word),
        ("note", &request.note),
        ("entry", &request.entry),
        ("file", &request.file),
        ("filter", &request.filter),
        ("flow", &request.flow),
        ("pool", &request.pool),
        ("to", &request.to),
        ("subject", &request.subject),
        ("body", &request.body),
        ("target", &request.target),
        ("role", &request.role),
        ("model", &request.model),
        ("band", &request.band),
        ("reasoning", &request.reasoning),
    ] {
        assert_eq!(*value, None, "blank {name} normalizes to None");
    }
    // Blank clauses inside the lists stay as written.
    assert_eq!(
        request.done_when,
        Some(vec![String::new(), String::from("green")])
    );
    assert_eq!(request.write_scope, Some(vec![String::from(" ")]));
}

fn assert_wrong_types_and_ranges_are_refused() {
    for field in [
        "priority = \"high\"",
        "priority = 9223372036854775808",
        "cap = \"ten\"",
        "cap = -1",
        "cap = 4294967296",
        "limit = -1",
        "limit = 4294967296",
        "lines = \"40\"",
        "lines = 4294967296",
        "offset = -1",
        "offset = 4294967296",
        "done_when = \"green\"",
        "write_scope = 3",
        "note = 7",
    ] {
        let reply = format!("+++\n[[action]]\nkind = \"read-status\"\n{field}\n+++");
        assert!(parse_envelope(&reply).is_err(), "{field} should not parse");
    }
    let max = "+++\n[[action]]\nkind = \"read-status\"\npriority = 9223372036854775807\n+++";
    assert_eq!(parse_envelope(max).unwrap()[0].priority, Some(i64::MAX));
}
