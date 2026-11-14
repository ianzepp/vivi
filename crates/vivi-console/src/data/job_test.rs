use super::*;
use crate::pages::test_support::{Project, project_with_task};

fn detail(project: &Project, job: &str) -> JobDetail {
    match load(&project.root, job).unwrap() {
        RunsState::Ready(Some(detail)) => detail,
        other => panic!("expected the job, got {other:?}"),
    }
}

#[test]
fn an_unknown_job_reads_as_none() {
    let project = project_with_task("brief");
    project.dispatch("job-1", 1_000);
    assert_eq!(load(&project.root, "nope").unwrap(), RunsState::Ready(None));
}

#[test]
fn a_job_with_no_collect_yet_has_a_run_a_brief_and_one_event() {
    let project = project_with_task("Fix the edge case.");
    project.dispatch("job-1", 1_000);
    let job = detail(&project, "job-1");
    assert_eq!(job.run.pool, "main");
    assert_eq!(job.run.slot, "slot-1");
    assert_eq!(job.run.need.as_deref(), Some("a1f3c2d0ff"));
    assert_eq!(job.run.budget_s, Some(3300));
    assert_eq!(job.run.branch, "factory/x");
    assert_eq!(job.run.start_sha, "0123456789abcdef0123");
    assert_eq!(job.run.collected_at, None);
    assert!(job.brief.unwrap().text.contains("Fix the edge case."));
    let kinds: Vec<&str> = job.events.iter().map(|e| e.event_type.as_str()).collect();
    assert_eq!(kinds, ["delivered", "pool_dispatched"]);
    assert!(job.events[1].at > 0);
    assert_eq!(job.replies.len(), 1);
}

#[test]
fn a_collected_job_carries_its_events_and_replies_in_order() {
    let project = project_with_task("brief");
    project.dispatch("job-1", 1_000);
    project.record(
        "pool_collected",
        r#"{"job":"job-1","slot":"slot-1","outcome":"finished","agent_rc":"0","agent_s":"90","check_rc":"1","check_s":"30","job_s":"130","check_tail":"FAILED","agent_cpu_s":"77"}"#,
        "pool result job-1: finished\n\n--- receipt\nthe agent said hi",
    );
    project.record(
        "pool_landed",
        r#"{"job":"job-1","landing":"red","command":"cargo test","tail":"boom"}"#,
        "pool landed job-1: red",
    );
    let job = detail(&project, "job-1");
    assert_eq!(job.run.outcome.as_deref(), Some("finished"));
    assert_eq!(job.run.check_rc, Some(1));
    assert_eq!(job.run.agent_s, Some(90));
    assert_eq!(job.run.agent_cpu_s, Some(77));
    assert_eq!(job.run.check_cpu_s, None);
    let kinds: Vec<&str> = job.events.iter().map(|e| e.event_type.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "delivered",
            "pool_dispatched",
            "pool_collected",
            "pool_landed"
        ]
    );
    let texts: Vec<&str> = job.replies.iter().map(|r| r.text.as_str()).collect();
    assert!(texts[0].starts_with("pool dispatch job-1"));
    assert!(texts[1].starts_with("pool result job-1"));
    assert!(texts[2].starts_with("pool landed job-1"));
}
