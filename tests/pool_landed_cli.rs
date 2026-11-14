//! `vivi pool landed` records a `pool_landed` event on a dispatched task and
//! the `slot_runs` index shows it; no host is contacted.

use std::path::Path;
use std::process::{Command, Output};

use vivi_core::mailspace::Mailspace;
use vivi_core::pool::collect::{Collected, JobResult, Outcome};
use vivi_core::pool::record::{DispatchRecord, OpenDispatch, collected_note, dispatch_note};

fn vivi(project: &Path, args: &[&str]) -> Output {
    let mut full: Vec<&str> = args.to_vec();
    full.extend(["--project", project.to_str().unwrap()]);
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(full)
        .output()
        .unwrap()
}

fn ok(project: &Path, args: &[&str]) -> String {
    let output = vivi(project, args);
    assert!(
        output.status.success(),
        "{args:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn refused(project: &Path, args: &[&str]) -> String {
    let output = vivi(project, args);
    assert!(!output.status.success(), "{args:?} should be refused");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn dispatch(job: &str) -> OpenDispatch {
    OpenDispatch {
        task: String::new(),
        record: DispatchRecord {
            target: None,
            repository: None,
            pool: "lanes".into(),
            need: Some("need-1".into()),
            kind: Some("hand".into()),
            slot: "slot-a".into(),
            machine: "studio".into(),
            job: job.into(),
            model: "m/ok".into(),
            from: "main".into(),
            branch: "factory/x".into(),
            start: "a".repeat(40),
            with: Vec::new(),
            budget: 1800,
            check_allowance_s: 0,
            at: 1_793_527_200,
            read_only: false,
            hardware: String::new(),
            abandoned: 0,
            abandoned_cost_micros: 0,
        },
    }
}

/// A project with one task, dispatched and collected as `job-1`.
fn project() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    ok(root, &["mailspace", "init"]);
    for (role, kind) in [("mind", "mind"), ("hand", "hand")] {
        ok(root, &["role", "add", role, "--kind", kind]);
    }
    let out = ok(
        root,
        &[
            "task",
            "send",
            "--from",
            "mind",
            "--to",
            "hand",
            "--subject",
            "Fix",
            "--body",
            "body",
        ],
    );
    let handle = out
        .lines()
        .find_map(|line| line.strip_prefix("created hand "))
        .unwrap()
        .trim()
        .to_string();
    let space = Mailspace::discover(Some(root)).unwrap();
    let first = dispatch("job-1");
    space
        .record_pool_note(
            &handle,
            "pool_dispatched",
            &dispatch_note(&first.record),
            "go",
        )
        .unwrap();
    let outcome = Outcome::Finished(Collected {
        result: JobResult::parse("push=pushed\nsha=bbbb\nagent_rc=0\nagent_s=240\njob_s=290\n"),
        receipt: String::new(),
        stderr: String::new(),
        log: String::new(),
    });
    space
        .record_pool_note(
            &handle,
            "pool_collected",
            &collected_note(&first, &outcome),
            "done",
        )
        .unwrap();
    (tmp, handle)
}

fn landed_notes(root: &Path) -> Vec<serde_json::Value> {
    let space = Mailspace::discover(Some(root)).unwrap();
    let storage = space.storage().unwrap();
    let events = storage
        .list_mailspace_events_of_types(&["pool_landed"])
        .unwrap();
    events
        .iter()
        .map(|event| serde_json::from_str(event.note.as_deref().unwrap()).unwrap())
        .collect()
}

fn run_json(root: &Path) -> serde_json::Value {
    let out = ok(
        root,
        &["slot", "history", "slot-a", "--pool", "lanes", "--json"],
    );
    serde_json::from_str::<serde_json::Value>(&out).unwrap()[0].clone()
}

#[test]
fn landed_records_the_exact_note_and_fills_the_index() {
    let (tmp, handle) = project();
    let root = tmp.path();

    let out = ok(
        root,
        &[
            "pool",
            "landed",
            &handle,
            "--job",
            "job-1",
            "--outcome",
            "landed",
            "--base",
            "aaa",
            "--tip",
            "bbb",
            "--secs",
            "41",
        ],
    );

    assert!(out.contains("job-1 landed"), "{out}");
    assert_eq!(
        landed_notes(root),
        vec![serde_json::json!({
            "job": "job-1", "landing": "landed", "base": "aaa", "tip": "bbb", "secs": 41,
        })]
    );
    let run = run_json(root);
    assert_eq!(run["landing"], "landed");
    assert!(run["landing_at"].as_i64().unwrap() > 0);
    assert!(run["landing_reason"].is_null());
}

#[test]
fn red_keeps_the_command_and_the_capped_tail_of_the_file() {
    let (tmp, handle) = project();
    let root = tmp.path();
    let tail = tmp.path().join("red.log");
    std::fs::write(&tail, format!("HEAD{}END", "x".repeat(10_000))).unwrap();

    ok(
        root,
        &[
            "pool",
            "landed",
            &handle,
            "--job",
            "job-1",
            "--outcome",
            "red",
            "--command",
            "cargo test",
            "--tail-file",
            tail.to_str().unwrap(),
        ],
    );

    let notes = landed_notes(root);
    assert_eq!(notes[0]["command"], "cargo test");
    let kept = notes[0]["tail"].as_str().unwrap();
    assert_eq!(kept.len(), 4096);
    assert!(kept.ends_with("END") && !kept.contains("HEAD"));
    let run = run_json(root);
    assert_eq!(run["landing"], "red");
    assert_eq!(run["landing_reason"], "cargo test");
}

#[test]
fn conflict_keeps_the_files_and_raced_has_no_reason() {
    let (tmp, handle) = project();
    let root = tmp.path();

    ok(
        root,
        &[
            "pool",
            "landed",
            &handle,
            "--job",
            "job-1",
            "--outcome",
            "conflict",
            "--file",
            "src/a.rs",
            "--file",
            "src/b.rs",
        ],
    );
    assert_eq!(
        landed_notes(root)[0]["files"],
        serde_json::json!(["src/a.rs", "src/b.rs"])
    );
    let run = run_json(root);
    assert_eq!(
        (run["landing"].as_str(), run["landing_reason"].as_str()),
        (Some("conflict"), Some("src/a.rs\nsrc/b.rs"))
    );

    ok(
        root,
        &[
            "pool",
            "landed",
            &handle,
            "--job",
            "job-1",
            "--outcome",
            "raced",
        ],
    );
    let run = run_json(root);
    assert_eq!(run["landing"], "raced");
    assert!(run["landing_reason"].is_null());
}

#[test]
fn an_unknown_job_or_outcome_is_refused_and_records_nothing() {
    let (tmp, handle) = project();
    let root = tmp.path();

    let err = refused(
        root,
        &[
            "pool",
            "landed",
            &handle,
            "--job",
            "job-9",
            "--outcome",
            "landed",
        ],
    );
    assert!(err.contains("no dispatch of job job-9"), "{err}");
    let err = refused(
        root,
        &[
            "pool",
            "landed",
            &handle,
            "--job",
            "job-1",
            "--outcome",
            "merged",
        ],
    );
    assert!(err.contains("unknown landing outcome"), "{err}");

    assert!(landed_notes(root).is_empty());
    assert!(run_json(root)["landing"].is_null());
}

#[test]
fn a_landing_does_not_change_which_dispatches_are_open() {
    let (tmp, handle) = project();
    let root = tmp.path();
    let space = Mailspace::discover(Some(root)).unwrap();
    let second = dispatch("job-2");
    space
        .record_pool_note(
            &handle,
            "pool_dispatched",
            &dispatch_note(&second.record),
            "go",
        )
        .unwrap();

    ok(
        root,
        &[
            "pool",
            "landed",
            &handle,
            "--job",
            "job-1",
            "--outcome",
            "landed",
        ],
    );

    let open = vivi_core::pool::record::fold_open(&space.pool_events().unwrap());
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].record.job, "job-2");
}
