//! Source contract: the pump records and dispatches work; pool jobs execute it.
//!
//! This scan fails if production `src/pump` grows an in-process merger, a
//! planner/check runner, or a git push of refs.

use std::fs;
use std::path::{Path, PathBuf};

fn production_pump_files() -> Vec<(PathBuf, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/pump");
    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

fn collect(dir: &Path, files: &mut Vec<(PathBuf, String)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, files);
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.ends_with(".rs") || name.ends_with("_test.rs") || name.ends_with("_tests.rs") {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        files.push((path, content));
    }
}

const FORBIDDEN_EXECUTION: &[(&str, &str)] = &[
    ("Merger::start", "in-process merge lane"),
    ("fn merge_work", "in-process unit merge"),
    ("fn batch_work", "in-process batch merge"),
    ("fn batch_round", "in-process bisect merge"),
    ("remote::offer", "REMOTE-MERGE protocol"),
];

#[test]
fn production_run_uses_pool_dispatch_and_restarts_through_receipt_collectors() {
    let run = include_str!("../src/pump/run.rs");
    let collect = include_str!("../src/pump/collect.rs");
    assert!(run.contains("jobs::dispatch_pending(ctx, st)"), "{run}");
    assert!(run.contains("collect::collect("), "{run}");
    assert!(
        collect.contains("jobs::apply_reconcile(ctx, st"),
        "{collect}"
    );
    assert!(
        collect.contains("planner_jobs::apply_completions(ctx, st)"),
        "{collect}"
    );
    assert!(
        !run.contains("Merger::start"),
        "legacy merger started in run.rs"
    );
}

#[test]
fn pump_does_not_execute_job_kinds_in_process() {
    for (path, content) in production_pump_files() {
        for (needle, why) in FORBIDDEN_EXECUTION {
            if *needle == "Host::spawn" {
                continue;
            }
            assert!(
                !content.contains(needle),
                "{} still executes a job kind in-process ({why}): {needle}",
                path.display()
            );
        }
    }
}

#[test]
fn pump_does_not_push_git_refs_itself() {
    for (path, content) in production_pump_files() {
        for (index, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") || trimmed.starts_with("*") || trimmed.starts_with("///") {
                continue;
            }
            if trimmed.contains("Do NOT")
                || trimmed.contains("do not push")
                || trimmed.contains("Do not push")
            {
                continue;
            }
            let push = trimmed.contains("git push")
                || trimmed.contains("push {} --delete")
                || trimmed.contains("push {} refs/heads")
                || trimmed.contains("\"push origin");
            assert!(
                !push,
                "{}:{} pump still pushes a git ref: {trimmed}",
                path.display(),
                index + 1
            );
        }
    }
}

#[test]
fn hand_planner_and_merge_producers_dispatch_through_pool_jobs() {
    let unit = include_str!("../src/pump/dispatch.rs");
    let planner = include_str!("../src/pump/planner_jobs.rs");
    let merge = include_str!("../src/pump/merge/pool_dispatch.rs");
    assert!(unit.contains("pool dispatch --pool"), "{unit}");
    assert!(unit.contains("--publish-ref"), "{unit}");
    assert!(!unit.contains("host.spawn"), "{unit}");
    assert!(planner.contains("jobs::enqueue(st, spec)"), "{planner}");
    assert!(!planner.contains(".spawn("), "{planner}");
    assert!(merge.contains("jobs::enqueue(st, spec)"), "{merge}");
    assert!(!merge.contains("Merger::start"), "{merge}");
    assert!(!merge.contains(".spawn("), "{merge}");
}
