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

/// One otherwise-valid request per canonical kind: the fields it requires
/// beyond the common watched project. Keep one entry per `Action` variant.
const VALID_FIELDS: [(Action, &str); 28] = [
    (Action::ReadStatus, ""),
    (Action::ReadPumpLog, ""),
    (Action::ReadPumpHistory, ""),
    (Action::ReadNeed, "need = \"n1\"\n"),
    (Action::ReadNeedThread, "need = \"n1\"\n"),
    (Action::ReadNeedUnits, "need = \"n1\"\n"),
    (Action::ReadTaskThread, "step = \"s1\"\n"),
    (Action::ReadConfig, ""),
    (Action::ReadPool, ""),
    (Action::LetterToOperator, ""),
    (Action::Escalate, ""),
    (Action::PausePump, ""),
    (Action::ResumePump, ""),
    (Action::Requeue, "step = \"s1\"\n"),
    (Action::Replan, "need = \"n1\"\n"),
    (Action::Answer, "step = \"s1\"\nword = \"done\"\n"),
    (Action::Unlock, "need = \"n1\"\n"),
    (Action::ReplyNeed, "need = \"n1\"\n"),
    (Action::FileNeed, "need = \"n1\"\n"),
    (Action::DispatchNeed, "need = \"n1\"\n"),
    (Action::SetPriority, "need = \"n1\"\npriority = 2\n"),
    (Action::WithdrawNeed, "need = \"n1\"\n"),
    (Action::SetRoleModel, "role = \"r1\"\nmodel = \"m1\"\n"),
    (Action::SetPoolCap, "pool = \"p1\"\ncap = 3\n"),
    (Action::SetPoolBand, "pool = \"p1\"\nband = \"b1\"\n"),
    (Action::FileFixNeed, "need = \"n1\"\n"),
    (Action::Upgrade, "target = \"v9\"\n"),
    (Action::SelfRelease, ""),
];

/// One `[[action]]` envelope for `kind`, with an optional project line and
/// extra fields as written.
fn envelope(kind: Action, project: Option<&str>, fields: &str) -> String {
    let mut body = format!("kind = \"{}\"\n", kind.as_str());
    if let Some(project) = project {
        body.push_str(&format!("project = \"{project}\"\n"));
    }
    body.push_str(fields);
    format!("+++\n[[action]]\n{body}\n+++\n")
}

/// The implemented stage that opens `kind`; unimplemented stubs get the
/// lowest stage, since their gate denial is what a valid request meets.
fn opening_stage(kind: Action) -> AuthorityStage {
    let stage = kind.required_stage();
    if stage.implemented() {
        stage
    } else {
        AuthorityStage::Observe
    }
}

#[test]
fn all_actions_require_watched_projects() {
    assert_eq!(VALID_FIELDS.len(), 28, "one row per canonical action");
    for (kind, fields) in VALID_FIELDS {
        let stage = opening_stage(kind);
        let (planned, _) = run(stage, &envelope(kind, Some("/w/app"), fields));
        assert!(
            matches!(planned[0].verdict, Ok(()) | Err(Refusal::Gate(_))),
            "{kind:?} watched should clear fields and project: {:?}",
            planned[0].verdict
        );
        let (planned, _) = run(stage, &envelope(kind, Some("/elsewhere"), fields));
        assert_eq!(
            planned[0].verdict,
            Err(Refusal::UnwatchedProject),
            "{kind:?} unwatched"
        );
        let (planned, _) = run(stage, &envelope(kind, None, fields));
        assert_eq!(
            planned[0].verdict,
            Err(Refusal::MissingField("project")),
            "{kind:?} without a project"
        );
    }
}

#[test]
fn inapplicable_fields_are_refused() {
    let surplus = [
        (Action::ReadStatus, "entry = \"e1\"", "entry"),
        (Action::ReadStatus, "model = \"m1\"", "model"),
        (Action::ReadStatus, "priority = 1", "priority"),
        (Action::ReadStatus, "need = \"n1\"", "need"),
        (Action::ReadStatus, "lines = 5", "lines"),
        (Action::LetterToOperator, "model = \"m1\"", "model"),
        (Action::LetterToOperator, "entry = \"e1\"", "entry"),
        (Action::Escalate, "cap = 2", "cap"),
        (Action::Requeue, "step = \"s1\"\npriority = 1", "priority"),
        (Action::ReadNeed, "need = \"n1\"\npool = \"p1\"", "pool"),
        (Action::ReadPool, "file = \"f1\"", "file"),
    ];
    for (kind, fields, field) in surplus {
        let (planned, _) = run(opening_stage(kind), &envelope(kind, Some("/w/app"), fields));
        assert_eq!(
            planned[0].verdict,
            Err(Refusal::UnexpectedField(field)),
            "{kind:?} with {fields:?}"
        );
    }
    // A blank inapplicable string is an absent one, so the request passes.
    let blank = "+++\n[[action]]\nkind = \"read-status\"\nproject = \"/w/app\"\nentry = \"\"\nmodel = \" \"\n+++\n";
    let (planned, _) = run(AuthorityStage::Observe, blank);
    assert_eq!(planned[0].verdict, Ok(()));
    // Reads and letters honour their documented fields, and unlock alone
    // takes entry.
    let valid = [
        (
            Action::ReadPumpLog,
            "file = \"log\"\nlines = 40\nfilter = \"err\"\n",
        ),
        (Action::ReadPumpHistory, "need = \"n1\"\nflow = \"main\"\n"),
        (Action::ReadNeed, "need = \"n1\"\nlimit = 10\noffset = 5\n"),
        (Action::ReadNeedThread, "need = \"n1\"\nlimit = 10\n"),
        (Action::ReadNeedUnits, "need = \"n1\"\noffset = 5\n"),
        (
            Action::ReadTaskThread,
            "step = \"s1\"\nlimit = 10\noffset = 5\n",
        ),
        (Action::ReadPool, "pool = \"p1\"\n"),
        (
            Action::LetterToOperator,
            "to = \"operator\"\nsubject = \"status\"\nbody = \"all clear\"\n",
        ),
        (Action::Unlock, "need = \"n1\"\nentry = \"e1\"\n"),
    ];
    for (kind, fields) in valid {
        let (planned, _) = run(opening_stage(kind), &envelope(kind, Some("/w/app"), fields));
        assert_eq!(planned[0].verdict, Ok(()), "{kind:?} documented fields");
    }
}

#[test]
fn new_actions_validate_required_fields() {
    let cases = [
        (Action::ReadNeed, "", "need"),
        (Action::ReadNeedThread, "", "need"),
        (Action::ReadNeedUnits, "", "need"),
        (Action::ReadTaskThread, "", "step"),
        (Action::Requeue, "", "step"),
        (Action::Answer, "step = \"s1\"", "word"),
    ];
    for (kind, fields, field) in cases {
        let (planned, _) = run(opening_stage(kind), &envelope(kind, Some("/w/app"), fields));
        assert_eq!(
            planned[0].verdict,
            Err(Refusal::MissingField(field)),
            "{kind:?} without {field}"
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
