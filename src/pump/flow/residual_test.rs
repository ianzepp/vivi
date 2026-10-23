//! Tests for residual and requirement-gap need filing.

use std::sync::Arc;

use super::file;
use crate::pump::config::PumpConfig;
use crate::pump::envelope::review::{
    Auditor, AuditorAnswer, Finding, Requirement, RequirementStatus,
};
use crate::pump::host::{Ctx, FakeHost, test_ctx};

fn context(root: &std::path::Path) -> (Ctx, Arc<FakeHost>) {
    let (ctx, host) = test_ctx(root, PumpConfig::default());
    host.reply("need list", 0, "[]");
    host.reply("need send", 0, "created mind abc12345\nsent abc12345\n");
    (ctx, host)
}

fn requirement(id: &str, status: RequirementStatus) -> Requirement {
    Requirement {
        id: id.to_owned(),
        text: format!("requirement {id}"),
        status,
        evidence: Vec::new(),
    }
}

fn finding(id: &str) -> Finding {
    Finding {
        id: id.to_owned(),
        content: format!("finding {id}"),
        paths: vec![String::from("src/a.rs")],
        recheck_trigger: String::from("recheck a.rs"),
    }
}

fn front(answer: AuditorAnswer) -> Auditor {
    Auditor {
        answer,
        raise_rung: None,
        requirement: vec![
            requirement("R1", RequirementStatus::Satisfied),
            requirement("R2", RequirementStatus::Missing),
        ],
        finding: vec![finding("F1")],
    }
}

fn send_commands(host: &FakeHost) -> Vec<String> {
    host.commands()
        .into_iter()
        .filter(|c| c.contains("need send"))
        .collect()
}

#[test]
fn residual_files_finding_and_gap() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = context(dir.path());
    let handles = file(&ctx, "n1", 1, "audit-task", &front(AuditorAnswer::Residual)).unwrap();
    assert_eq!(handles.len(), 2);
    let sends = send_commands(&host);
    assert_eq!(sends.len(), 2);
    assert!(sends.iter().any(|c| c.contains("audit requirement gap n1")));
    assert!(sends.iter().any(|c| c.contains("audit finding n1")));
}

#[test]
fn clean_pass_without_gaps_files_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = context(dir.path());
    let mut clean = front(AuditorAnswer::CleanPass);
    clean.requirement = vec![requirement("R1", RequirementStatus::Satisfied)];
    assert!(
        file(&ctx, "n1", 1, "audit-task", &clean)
            .unwrap()
            .is_empty()
    );
    assert!(send_commands(&host).is_empty());
}

#[test]
fn block_range_files_no_findings() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = context(dir.path());
    let mut blocked = front(AuditorAnswer::BlockRange);
    blocked.requirement.clear();
    assert!(
        file(&ctx, "n1", 1, "audit-task", &blocked)
            .unwrap()
            .is_empty()
    );
    assert!(send_commands(&host).is_empty());
}

#[test]
fn gap_is_filed_whatever_the_answer() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = context(dir.path());
    let handles = file(
        &ctx,
        "n1",
        1,
        "audit-task",
        &front(AuditorAnswer::BlockShip),
    )
    .unwrap();
    assert_eq!(handles.len(), 1);
    assert_eq!(send_commands(&host).len(), 1);
}

#[test]
fn repeat_with_same_keys_files_nothing_new() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = context(dir.path());
    let auditor = front(AuditorAnswer::Residual);
    file(&ctx, "n1", 1, "audit-task", &auditor).unwrap();
    let rows: Vec<String> = send_commands(&host)
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let after = c.split("--subject ").nth(1).unwrap();
            let subject = after
                .split(" --body-file")
                .next()
                .unwrap()
                .trim_matches('\'');
            format!("{{\"handle\":\"h{i}\",\"date\":\"2026-10-23\",\"subject\":{subject:?}}}")
        })
        .collect();
    host.reply("need list", 0, &format!("[{}]", rows.join(",")));
    let before = send_commands(&host).len();
    let handles = file(&ctx, "n1", 1, "audit-task", &auditor).unwrap();
    assert_eq!(handles, vec![String::from("h0"), String::from("h1")]);
    assert_eq!(send_commands(&host).len(), before);
}
