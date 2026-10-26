//! Turning each permitted action into its one server tool call.

use std::cell::RefCell;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{HttpCaller, McpActuator, ToolCaller};
use crate::mind::actions::{ActionRequest, Actuator};
use crate::mind::authority::Action;

#[derive(Default)]
struct Recorder {
    calls: RefCell<Vec<(String, Value)>>,
    fail_with: Option<String>,
}

impl ToolCaller for Recorder {
    fn call(&self, tool: &str, arguments: &Value) -> Result<String, String> {
        self.calls
            .borrow_mut()
            .push((tool.to_string(), arguments.clone()));
        match &self.fail_with {
            Some(error) => Err(error.clone()),
            None => Ok(format!("{tool} ok")),
        }
    }
}

fn request(kind: Action) -> ActionRequest {
    ActionRequest {
        kind,
        project: Some(PathBuf::from("/srv/projects/app")),
        need: Some("3f9a1c2e".into()),
        step: Some("a1b2c3d4".into()),
        word: Some("yes".into()),
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

/// A read-status request with no filters at all.
fn bare_status() -> ActionRequest {
    let mut bare = request(Action::ReadStatus);
    bare.need = None;
    bare
}

fn run(request: &ActionRequest) -> (Result<String, String>, Vec<(String, Value)>) {
    let mut actuator = McpActuator::new(Recorder::default());
    let result = actuator.apply(request);
    let calls = actuator.caller.calls.borrow().clone();
    (result, calls)
}

#[test]
fn pause_and_resume_call_pump_control_with_the_project_name() {
    let (result, calls) = run(&request(Action::PausePump));
    assert_eq!(result, Ok("pump_control: pump_control ok".into()));
    assert_eq!(
        calls,
        vec![(
            "pump_control".to_string(),
            json!({ "project": "app", "action": "pause" })
        )]
    );
    let (_, calls) = run(&request(Action::ResumePump));
    assert_eq!(calls[0].1["action"], "resume");
}

#[test]
fn pump_actions_carry_their_fields_and_leave_unset_ones_out() {
    let (_, calls) = run(&request(Action::Unlock));
    assert_eq!(
        calls[0],
        (
            "pump_action".to_string(),
            json!({ "project": "app", "action": "unlock", "need": "3f9a1c2e" })
        )
    );
    let mut with_note = request(Action::Requeue);
    with_note.note = Some("flaky gate".into());
    let (_, calls) = run(&with_note);
    assert_eq!(
        calls[0].1,
        json!({
            "project": "app",
            "action": "requeue",
            "steps": ["a1b2c3d4"],
            "reason": "flaky gate"
        })
    );
    let (_, calls) = run(&request(Action::Answer));
    assert_eq!(calls[0].1["word"], "yes");
    assert_eq!(calls[0].1["step"], "a1b2c3d4");
    let (_, calls) = run(&request(Action::Replan));
    assert_eq!(calls[0].1["need"], "3f9a1c2e");
}

#[test]
fn unlock_travels_with_its_entry_when_one_is_named() {
    let mut restart = request(Action::Unlock);
    restart.entry = Some("rework1".into());
    restart.note = Some("gate flaked".into());
    let (result, calls) = run(&restart);
    assert_eq!(result, Ok("pump_action: pump_action ok".into()));
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0],
        (
            "pump_action".to_string(),
            json!({
                "project": "app",
                "action": "unlock",
                "need": "3f9a1c2e",
                "entry": "rework1",
                "note": "gate flaked"
            })
        )
    );
    // No entry named: the key stays out so the server's own restart
    // point holds; the call count stays at one either way.
    let (_, calls) = run(&request(Action::Unlock));
    assert_eq!(calls.len(), 1);
    assert!(calls[0].1.get("entry").is_none());
}

#[test]
fn a_reply_need_replies_on_the_needs_thread_without_a_from() {
    let mut reply = request(Action::ReplyNeed);
    reply.note = Some("the fix landed".into());
    let (result, calls) = run(&reply);
    assert_eq!(result, Ok("reply: reply ok".into()));
    // One call, and no implicit unlock after it.
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "reply");
    assert_eq!(
        calls[0].1,
        json!({ "project": "app", "handle": "3f9a1c2e", "body": "the fix landed" })
    );
    // Authorship is never sent: the authenticated operator grant decides.
    assert!(calls[0].1.get("from").is_none());
}

#[test]
fn set_priority_sends_the_level_even_when_it_is_zero() {
    let mut level = request(Action::SetPriority);
    level.priority = Some(0);
    let (result, calls) = run(&level);
    assert_eq!(result, Ok("pump_priority: pump_priority ok".into()));
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0],
        (
            "pump_priority".to_string(),
            json!({
                "project": "app",
                "action": "set",
                "need": "3f9a1c2e",
                "level": 0
            })
        )
    );
    level.priority = Some(3);
    let (_, calls) = run(&level);
    assert_eq!(calls[0].1["level"], 3);
}

#[test]
fn a_withdrawal_names_one_need_and_its_reason() {
    let mut withdraw = request(Action::WithdrawNeed);
    withdraw.note = Some("superseded by the refiled need".into());
    let (result, calls) = run(&withdraw);
    assert_eq!(result, Ok("pump_action: pump_action ok".into()));
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "pump_action");
    assert_eq!(calls[0].1["action"], "withdraw");
    assert_eq!(calls[0].1["needs"], json!(["3f9a1c2e"]));
    assert_eq!(calls[0].1["reason"], "superseded by the refiled need");
}

#[test]
fn a_server_refusal_on_a_new_operator_action_is_the_failure_text() {
    for (kind, tool) in [
        (Action::ReplyNeed, "reply"),
        (Action::SetPriority, "pump_priority"),
        (Action::WithdrawNeed, "pump_action"),
    ] {
        let mut actuator = McpActuator::new(Recorder {
            fail_with: Some("the key is not an operator".into()),
            ..Recorder::default()
        });
        let mut ask = request(kind);
        if kind != Action::SetPriority {
            ask.note = Some("because".into());
        } else {
            ask.priority = Some(2);
        }
        let error = actuator.apply(&ask).unwrap_err();
        assert_eq!(error, format!("{tool}: the key is not an operator"));
        // One call, and no retry or second tool after the refusal.
        assert_eq!(actuator.caller.calls.borrow().len(), 1);
    }
}

#[test]
fn read_status_is_one_pump_list_call_with_absent_values_left_out() {
    // The request's need travels; the absent status and limit are omitted
    // rather than sent as nulls, so open steps stay the server's default.
    let (result, calls) = run(&request(Action::ReadStatus));
    assert_eq!(result, Ok("pump_list: pump_list ok".into()));
    assert_eq!(
        calls,
        vec![(
            "pump_list".to_string(),
            json!({ "project": "app", "need": "3f9a1c2e" })
        )]
    );
    let (_, calls) = run(&bare_status());
    assert_eq!(
        calls,
        vec![("pump_list".to_string(), json!({ "project": "app" }))]
    );
}

#[test]
fn read_status_forwards_the_history_need_and_limit_filters_in_one_call() {
    let mut filtered = request(Action::ReadStatus);
    filtered.status = Some("done".into());
    filtered.limit = Some(5);
    let (_, calls) = run(&filtered);
    assert_eq!(
        calls,
        vec![(
            "pump_list".to_string(),
            json!({
                "project": "app",
                "need": "3f9a1c2e",
                "status": "done",
                "limit": 5
            })
        )]
    );
}

#[test]
fn read_log_names_a_required_file_and_leaves_unset_options_out() {
    let (result, calls) = run(&request(Action::ReadLog));
    assert_eq!(result, Err("the action names no file".into()));
    assert!(calls.is_empty());

    let mut tail = request(Action::ReadLog);
    tail.file = Some("log".into());
    let (_, calls) = run(&tail);
    assert_eq!(
        calls,
        vec![(
            "pump_log".to_string(),
            json!({ "project": "app", "file": "log" })
        )]
    );

    tail.lines = Some(40);
    tail.filter = Some("requeue".into());
    let (_, calls) = run(&tail);
    assert_eq!(
        calls,
        vec![(
            "pump_log".to_string(),
            json!({
                "project": "app",
                "file": "log",
                "lines": 40,
                "filter": "requeue"
            })
        )]
    );
}

#[test]
fn read_need_is_one_need_show_call_naming_the_handle() {
    let (result, calls) = run(&request(Action::ReadNeed));
    assert_eq!(result, Ok("need_show: need_show ok".into()));
    assert_eq!(
        calls,
        vec![(
            "need_show".to_string(),
            json!({ "project": "app", "handle": "3f9a1c2e" })
        )]
    );
    let mut no_need = request(Action::ReadNeed);
    no_need.need = None;
    let (result, calls) = run(&no_need);
    assert_eq!(result, Err("the action names no need".into()));
    assert!(calls.is_empty());
}

#[test]
fn thread_reads_take_the_server_defaults_until_a_page_is_named() {
    // Each thread read is exactly one call: the need's thread by the
    // need's handle, the task's by the step's, with no limit or before
    // sent so the server's defaults hold.
    let (result, calls) = run(&request(Action::ReadNeedThread));
    assert_eq!(result, Ok("need_thread: need_thread ok".into()));
    assert_eq!(
        calls,
        vec![(
            "need_thread".to_string(),
            json!({ "project": "app", "handle": "3f9a1c2e" })
        )]
    );
    let (result, calls) = run(&request(Action::ReadTaskThread));
    assert_eq!(result, Ok("task_thread: task_thread ok".into()));
    assert_eq!(
        calls,
        vec![(
            "task_thread".to_string(),
            json!({ "project": "app", "handle": "a1b2c3d4" })
        )]
    );
}

#[test]
fn thread_reads_forward_an_explicit_page_and_cursor() {
    let mut paged = request(Action::ReadNeedThread);
    paged.limit = Some(5);
    paged.before = Some("f0e1d2c3".into());
    let (_, calls) = run(&paged);
    assert_eq!(
        calls,
        vec![(
            "need_thread".to_string(),
            json!({
                "project": "app",
                "handle": "3f9a1c2e",
                "limit": 5,
                "before": "f0e1d2c3"
            })
        )]
    );
    let mut task_page = request(Action::ReadTaskThread);
    task_page.limit = Some(100);
    let (_, calls) = run(&task_page);
    assert_eq!(
        calls,
        vec![(
            "task_thread".to_string(),
            json!({ "project": "app", "handle": "a1b2c3d4", "limit": 100 })
        )]
    );
}

#[test]
fn read_flow_names_the_flow_and_asks_runs_only_for_a_need() {
    let mut flow = request(Action::ReadFlow);
    flow.flow = Some("build".into());
    let (result, calls) = run(&flow);
    assert_eq!(result, Ok("flow_show: flow_show ok".into()));
    assert_eq!(
        calls,
        vec![(
            "flow_show".to_string(),
            json!({ "project": "app", "kind": "build", "need": "3f9a1c2e" })
        )]
    );
    // No need named: the flow's source alone is asked for.
    flow.need = None;
    let (_, calls) = run(&flow);
    assert_eq!(
        calls,
        vec![(
            "flow_show".to_string(),
            json!({ "project": "app", "kind": "build" })
        )]
    );
    // The flow is required: nothing is called without one.
    flow.flow = None;
    let (result, calls) = run(&flow);
    assert_eq!(result, Err("the action names no flow".into()));
    assert!(calls.is_empty());
}

#[test]
fn read_config_and_read_pool_stay_unresolved_with_one_call_each() {
    let (result, calls) = run(&request(Action::ReadConfig));
    assert_eq!(result, Ok("config_show: config_show ok".into()));
    assert_eq!(
        calls,
        vec![("config_show".to_string(), json!({ "project": "app" }))]
    );
    let mut pool = request(Action::ReadPool);
    pool.pool = Some("build".into());
    let (result, calls) = run(&pool);
    assert_eq!(result, Ok("pool_show: pool_show ok".into()));
    assert_eq!(
        calls,
        vec![(
            "pool_show".to_string(),
            json!({ "project": "app", "name": "build" })
        )]
    );
    // Neither read ever asks for the live free-slot probe, and a pool
    // read without a pool calls nothing.
    let (result, calls) = run(&request(Action::ReadPool));
    assert_eq!(result, Err("the action names no pool".into()));
    assert!(calls.is_empty());
}

#[test]
fn a_server_refusal_on_a_read_is_the_failure_text() {
    let mut actuator = McpActuator::new(Recorder {
        fail_with: Some("not a need: 3f9a1c2e".into()),
        ..Recorder::default()
    });
    let error = actuator
        .apply(&request(Action::ReadNeedThread))
        .unwrap_err();
    assert_eq!(error, "need_thread: not a need: 3f9a1c2e");
}

#[test]
fn a_letter_to_the_operator_is_a_mail_send() {
    let mut escalation = request(Action::Escalate);
    escalation.note = Some("needs a decision".into());
    let (_, calls) = run(&escalation);
    assert_eq!(calls[0].0, "send");
    assert_eq!(calls[0].1["kind"], "mail");
    assert_eq!(calls[0].1["to"], "operator");
    assert_eq!(calls[0].1["subject"], "mind: escalate (need 3f9a1c2e)");
    assert_eq!(calls[0].1["body"], "needs a decision");
}

#[test]
fn the_server_refusing_a_call_is_the_failure_text() {
    let mut actuator = McpActuator::new(Recorder {
        fail_with: Some("the key is not an operator".into()),
        ..Recorder::default()
    });
    let error = actuator.apply(&request(Action::Unlock)).unwrap_err();
    assert_eq!(error, "pump_action: the key is not an operator");
}

#[test]
fn an_action_the_server_cannot_do_yet_is_an_error_that_calls_nothing() {
    let (result, calls) = run(&request(Action::FileFixNeed));
    assert!(result.unwrap_err().contains("not wired to the server yet"));
    assert!(calls.is_empty());
    let mut no_project = request(Action::PausePump);
    no_project.project = None;
    let (result, calls) = run(&no_project);
    assert_eq!(result, Err("the action names no project".into()));
    assert!(calls.is_empty());
}

#[test]
fn a_long_answer_is_cut_in_the_receipt() {
    struct Long;
    impl ToolCaller for Long {
        fn call(&self, _tool: &str, _arguments: &Value) -> Result<String, String> {
            Ok("x".repeat(5000))
        }
    }
    let receipt = McpActuator::new(Long)
        .apply(&request(Action::ResumePump))
        .unwrap();
    assert!(
        receipt.ends_with("[cut]"),
        "{}",
        &receipt[receipt.len() - 20..]
    );
    assert!(receipt.len() < 1700);
}

#[test]
fn the_actuator_feeds_its_answers_back() {
    assert!(McpActuator::new(Recorder::default()).feeds_back());
}

#[test]
fn the_key_file_is_read_trimmed_and_must_not_be_empty() {
    let dir = tempfile::tempdir().unwrap();
    let missing = HttpCaller::from_key_file("http://x", &dir.path().join("none")).err();
    assert!(missing.unwrap().contains("cannot read the mind's key"));
    let key = dir.path().join("mcp.key");
    std::fs::write(&key, "  \n").unwrap();
    assert!(
        HttpCaller::from_key_file("http://x", &key)
            .err()
            .unwrap()
            .contains("is empty")
    );
    std::fs::write(&key, "secret-key\n").unwrap();
    assert!(HttpCaller::from_key_file("http://x", &key).is_ok());
}
