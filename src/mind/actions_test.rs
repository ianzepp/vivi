//! Tests for [`crate::mind::actions`].

use std::path::{Path, PathBuf};

use super::*;
use crate::mind::authority::Ledger;
use crate::mind::config::{AuthorityStage, Budgets};

const NOW: u64 = 1_000;

fn watched() -> Vec<PathBuf> {
    vec![PathBuf::from("/w/app"), PathBuf::from("/w/vivi")]
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
    let reply = "+++\n[[action]]\nkind = \"read-status\"\nproject = \"/w/app\"\nneed = \"\"\nstep = \" \"\nword = \"\"\n+++";
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
fn signed_numeric_fields_parse_but_are_refused_typed() {
    let reply = "+++\n[[action]]\nkind = \"read-status\"\nproject = \"/w/app\"\nlimit = -1\nlines = -5\npriority = -10\ncap = -3\n+++";
    let requests = parse_envelope(reply).unwrap();
    assert_eq!(requests[0].limit, Some(-1));
    assert_eq!(requests[0].lines, Some(-5));
    assert_eq!(requests[0].priority, Some(-10));
    assert_eq!(requests[0].cap, Some(-3));
    // Parse still accepts signed values; the field check now refuses them
    // with typed errors naming the one field.
    let (planned, _) = run(AuthorityStage::Observe, reply);
    assert_eq!(planned[0].verdict, Err(Refusal::InvalidField("limit")));
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
    let reply = "+++\n[[action]]\nkind = \"read-status\"\nproject = \"/w/app\"\nneed = \"\"\nstep = \" \"\nword = \"\\t\"\nnote = \"\"\nentry = \"\"\nstatus = \"\"\nfile = \"\"\nfilter = \"\"\nflow = \"\"\npool = \"\"\nbefore = \"\"\nrole = \"\"\nmodel = \"\"\nband = \"\"\nreasoning = \"\"\nto = \"\"\nsubject = \"\"\nbody = \"\"\ntarget = \"\"\n+++";
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
    // A blank list member is kept, not silently dropped: checked on the
    // parsed request, since the plan refuses lists a kind never reads.
    let listy = "+++\n[[action]]\nkind = \"read-status\"\nproject = \"/w/app\"\ndone_when = [\"\", \"gate green\"]\nwrite_scope = [\" \"]\n+++";
    let requests = parse_envelope(listy).unwrap();
    assert_eq!(
        requests[0].done_when,
        vec![String::new(), String::from("gate green")]
    );
    assert_eq!(requests[0].write_scope, vec![String::from(" ")]);
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
    let unlock = ActionRequest {
        project: Some("/w/app".into()),
        need: Some("3f9a1c2e".into()),
        ..request(Action::Unlock)
    };
    assert_eq!(
        unlock.argv().unwrap(),
        ["pump", "unlock", "3f9a1c2e", "--project", "/w/app"]
    );
    let entered = ActionRequest {
        entry: Some("fix1".into()),
        ..unlock
    };
    assert_eq!(
        entered.argv().unwrap(),
        [
            "pump",
            "unlock",
            "3f9a1c2e",
            "--entry",
            "fix1",
            "--project",
            "/w/app"
        ]
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
    assert!(line.contains("pump_action"), "{line}");
    assert!(line.contains("step=b71d04aa"), "{line}");
    assert!(line.contains("note=\"flaky gate\""), "{line}");
    assert_eq!(actuator.applied, vec![line]);
    assert!(!DryRun::default().feeds_back());
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
        "would send the operator an escalation via send need=3f9a1c2e project=/w/app note=\"locked at the operator gate\""
    );
}

#[test]
fn a_dry_run_unlock_names_the_entry_only_when_it_travels() {
    let bare = ActionRequest {
        project: Some("/w/app".into()),
        need: Some("3f9a1c2e".into()),
        ..request(Action::Unlock)
    };
    let line = DryRun::default().apply(&bare).unwrap();
    assert_eq!(
        line,
        "would unlock a stuck need via pump_action need=3f9a1c2e project=/w/app"
    );
    let entered = ActionRequest {
        entry: Some("fix1".into()),
        note: Some("restart at fix1".into()),
        ..bare
    };
    let line = DryRun::default().apply(&entered).unwrap();
    assert!(line.contains("entry=fix1"), "{line}");
    assert!(line.contains("note=\"restart at fix1\""), "{line}");
}

/// One row of the dry-run table: the request to describe, the substrings
/// its receipt must carry — the server tool it would drive and the action
/// parameters that matter — and the substrings it must not. Every request
/// names the watched root /w/app, as a permitted one does.
fn dry_cases() -> Vec<(
    ActionRequest,
    &'static [&'static str],
    &'static [&'static str],
)> {
    let mut cases: Vec<(
        ActionRequest,
        &'static [&'static str],
        &'static [&'static str],
    )> = vec![
        (
            ActionRequest {
                need: Some("3f9a1c2e".into()),
                status: Some("open".into()),
                limit: Some(25),
                ..request(Action::ReadStatus)
            },
            &[
                "would read pump records via pump_list",
                "need=3f9a1c2e",
                "status=open",
                "limit=25",
            ],
            &["pump_log"],
        ),
        (
            ActionRequest {
                file: Some("log".into()),
                lines: Some(80),
                filter: Some("flaky".into()),
                ..request(Action::ReadLog)
            },
            &["pump_log", "file=log", "lines=80", "filter=flaky"],
            &["pump_list"],
        ),
        (
            ActionRequest {
                need: Some("3f9a1c2e".into()),
                ..request(Action::ReadNeed)
            },
            &["need_show", "need=3f9a1c2e"],
            &[],
        ),
        (
            ActionRequest {
                need: Some("3f9a1c2e".into()),
                limit: Some(25),
                before: Some("msg_01".into()),
                ..request(Action::ReadNeedThread)
            },
            &["need_thread", "need=3f9a1c2e", "limit=25", "before=msg_01"],
            &[],
        ),
        (
            ActionRequest {
                step: Some("b71d04aa".into()),
                before: Some("msg_01".into()),
                ..request(Action::ReadTaskThread)
            },
            &["task_thread", "step=b71d04aa", "before=msg_01"],
            &[],
        ),
        (
            ActionRequest {
                flow: Some("build".into()),
                need: Some("3f9a1c2e".into()),
                ..request(Action::ReadFlow)
            },
            &["flow_show", "flow=build", "need=3f9a1c2e"],
            &[],
        ),
        (
            request(Action::ReadConfig),
            &["config_show", "project=/w/app"],
            &[],
        ),
        (
            ActionRequest {
                pool: Some("hands".into()),
                ..request(Action::ReadPool)
            },
            &["pool_show", "pool=hands"],
            &[],
        ),
        (
            ActionRequest {
                need: Some("3f9a1c2e".into()),
                note: Some("status".into()),
                ..request(Action::LetterToOperator)
            },
            &["would send the operator a letter via send", "need=3f9a1c2e"],
            &[],
        ),
        (
            ActionRequest {
                need: Some("3f9a1c2e".into()),
                note: Some("stuck".into()),
                ..request(Action::Escalate)
            },
            &[
                "would send the operator an escalation via send",
                "need=3f9a1c2e",
            ],
            &[],
        ),
        (
            ActionRequest {
                need: Some("3f9a1c2e".into()),
                note: Some("on it".into()),
                ..request(Action::ReplyNeed)
            },
            &["via reply", "need=3f9a1c2e", "note=\"on it\""],
            &[],
        ),
        (
            request(Action::PausePump),
            &["would pause the pump via pump_control"],
            &[],
        ),
        (
            request(Action::ResumePump),
            &["would resume the pump via pump_control"],
            &[],
        ),
        (
            ActionRequest {
                step: Some("b71d04aa".into()),
                ..request(Action::Requeue)
            },
            &["would requeue a step via pump_action", "step=b71d04aa"],
            &[],
        ),
        (
            ActionRequest {
                need: Some("3f9a1c2e".into()),
                ..request(Action::Replan)
            },
            &["would ask for a replan via pump_action", "need=3f9a1c2e"],
            &[],
        ),
        (
            ActionRequest {
                step: Some("b71d04aa".into()),
                word: Some("go".into()),
                ..request(Action::Answer)
            },
            &["pump_action", "step=b71d04aa", "word=go"],
            &[],
        ),
        (
            ActionRequest {
                need: Some("3f9a1c2e".into()),
                ..request(Action::Unlock)
            },
            &["would unlock a stuck need via pump_action", "need=3f9a1c2e"],
            &["entry="],
        ),
        (
            ActionRequest {
                need: Some("3f9a1c2e".into()),
                note: Some("duplicate".into()),
                ..request(Action::WithdrawNeed)
            },
            &["would withdraw a need via pump_action", "need=3f9a1c2e"],
            &[],
        ),
        (
            ActionRequest {
                need: Some("3f9a1c2e".into()),
                priority: Some(1),
                ..request(Action::SetPriority)
            },
            &["pump_priority action=set", "need=3f9a1c2e", "level=1"],
            &[],
        ),
        (
            ActionRequest {
                role: Some("hand".into()),
                model: Some("vendor/model.v1".into()),
                ..request(Action::SetRoleModel)
            },
            &["role_set", "role=hand", "model=vendor/model.v1"],
            &[],
        ),
        (
            ActionRequest {
                pool: Some("hands".into()),
                model: Some("vendor/model.v1".into()),
                cap: Some(2),
                ..request(Action::SetPoolCap)
            },
            &[
                "would set a pool's per-model cap via config_set",
                "pool=hands",
                "model=vendor/model.v1",
                "cap=2",
            ],
            &["pool_bands"],
        ),
        (
            ActionRequest {
                pool: Some("hands".into()),
                band: Some("P1".into()),
                model: Some("vendor/model.v1".into()),
                reasoning: Some("mid".into()),
                ..request(Action::SetPoolBand)
            },
            &[
                "pool_bands action=set",
                "pool=hands",
                "band=P1",
                "model=vendor/model.v1",
                "reasoning=mid",
            ],
            &["config_set"],
        ),
        (
            request(Action::FileNeed),
            &[
                "would file a new need",
                "filing is not wired to the server yet",
            ],
            &[],
        ),
        (
            request(Action::FileFixNeed),
            &[
                "would file a fix need",
                "filing is not wired to the server yet",
            ],
            &[],
        ),
        (
            ActionRequest {
                need: Some("3f9a1c2e".into()),
                ..request(Action::DispatchNeed)
            },
            &["dispatch is not wired", "need=3f9a1c2e"],
            &[],
        ),
        (request(Action::Upgrade), &["upgrade is not wired"], &[]),
        (
            request(Action::SelfRelease),
            &["self-release is not wired"],
            &[],
        ),
    ];
    for (request, _, _) in &mut cases {
        request.project = Some("/w/app".into());
    }
    cases
}

#[test]
fn dry_run_describes_the_intended_mcp_operation_for_every_kind() {
    for (request, present, absent) in dry_cases() {
        let line = DryRun::default().apply(&request).unwrap();
        assert!(line.contains("project=/w/app"), "{line}");
        for needle in present {
            assert!(line.contains(needle), "{line} lacks {needle:?}");
        }
        for needle in absent {
            assert!(!line.contains(needle), "{line} carries {needle:?}");
        }
    }
    // The table names every kind the field matrix knows, so a new action
    // cannot land without a described dry run.
    let mut covered: Vec<Action> = dry_cases()
        .into_iter()
        .map(|(request, _, _)| request.kind)
        .collect();
    covered.sort_unstable();
    covered.dedup();
    let mut all: Vec<Action> = matrix().into_iter().map(|row| row.kind).collect();
    all.sort_unstable();
    assert_eq!(covered, all);
}

/// One row of the field matrix: a kind and the TOML lines its required
/// fields need. Kinds with empty requireds still get a row, so the project
/// rule and the stray-field rule cover every action.
struct Row {
    kind: Action,
    required: &'static [(&'static str, &'static str)],
}

const NEED: (&str, &str) = ("need", "need = \"3f9a1c2e\"");
const STEP: (&str, &str) = ("step", "step = \"b71d04aa\"");
const NOTE: (&str, &str) = ("note", "note = \"r\"");
const POOL: (&str, &str) = ("pool", "pool = \"hands\"");
const MODEL: (&str, &str) = ("model", "model = \"vendor/model.v1\"");
const FILED: &[(&str, &str)] = &[
    ("to", "to = \"operator\""),
    ("subject", "subject = \"status\""),
    ("body", "body = \"all quiet\""),
    ("done_when", "done_when = [\"gate green\"]"),
    ("target", "target = \"build-log\""),
];

/// The fix policy pins file-fix-need's target to the vivi project.
const FILED_FIX: &[(&str, &str)] = &[
    ("to", "to = \"operator\""),
    ("subject", "subject = \"status\""),
    ("body", "body = \"all quiet\""),
    ("done_when", "done_when = [\"gate green\"]"),
    ("target", "target = \"vivi\""),
];

fn matrix() -> Vec<Row> {
    vec![
        Row {
            kind: Action::ReadStatus,
            required: &[],
        },
        Row {
            kind: Action::ReadLog,
            required: &[("file", "file = \"log\"")],
        },
        Row {
            kind: Action::ReadNeed,
            required: &[NEED],
        },
        Row {
            kind: Action::ReadNeedThread,
            required: &[NEED],
        },
        Row {
            kind: Action::ReadTaskThread,
            required: &[STEP],
        },
        Row {
            kind: Action::ReadFlow,
            required: &[("flow", "flow = \"build\"")],
        },
        Row {
            kind: Action::ReadConfig,
            required: &[],
        },
        Row {
            kind: Action::ReadPool,
            required: &[POOL],
        },
        Row {
            kind: Action::LetterToOperator,
            required: &[],
        },
        Row {
            kind: Action::Escalate,
            required: &[],
        },
        Row {
            kind: Action::PausePump,
            required: &[],
        },
        Row {
            kind: Action::ResumePump,
            required: &[],
        },
        Row {
            kind: Action::Requeue,
            required: &[NEED, STEP],
        },
        Row {
            kind: Action::Replan,
            required: &[NEED],
        },
        Row {
            kind: Action::Answer,
            required: &[NEED, STEP, ("word", "word = \"go\"")],
        },
        Row {
            kind: Action::Unlock,
            required: &[NEED],
        },
        Row {
            kind: Action::ReplyNeed,
            required: &[NEED, NOTE],
        },
        Row {
            kind: Action::WithdrawNeed,
            required: &[NEED, NOTE],
        },
        Row {
            kind: Action::SetPriority,
            required: &[NEED, ("priority", "priority = 1")],
        },
        Row {
            kind: Action::SetRoleModel,
            required: &[("role", "role = \"hand\""), MODEL],
        },
        Row {
            kind: Action::SetPoolCap,
            required: &[POOL, MODEL, ("cap", "cap = 2")],
        },
        Row {
            kind: Action::SetPoolBand,
            required: &[
                POOL,
                ("band", "band = \"P1\""),
                MODEL,
                ("reasoning", "reasoning = \"mid\""),
            ],
        },
        Row {
            kind: Action::FileNeed,
            required: FILED,
        },
        Row {
            kind: Action::FileFixNeed,
            required: FILED_FIX,
        },
        Row {
            kind: Action::DispatchNeed,
            required: &[NEED],
        },
        Row {
            kind: Action::Upgrade,
            required: &[],
        },
        Row {
            kind: Action::SelfRelease,
            required: &[],
        },
    ]
}

/// An envelope naming one action, optionally its project, plus field lines.
fn envelope(kind: Action, project: Option<&str>, lines: &[&str]) -> String {
    let mut body = format!("[[action]]\nkind = \"{}\"\n", kind.as_str());
    if let Some(project) = project {
        body.push_str("project = \"");
        body.push_str(project);
        body.push_str("\"\n");
    }
    for line in lines {
        body.push_str(line);
        body.push('\n');
    }
    format!("+++\n{body}+++")
}

/// Plan one envelope at the stage that opens its kind, against the
/// watched root the kind's policy names: the fix policy pins file-fix-need
/// to a watched root named vivi, every other kind plans at /w/app.
fn plan_at(kind: Action, project: Option<&str>, lines: &[&str]) -> Planned {
    let (mut planned, _) = run(kind.required_stage(), &envelope(kind, project, lines));
    planned.remove(0)
}

/// The watched root a kind plans its required fields against.
fn project_for(kind: Action) -> Option<&'static str> {
    match kind {
        Action::FileFixNeed => Some("/w/vivi"),
        _ => Some("/w/app"),
    }
}

fn as_lines(lines: &[String]) -> Vec<&str> {
    lines.iter().map(String::as_str).collect()
}

/// The kind's required TOML lines, with `name`'s line written as `line`
/// (appended when the kind does not require that field).
fn with_field(kind: Action, name: &str, line: &str) -> Vec<String> {
    let row = matrix()
        .into_iter()
        .find(|row| row.kind == kind)
        .unwrap_or_else(|| panic!("{kind:?} is in the matrix"));
    let mut replaced = false;
    let mut out: Vec<String> = row
        .required
        .iter()
        .map(|&(field, text)| {
            if field == name {
                replaced = true;
                String::from(line)
            } else {
                String::from(text)
            }
        })
        .collect();
    if !replaced {
        out.push(String::from(line));
    }
    out
}

/// The kind's required TOML lines, as written.
fn required_lines(kind: Action) -> Vec<String> {
    let row = matrix()
        .into_iter()
        .find(|row| row.kind == kind)
        .unwrap_or_else(|| panic!("{kind:?} is in the matrix"));
    row.required
        .iter()
        .map(|&(_, text)| String::from(text))
        .collect()
}

/// The kind's required TOML lines minus the named one.
fn without_field(kind: Action, name: &str) -> Vec<String> {
    let row = matrix()
        .into_iter()
        .find(|row| row.kind == kind)
        .unwrap_or_else(|| panic!("{kind:?} is in the matrix"));
    row.required
        .iter()
        .filter(|&&(field, _)| field != name)
        .map(|&(_, text)| String::from(text))
        .collect()
}

/// Kinds and a nonblank field each never reads.
const STRAYS: &[(Action, &str)] = &[
    (Action::ReadStatus, "note = \"look\""),
    (Action::ReadNeed, "limit = 5"),
    (Action::ReadConfig, "pool = \"hands\""),
    (Action::PausePump, "note = \"why\""),
    (Action::Requeue, "flow = \"build\""),
    (Action::Answer, "priority = 1"),
    (Action::SetRoleModel, "need = \"3f9a1c2e\""),
    (Action::SetPoolCap, "need = \"3f9a1c2e\""),
    (Action::SetPoolBand, "need = \"3f9a1c2e\""),
    (Action::FileNeed, "need = \"3f9a1c2e\""),
    (Action::DispatchNeed, "step = \"b71d04aa\""),
    (Action::LetterToOperator, "status = \"open\""),
];

/// Kinds, a well-typed but malformed value for a field they do read, and
/// the field's name.
const INVALID: &[(Action, &str, &str)] = &[
    (Action::ReadStatus, "limit = 0", "limit"),
    (Action::ReadStatus, "limit = 101", "limit"),
    (Action::ReadStatus, "limit = -1", "limit"),
    (Action::ReadStatus, "status = \"closed\"", "status"),
    (Action::ReadLog, "lines = 0", "lines"),
    (Action::ReadLog, "lines = 501", "lines"),
    (Action::ReadLog, "lines = -3", "lines"),
    (Action::ReadFlow, "flow = \"ship\"", "flow"),
    (Action::SetPriority, "priority = -1", "priority"),
    (Action::SetPoolCap, "cap = -1", "cap"),
    (Action::SetPoolCap, "cap = 4294967296", "cap"),
    (Action::SetPoolBand, "band = \"P6\"", "band"),
    (Action::SetPoolBand, "reasoning = \"way!too\"", "reasoning"),
    (Action::SetRoleModel, "model = \"not a slug\"", "model"),
];

/// Kinds and a value that parses but is not a plain handle.
const BAD_TOKENS: &[(Action, &str, &str)] = &[
    (Action::ReadNeedThread, "before = \"--flag\"", "before"),
    (Action::Answer, "word = \"nope nope\"", "word"),
    (Action::Requeue, "need = \"vendor/model.v1\"", "need"),
];

#[test]
fn every_kind_accepts_its_required_fields_and_watched_project() {
    for row in matrix() {
        if !row.kind.required_stage().implemented() {
            continue; // upgrade and self-release stay unimplemented stubs
        }
        let fields = required_lines(row.kind);
        let lines = as_lines(&fields);
        let planned = plan_at(row.kind, project_for(row.kind), &lines);
        assert_eq!(planned.verdict, Ok(()), "{}", row.kind.as_str());
    }
}

#[test]
fn required_fields_are_refused_when_missing() {
    for row in matrix() {
        for &(name, _) in row.required {
            let fields = without_field(row.kind, name);
            let lines = as_lines(&fields);
            let planned = plan_at(row.kind, Some("/w/app"), &lines);
            assert_eq!(
                planned.verdict,
                Err(Refusal::MissingField(name)),
                "{} without {name}",
                row.kind.as_str()
            );
        }
    }
}

#[test]
fn blank_required_fields_are_missing() {
    for row in matrix() {
        for &(name, line) in row.required {
            // Numeric and list requireds cannot be written blank: the TOML
            // parse refuses the type mismatch instead.
            if !line.contains('"') || name == "done_when" {
                continue;
            }
            let blank = format!("{name} = \"\"");
            let fields = with_field(row.kind, name, &blank);
            let lines = as_lines(&fields);
            let planned = plan_at(row.kind, Some("/w/app"), &lines);
            assert_eq!(
                planned.verdict,
                Err(Refusal::MissingField(name)),
                "{} blank {name}",
                row.kind.as_str()
            );
        }
    }
}

#[test]
fn inappropriate_fields_are_refused() {
    for &(kind, stray) in STRAYS {
        let name = stray.split('=').next().unwrap_or_default().trim();
        let fields = with_field(kind, name, stray);
        let lines = as_lines(&fields);
        let planned = plan_at(kind, Some("/w/app"), &lines);
        assert_eq!(
            planned.verdict,
            Err(Refusal::UnexpectedField(name)),
            "{kind:?} stray {stray}"
        );
    }
}

#[test]
fn invalid_values_are_typed_per_field() {
    for &(kind, assignment, name) in INVALID {
        let fields = with_field(kind, name, assignment);
        let lines = as_lines(&fields);
        let planned = plan_at(kind, Some("/w/app"), &lines);
        assert_eq!(
            planned.verdict,
            Err(Refusal::InvalidField(name)),
            "{kind:?} {assignment}"
        );
    }
    for &(kind, assignment, name) in BAD_TOKENS {
        let fields = with_field(kind, name, assignment);
        let lines = as_lines(&fields);
        let planned = plan_at(kind, Some("/w/app"), &lines);
        assert_eq!(
            planned.verdict,
            Err(Refusal::BadHandle(name)),
            "{kind:?} {assignment}"
        );
    }
}

#[test]
fn value_boundaries_are_enforced() {
    let passes = |kind: Action, line: &str| {
        let name = line.split('=').next().unwrap_or_default().trim();
        let fields = with_field(kind, name, line);
        plan_at(kind, Some("/w/app"), &as_lines(&fields)).verdict
    };
    assert_eq!(passes(Action::ReadStatus, "limit = 100"), Ok(()));
    assert_eq!(passes(Action::ReadStatus, "limit = 1"), Ok(()));
    assert_eq!(passes(Action::ReadLog, "lines = 500"), Ok(()));
    assert_eq!(passes(Action::ReadLog, "lines = 1"), Ok(()));
    assert_eq!(passes(Action::SetPriority, "priority = 0"), Ok(()));
    assert_eq!(passes(Action::SetPoolCap, "cap = 4294967295"), Ok(()));
    assert_eq!(passes(Action::SetPoolBand, "band = \"P0\""), Ok(()));
    assert_eq!(passes(Action::SetPoolBand, "band = \"P5\""), Ok(()));
    let longest = format!("reasoning = \"{}\"", "a".repeat(32));
    assert_eq!(passes(Action::SetPoolBand, &longest), Ok(()));
    let overlong = format!("reasoning = \"{}\"", "a".repeat(33));
    assert_eq!(
        passes(Action::SetPoolBand, &overlong),
        Err(Refusal::InvalidField("reasoning"))
    );
}

#[test]
fn all_actions_require_watched_projects() {
    for row in matrix() {
        let fields = required_lines(row.kind);
        let lines = as_lines(&fields);
        let missing = plan_at(row.kind, None, &lines);
        assert_eq!(
            missing.verdict,
            Err(Refusal::MissingField("project")),
            "{}",
            row.kind.as_str()
        );
        let unwatched = plan_at(row.kind, Some("/etc"), &lines);
        assert_eq!(
            unwatched.verdict,
            Err(Refusal::UnwatchedProject),
            "{}",
            row.kind.as_str()
        );
        // With a watched root the project layer passes: what is left is
        // the gate's business (Ok, or the stub denial for upgrade/
        // self-release), save that the vivi-only fix policy may still
        // refuse file-fix-need's project or target.
        let watched = plan_at(row.kind, Some("/w/app"), &lines);
        assert!(
            !matches!(
                watched.verdict,
                Err(Refusal::MissingField(_) | Refusal::UnwatchedProject)
            ),
            "{}",
            row.kind.as_str()
        );
    }
}

#[test]
fn model_slugs_are_not_handles() {
    // The dot and slash that make a vendor-qualified slug a model keep it
    // out of the handle rule: the model field takes slugs, handle fields
    // refuse them.
    assert!(crate::pool::config::is_model_slug("vendor/model.v1"));
    assert!(!super::fields::plain_handle("vendor/model.v1"));
    let planned = plan_at(
        Action::SetRoleModel,
        Some("/w/app"),
        &["role = \"hand\"", "model = \"vendor/model.v1\""],
    );
    assert_eq!(planned.verdict, Ok(()));
    let planned = plan_at(
        Action::Requeue,
        Some("/w/app"),
        &["need = \"vendor/model.v1\"", "step = \"b71d04aa\""],
    );
    assert_eq!(planned.verdict, Err(Refusal::BadHandle("need")));
}

/// Plan one filing request: the kind's required lines with `line` swapped
/// in (appended when the field is only optional), at the kind's root.
fn plan_filed(kind: Action, line: &str) -> Planned {
    let name = line.split('=').next().unwrap_or_default().trim();
    let fields = with_field(kind, name, line);
    plan_at(kind, project_for(kind), &as_lines(&fields))
}

/// A TOML list line of quoted strings.
fn quoted_list(name: &str, items: &[String]) -> String {
    let joined = items
        .iter()
        .map(|item| format!("\"{item}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{name} = [{joined}]")
}

#[test]
fn filing_prose_limits_are_bounded() {
    let subject = format!(
        "subject = \"{}\"",
        "s".repeat(super::filing_fields::MAX_SUBJECT)
    );
    assert_eq!(plan_filed(Action::FileNeed, &subject).verdict, Ok(()));
    let subject = format!(
        "subject = \"{}\"",
        "s".repeat(super::filing_fields::MAX_SUBJECT + 1)
    );
    assert_eq!(
        plan_filed(Action::FileNeed, &subject).verdict,
        Err(Refusal::FilingTooLarge("subject"))
    );
    let body = format!("body = \"{}\"", "b".repeat(super::filing_fields::MAX_BODY));
    assert_eq!(plan_filed(Action::FileNeed, &body).verdict, Ok(()));
    let body = format!(
        "body = \"{}\"",
        "b".repeat(super::filing_fields::MAX_BODY + 1)
    );
    assert_eq!(
        plan_filed(Action::FileNeed, &body).verdict,
        Err(Refusal::FilingTooLarge("body"))
    );
}

#[test]
fn done_when_clauses_are_bounded_and_nonblank() {
    use super::filing_fields::{MAX_CLAUSE, MAX_CLAUSES};
    let clauses =
        |count: usize, len: usize| quoted_list("done_when", &vec!["x".repeat(len); count]);
    assert_eq!(
        plan_filed(Action::FileNeed, &clauses(MAX_CLAUSES, MAX_CLAUSE)).verdict,
        Ok(())
    );
    assert_eq!(
        plan_filed(Action::FileNeed, &clauses(MAX_CLAUSES + 1, 1)).verdict,
        Err(Refusal::FilingTooLarge("done_when"))
    );
    assert_eq!(
        plan_filed(Action::FileNeed, &clauses(1, MAX_CLAUSE + 1)).verdict,
        Err(Refusal::FilingTooLarge("done_when"))
    );
    // A whitespace-only clause is invalid content, not merely short.
    assert_eq!(
        plan_filed(Action::FileNeed, "done_when = [\"gate green\", \" \"]").verdict,
        Err(Refusal::InvalidField("done_when"))
    );
    // An empty required list is an absent one.
    assert_eq!(
        plan_filed(Action::FileNeed, "done_when = []").verdict,
        Err(Refusal::MissingField("done_when"))
    );
}

#[test]
fn write_scope_paths_are_bounded_and_relative() {
    use super::filing_fields::{MAX_WRITE_PATH, MAX_WRITE_PATHS};
    let scope = |count: usize| quoted_list("write_scope", &vec![String::from("src/lib.rs"); count]);
    assert_eq!(
        plan_filed(Action::FileNeed, &scope(MAX_WRITE_PATHS)).verdict,
        Ok(())
    );
    assert_eq!(
        plan_filed(Action::FileNeed, &scope(MAX_WRITE_PATHS + 1)).verdict,
        Err(Refusal::FilingTooLarge("write_scope"))
    );
    let longest = format!("write_scope = [\"{}\"]", "a".repeat(MAX_WRITE_PATH));
    assert_eq!(plan_filed(Action::FileNeed, &longest).verdict, Ok(()));
    let overlong = format!("write_scope = [\"{}\"]", "a".repeat(MAX_WRITE_PATH + 1));
    assert_eq!(
        plan_filed(Action::FileNeed, &overlong).verdict,
        Err(Refusal::FilingTooLarge("write_scope"))
    );
    for bad in ["\" \"", "\"/etc/passwd\"", "\"src/../lib.rs\"", "\"..\""] {
        let line = format!("write_scope = [{bad}]");
        assert_eq!(
            plan_filed(Action::FileNeed, &line).verdict,
            Err(Refusal::InvalidField("write_scope")),
            "{bad}"
        );
    }
}

#[test]
fn filing_bodies_are_prose_not_front_matter() {
    let letter = "body = \"+++\\n[[letter]]\\nto = \\\"operator\\\"\\n+++\"";
    assert_eq!(
        plan_filed(Action::FileNeed, letter).verdict,
        Err(Refusal::InvalidField("body"))
    );
    assert_eq!(
        plan_filed(Action::FileNeed, "body = \"  +++ first\"").verdict,
        Err(Refusal::InvalidField("body"))
    );
    // Plain prose files clean, scope and all.
    let mut fields = with_field(
        Action::FileNeed,
        "body",
        "body = \"The retry loop flakes when the gate times out.\"",
    );
    fields.push(String::from("write_scope = [\"src/lib.rs\"]"));
    let planned = plan_at(Action::FileNeed, Some("/w/app"), &as_lines(&fields));
    assert_eq!(planned.verdict, Ok(()));
}

#[test]
fn file_fix_need_requires_watched_vivi() {
    // A watched root that is not named vivi refuses as project.
    let fields = required_lines(Action::FileFixNeed);
    let planned = plan_at(Action::FileFixNeed, Some("/w/app"), &as_lines(&fields));
    assert_eq!(planned.verdict, Err(Refusal::InvalidField("project")));
    // Even at the vivi root the target must say vivi.
    let fields = with_field(Action::FileFixNeed, "target", "target = \"build-log\"");
    let planned = plan_at(Action::FileFixNeed, Some("/w/vivi"), &as_lines(&fields));
    assert_eq!(planned.verdict, Err(Refusal::InvalidField("target")));
    // The vivi root with a vivi target files.
    let fields = required_lines(Action::FileFixNeed);
    let planned = plan_at(Action::FileFixNeed, Some("/w/vivi"), &as_lines(&fields));
    assert_eq!(planned.verdict, Ok(()));
    // General file-need may still file to the vivi root with another target.
    let fields = required_lines(Action::FileNeed);
    let planned = plan_at(Action::FileNeed, Some("/w/vivi"), &as_lines(&fields));
    assert_eq!(planned.verdict, Ok(()));
}

/// The role/pool tuning action body, the cheapest project-budgeted kind.
fn tuning_body(project: &str) -> String {
    format!(
        "[[action]]\nkind = \"set-role-model\"\nproject = \"{project}\"\nrole = \"hand\"\nmodel = \"vendor/model.v1\"\n"
    )
}

/// One TOML action body line list, as lines inside the `+++` fences.
fn fenced(body: &str) -> String {
    format!("+++\n{body}+++")
}

/// The project-budgeted kinds and the watched root each plans at.
const PROJECT_BUDGETED: &[(Action, &str)] = &[
    (Action::FileNeed, "/w/app"),
    (Action::FileFixNeed, "/w/vivi"),
    (Action::SetRoleModel, "/w/app"),
    (Action::SetPoolCap, "/w/app"),
    (Action::SetPoolBand, "/w/app"),
];

#[test]
fn every_project_budgeted_action_spends_the_shared_project_bucket() {
    for &(kind, project) in PROJECT_BUDGETED {
        let fields = required_lines(kind);
        let lines = as_lines(&fields);
        let (planned, ledger) = run(
            kind.required_stage(),
            &envelope(kind, Some(project), &lines),
        );
        assert_eq!(planned[0].verdict, Ok(()), "{}", kind.as_str());
        let key = authority::project_key(Path::new(project));
        assert_eq!(ledger[&key], vec![NOW], "{}", kind.as_str());
        // Filing never fabricates a destination need: no bare need key is
        // written, every spend lands in the reserved project namespace.
        assert!(
            ledger.keys().all(|key| key.starts_with("project:")),
            "{}",
            kind.as_str()
        );
    }
}

#[test]
fn mixed_filing_and_tuning_exhaust_one_project_window() {
    // run() caps each window at one action: the first permitted project
    // action spends the shared bucket, and a different kind on the same
    // project is then refused without spending again.
    let filing = format!(
        "[[action]]\nkind = \"file-need\"\nproject = \"/w/app\"\n{}",
        required_lines(Action::FileNeed)
            .into_iter()
            .map(|line| format!("{line}\n"))
            .collect::<String>()
    );
    let (planned, ledger) = run(
        AuthorityStage::FileFixes,
        &fenced(&format!("{filing}{}", tuning_body("/w/app"))),
    );
    assert_eq!(planned[0].verdict, Ok(()));
    assert_eq!(
        planned[1].verdict,
        Err(Refusal::Gate(Denial::ProjectBudget {
            project: authority::project_key(Path::new("/w/app")),
            used: 1,
            limit: 1,
        }))
    );
    assert_eq!(
        ledger[&authority::project_key(Path::new("/w/app"))],
        vec![NOW]
    );
}

#[test]
fn project_windows_are_independent_between_watched_roots() {
    let (planned, ledger) = run(
        AuthorityStage::Unstick,
        &fenced(&format!(
            "{}{}{}",
            tuning_body("/w/app"),
            tuning_body("/w/vivi"),
            tuning_body("/w/app")
        )),
    );
    assert_eq!(planned[0].verdict, Ok(()));
    assert_eq!(planned[1].verdict, Ok(()));
    assert_eq!(
        planned[2].verdict,
        Err(Refusal::Gate(Denial::ProjectBudget {
            project: authority::project_key(Path::new("/w/app")),
            used: 1,
            limit: 1,
        }))
    );
    // The refusal spent nothing: one entry per project key.
    assert_eq!(
        ledger[&authority::project_key(Path::new("/w/app"))],
        vec![NOW]
    );
    assert_eq!(
        ledger[&authority::project_key(Path::new("/w/vivi"))],
        vec![NOW]
    );
}

#[test]
fn an_expired_project_entry_frees_the_bucket_in_plan() {
    let budgets = Budgets {
        per_need_actions: 1,
        per_need_window_secs: 1800,
        max_wakes_per_hour: 6,
    };
    let gate = Gate {
        stage: AuthorityStage::Unstick,
        paused: false,
        budgets: &budgets,
        now: 1_000_000,
    };
    let mut ledger = Ledger::new();
    let key = authority::project_key(Path::new("/w/app"));
    // NOW is far outside the 1800s window the gate above looks through.
    ledger.insert(key, vec![NOW]);
    let requests = parse_envelope(&fenced(&tuning_body("/w/app"))).unwrap();
    let planned = plan(requests, &gate, &watched(), &mut ledger);
    assert_eq!(planned[0].verdict, Ok(()));
    // The out-of-window entry was pruned on spend, not counted.
    assert_eq!(
        ledger[&authority::project_key(Path::new("/w/app"))],
        vec![1_000_000]
    );
}

#[test]
fn local_refusals_spend_nothing() {
    // A stage refusal at the gate leaves every bucket untouched.
    let (planned, ledger) = run(AuthorityStage::Observe, &fenced(&tuning_body("/w/app")));
    assert!(matches!(
        planned[0].verdict,
        Err(Refusal::Gate(Denial::Stage { .. }))
    ));
    assert!(ledger.is_empty());
    // A validation refusal after the gate order never spends either.
    let oversized = format!(
        "subject = \"{}\"",
        "s".repeat(super::filing_fields::MAX_SUBJECT + 1)
    );
    let fields = with_field(Action::FileNeed, "subject", &oversized);
    let body = format!(
        "[[action]]\nkind = \"file-need\"\nproject = \"/w/app\"\n{}",
        as_lines(&fields)
            .into_iter()
            .map(|line| format!("{line}\n"))
            .collect::<String>()
    );
    let (planned, ledger) = run(AuthorityStage::FileFixes, &fenced(&body));
    assert_eq!(planned[0].verdict, Err(Refusal::FilingTooLarge("subject")));
    assert!(ledger.is_empty());
}

#[test]
fn project_mutations_cannot_bypass_budget_with_a_need_field() {
    // A `need` field is not one the tuning kinds read, so it cannot route a
    // project mutation into the need bucket: the field is refused before the
    // gate, and a refusal never spends.
    let tuning = tuning_body("/w/app");
    let with_need = tuning.replace(
        "project = \"/w/app\"",
        "project = \"/w/app\"\nneed = \"3f9a1c2e\"",
    );
    let (planned, ledger) = run(
        AuthorityStage::Unstick,
        &fenced(&format!("{tuning}{with_need}{}", tuning_body("/w/app"))),
    );
    assert_eq!(planned[0].verdict, Ok(()));
    assert_eq!(planned[1].verdict, Err(Refusal::UnexpectedField("need")));
    // The same tuning without the stray need field is refused on the spent
    // project window instead — the need field rescues nothing.
    assert!(matches!(
        planned[2].verdict,
        Err(Refusal::Gate(Denial::ProjectBudget { .. }))
    ));
    // The ledger holds exactly the one permitted spend, under the project
    // key: nothing under the bare need handle.
    assert!(ledger.get("3f9a1c2e").is_none());
    assert_eq!(
        ledger[&authority::project_key(Path::new("/w/app"))],
        vec![NOW]
    );
}

#[test]
fn project_keys_cannot_pass_the_handle_validator() {
    for project in ["/w/app", "/w/vivi", "plain", "app"] {
        let key = authority::project_key(Path::new(project));
        assert!(
            !super::fields::plain_handle(&key),
            "{key} must not pass the handle validator"
        );
    }
}
