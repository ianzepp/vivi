//! Turning permitted actions into server tool calls.

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
    }
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
fn read_status_reads_the_open_records_and_the_log_tail() {
    let (result, calls) = run(&request(Action::ReadStatus));
    let tools: Vec<&str> = calls.iter().map(|(tool, _)| tool.as_str()).collect();
    assert_eq!(tools, ["pump_list", "pump_log"]);
    assert_eq!(calls[0].1["project"], "app");
    assert_eq!(calls[1].1["file"], "log");
    let receipt = result.unwrap();
    assert!(receipt.contains("pump_list: pump_list ok"), "{receipt}");
    assert!(receipt.contains("pump_log: pump_log ok"), "{receipt}");
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
