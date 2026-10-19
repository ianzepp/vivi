use std::collections::BTreeMap;

use crate::pool::record::{DispatchRecord, OpenDispatch};

use super::super::events::{FinishedDispatch, ProjectEvents, TaskInfo};
use super::super::model::RowState;
use super::rows;

fn dispatch(pool: &str) -> DispatchRecord {
    DispatchRecord {
        target: None,
        repository: None,
        pool: pool.into(),
        need: Some("need1234".into()),
        kind: Some("fix".into()),
        slot: "slot-1".into(),
        machine: "alpha/lanes/slot-1".into(),
        job: "job-1".into(),
        model: "model-x".into(),
        from: "main".into(),
        branch: "agent/work".into(),
        start: "main".into(),
        with: Vec::new(),
        budget: 600,
        at: 1000,
        read_only: true,
        hardware: String::new(),
        abandoned: 0,
        abandoned_cost_micros: 0,
    }
}

#[test]
fn remote_open_dispatch_is_visible_and_has_no_tail() {
    let pools: BTreeMap<String, crate::pool::config::PoolConfig> =
        toml::from_str("[pods]\nprovider='ssh'\nslots=1\n").unwrap();
    let events = ProjectEvents {
        open: vec![OpenDispatch {
            task: "abcd1234".into(),
            record: dispatch("pods"),
        }],
        task_info: BTreeMap::from([(
            "abcd1234".into(),
            TaskInfo {
                subject: "Review".into(),
                role: "auditor".into(),
                need: None,
                kind: None,
            },
        )]),
        ..ProjectEvents::default()
    };
    let found = rows(
        "alpha",
        std::path::Path::new("."),
        "",
        &pools,
        &events,
        1060,
    );
    assert_eq!(found.len(), 1);
    assert!(matches!(found[0].state, RowState::Running));
    assert!(found[0].remote);
    assert_eq!(found[0].provider, "ssh");
    assert_eq!(found[0].running_secs, Some(60));
    assert_eq!(found[0].role, "auditor");
    assert_eq!(found[0].need.as_deref(), Some("need1234"));
    assert_eq!(found[0].work_kind.as_deref(), Some("fix"));
    assert!(found[0].tail.is_none());
}

#[test]
fn finished_local_dispatch_keeps_outcome_and_log_tail() {
    let temp = tempfile::tempdir().unwrap();
    let pools: BTreeMap<String, crate::pool::config::PoolConfig> = toml::from_str(&format!(
        "[lanes]\nprovider='local'\nslots=1\n[lanes.local]\nroot='{}'\n",
        temp.path().display()
    ))
    .unwrap();
    let slot = temp.path().join("alpha/lanes/slot-1");
    std::fs::create_dir_all(slot.join("job")).unwrap();
    std::fs::write(slot.join("job/agent.out"), "done\n").unwrap();
    let events = ProjectEvents {
        finished: vec![FinishedDispatch {
            task: "abcd1234".into(),
            subject: "Review".into(),
            role: "hand".into(),
            need: Some("need1234".into()),
            kind: Some("fix".into()),
            outcome: "pushed".into(),
            model: "model-x".into(),
            pool: "lanes".into(),
            slot: "slot-1".into(),
            machine: "alpha/lanes/slot-1".into(),
            started_at: 1000,
            budget_secs: 600,
            read_only: false,
            ended_at: "1970-01-01T00:17:40Z".into(),
        }],
        ..ProjectEvents::default()
    };
    let found = rows(
        "alpha",
        std::path::Path::new("."),
        "",
        &pools,
        &events,
        1060,
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].state, RowState::Finished("pushed".into()));
    assert_eq!(found[0].need.as_deref(), Some("need1234"));
    assert_eq!(found[0].work_kind.as_deref(), Some("fix"));
    assert!(
        found[0]
            .tail
            .as_ref()
            .is_some_and(|path| path.ends_with("job/agent.out"))
    );
}
