//! `vivi slot list | show | history | rebuild` read the `slot_runs` index that
//! the pool events keep; no host is contacted.

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

fn record(job: &str, slot: &str, at: u64) -> DispatchRecord {
    DispatchRecord {
        target: None,
        repository: None,
        pool: "lanes".into(),
        need: Some("need-1".into()),
        kind: Some("hand".into()),
        slot: slot.into(),
        machine: "studio".into(),
        job: job.into(),
        model: "m/ok".into(),
        from: "main".into(),
        branch: "factory/x".into(),
        start: "a".repeat(40),
        with: Vec::new(),
        budget: 1800,
        check_allowance_s: 0,
        at,
        read_only: false,
        hardware: String::new(),
        abandoned: 0,
        abandoned_cost_micros: 0,
    }
}

fn finished() -> Outcome {
    Outcome::Finished(Collected {
        result: JobResult::parse("push=pushed\nsha=bbbb\nagent_rc=0\nagent_s=240\njob_s=290\n"),
        receipt: String::new(),
        stderr: String::new(),
        log: String::new(),
    })
}

/// A project with two declared slots, one task, a finished job in `slot-a` and
/// a running one in `slot-b`.
fn project() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    ok(root, &["mailspace", "init"]);
    for (role, kind) in [("mind", "mind"), ("hand", "hand")] {
        ok(root, &["role", "add", role, "--kind", kind]);
    }
    let config = root.join(".vivi/mailspace.toml");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str(
        "\n[[slot]]\nname = \"slot-a\"\nrepository = \"acme/app\"\nhost = \"studio\"\ndir = \"/work/a\"\n\
         \n[[slot]]\nname = \"slot-b\"\nrepository = \"acme/app\"\nhost = \"studio\"\ndir = \"/work/b\"\n\
         \n[pools.lanes]\nprovider = \"ssh\"\n",
    );
    std::fs::write(&config, text).unwrap();
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
    let first = OpenDispatch {
        task: handle.clone(),
        record: record("job-1", "slot-a", 1_793_527_200),
    };
    space
        .record_pool_note(
            &handle,
            "pool_dispatched",
            &dispatch_note(&first.record),
            "go",
        )
        .unwrap();
    space
        .record_pool_note(
            &handle,
            "pool_collected",
            &collected_note(&first, &finished()),
            "done",
        )
        .unwrap();
    let now = vivi_core::pool::work::epoch_secs();
    let second = record("job-2", "slot-b", now - 90);
    space
        .record_pool_note(&handle, "pool_dispatched", &dispatch_note(&second), "go")
        .unwrap();
    (tmp, handle)
}

#[test]
fn slot_list_shows_busy_and_free_slots_with_the_running_job() {
    let (tmp, handle) = project();

    let out = ok(tmp.path(), &["slot", "list"]);

    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(lines[0].starts_with("slot-a  free"), "{out}");
    assert!(lines[1].starts_with("slot-b  busy"), "{out}");
    assert!(lines[1].contains(&format!("task {handle}")), "{out}");
    assert!(lines[1].contains("need need-1"), "{out}");
    assert!(lines[1].contains("model m/ok"), "{out}");
    assert!(lines[1].contains("s of 1800s"), "{out}");
}

#[test]
fn slot_list_json_carries_state_and_the_open_run() {
    let (tmp, _) = project();

    let out = ok(tmp.path(), &["slot", "list", "--json"]);

    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(rows[0]["state"], "free");
    assert_eq!(rows[1]["state"], "busy");
    assert_eq!(rows[1]["run"]["job"], "job-2");
}

#[test]
fn slot_show_names_the_job_or_the_last_one() {
    let (tmp, _) = project();

    let busy = ok(tmp.path(), &["slot", "show", "slot-b"]);
    assert!(busy.contains("slot slot-b  busy"), "{busy}");
    assert!(busy.contains("job job-2"), "{busy}");

    let free = ok(tmp.path(), &["slot", "show", "slot-a"]);
    assert!(free.contains("slot slot-a  free"), "{free}");
    assert!(free.contains("last job job-1"), "{free}");
    assert!(free.contains("finished"), "{free}");

    assert!(!vivi(tmp.path(), &["slot", "show", "nope"]).status.success());
}

#[test]
fn slot_history_lists_a_slots_jobs_newest_first() {
    let (tmp, _) = project();

    let out = ok(
        tmp.path(),
        &["slot", "history", "slot-a", "--pool", "lanes"],
    );

    assert_eq!(out.lines().count(), 1, "{out}");
    assert!(out.contains("job-1"), "{out}");
    assert!(out.contains("finished push pushed  agent 240s"), "{out}");
    let json = ok(
        tmp.path(),
        &["slot", "history", "slot-b", "--limit", "5", "--json"],
    );
    let runs: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(runs.as_array().unwrap().len(), 1, "{json}");
    assert_eq!(runs[0]["job"], "job-2");
}

#[test]
fn slot_rebuild_reports_the_jobs_it_rebuilt_and_changes_nothing() {
    let (tmp, _) = project();
    let before = ok(tmp.path(), &["slot", "list", "--json"]);

    let out = ok(tmp.path(), &["slot", "rebuild"]);

    assert!(out.contains("2 job(s)"), "{out}");
    assert_eq!(ok(tmp.path(), &["slot", "list", "--json"]), before);
}
