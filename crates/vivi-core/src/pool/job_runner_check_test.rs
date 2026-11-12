//! Real bash runner checks, using the read-only runner's throwaway Git rig.
use super::*;
use crate::pool::job::sh_quote;

fn run(rig: &Rig, harness: &str, command: Option<&str>, timeout: u64, budget: u64) -> JobResult {
    let mut params = format!(
        "READ_ONLY=''\nBRANCH='pool/check'\nBUDGET='{budget}'\nCHECK_TIMEOUT_S='{timeout}'\n"
    );
    if let Some(command) = command {
        params.push_str("CHECK_COMMAND=");
        params.push_str(&sh_quote(command));
        params.push('\n');
    }
    rig.run_in_context_logged(harness, "build", &[], &rig.f.repo, None, &params)
        .0
}

#[test]
fn check_runs_after_agent_cleanup_on_committed_candidate_before_publish_with_slot_environment() {
    let rig = Rig::new();
    let cache = tempfile::tempdir().unwrap();
    let target = cache.path().join("slot-target");
    std::fs::write(
        rig.work.path().join("build-cache.sh"),
        crate::pool::ssh_script::WORKER_CARGO_SCRIPT,
    )
    .unwrap();
    std::fs::write(
        rig.work.path().join("build-cache.env"),
        format!(
            "CARGO_TARGET_DIR={}\nVIVI_CARGO_SCCACHE=0\n",
            sh_quote(target.to_str().unwrap())
        ),
    )
    .unwrap();
    let seen = rig.work.path().join("checked-sha");
    let done = rig.work.path().join("agent-done");
    let child = rig.work.path().join("child-pid");
    let harness = format!(
        "echo changed >> a.txt; (sleep 40) & echo $! > {}; printf '%s|%s|%s' \"$CARGO_TARGET_DIR\" \"$TMPDIR\" \"$PATH\" > {}; echo done",
        sh_quote(child.to_str().unwrap()),
        sh_quote(done.to_str().unwrap())
    );
    let check = format!(
        "set -e; test -f {}; ! kill -0 $(cat {}) 2>/dev/null; test \"$(cat {})\" = \"$CARGO_TARGET_DIR|$TMPDIR|$PATH\"; git diff --exit-code HEAD; git show HEAD:a.txt | grep changed; ! git --git-dir={} show-ref --verify --quiet refs/heads/pool/check; git rev-parse HEAD > {}",
        sh_quote(done.to_str().unwrap()),
        sh_quote(child.to_str().unwrap()),
        sh_quote(done.to_str().unwrap()),
        sh_quote(rig.f.repo.to_str().unwrap()),
        sh_quote(seen.to_str().unwrap())
    );
    let result = run(&rig, &harness, Some(&check), 20, 10);
    assert_eq!(result.get("check_rc"), "0", "{:?}", result.fields);
    assert_eq!(result.push(), "pushed");
    assert_eq!(
        std::fs::read_to_string(seen).unwrap().trim(),
        result.get("sha")
    );
    assert_eq!(
        git(&rig.f.repo, &["rev-parse", "pool/check"]),
        result.get("sha")
    );
}

#[test]
fn failed_check_still_publishes_and_collects_bounded_tail_and_red_task_result() {
    let rig = Rig::new();
    let command = "mkdir -p build; dd if=/dev/zero of=build/large-artifact bs=1024 count=128 2>/dev/null; for ((i=0;i<300;i++)); do echo line-$i-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; done; echo 'check_rc=0'; echo last-line >&2; exit 7";
    let result = run(&rig, "echo changed >> a.txt", Some(command), 10, 10);
    assert_eq!(result.push(), "pushed");
    assert_eq!(result.get("agent_rc"), "0");
    assert_eq!(
        std::fs::metadata(rig.repo().join("build/large-artifact"))
            .unwrap()
            .len(),
        131_072
    );
    assert_eq!(result.get("check_rc"), "7");
    assert!(result.get("check_s").parse::<u64>().is_ok());
    let tail = result.get("check_tail");
    assert!(tail.len() <= 4096);
    assert!(!tail.contains("line-0-"));
    assert!(tail.ends_with("check_rc=0\\nlast-line"), "{tail}");
    assert!(crate::pool::facts::unescape(tail).lines().count() <= 40);
    assert_eq!(
        git(&rig.f.repo, &["rev-parse", "pool/check"]),
        result.get("sha")
    );
    let mut collected =
        crate::pool::collect::parse_collected("@@vivi-pool result\njob=j1\n").unwrap();
    collected.result = result;
    let dispatch = crate::pool::record::OpenDispatch { task: "t1".into(), record: serde_json::from_value(serde_json::json!({
        "pool":"p", "slot":"s", "machine":"m", "job":"j1", "model":"m", "from":"main", "branch":"pool/check", "start":"a", "budget":10, "at":0
    })).unwrap() };
    let outcome = crate::pool::collect::Outcome::Finished(collected);
    let note: serde_json::Value =
        serde_json::from_str(&crate::pool::record::collected_note(&dispatch, &outcome)).unwrap();
    assert_eq!(note["check_rc"], "7");
    assert!(note.get("check_s").is_some());
    assert!(note.get("check_tail").is_some());
    let receipt = crate::pool::record::collected_text(&dispatch, &outcome);
    assert!(receipt.contains("runner check: failed exit 7"));
    let outcome = crate::pump::jobs::JobOutcome {
        state: "finished".into(),
        detail: String::new(),
        receipt,
        at: 1,
    };
    assert_eq!(outcome.agent_success(), Some(false));
}

#[test]
fn unset_no_change_and_read_only_jobs_skip_check() {
    for (harness, check) in [("echo changed >> a.txt", None), ("true", Some("exit 9"))] {
        let rig = Rig::new();
        let result = run(&rig, harness, check, 1, 5);
        assert_eq!(result.get("check_rc"), "");
        assert_eq!(result.get("check_s"), "");
        assert_eq!(result.get("check_tail"), "");
    }
    let rig = Rig::new();
    let (result, _) = rig.run_in_context_logged(
        "echo changed >> a.txt",
        "build",
        &[],
        &rig.f.repo,
        None,
        "CHECK_COMMAND='exit 9'\n",
    );
    assert_eq!(result.get("check_rc"), "");
}

#[test]
fn check_timeout_is_independent_and_work_is_published() {
    for (command, seconds, expected) in [("sleep 2", 5, "0"), ("sleep 30", 1, "124")] {
        let rig = Rig::new();
        let start = std::time::Instant::now();
        let result = run(&rig, "echo changed >> a.txt", Some(command), seconds, 1);
        assert_eq!(result.get("agent_rc"), "0");
        assert_eq!(result.get("check_rc"), expected, "{:?}", result.fields);
        assert_eq!(result.push(), "pushed");
        assert!(start.elapsed().as_secs() < 15);
    }
}

#[test]
fn repository_and_delivered_params_edits_cannot_replace_captured_config_command() {
    let rig = Rig::new();
    let params = rig.work.path().join("job/params.env");
    let harness = format!(
        "echo changed >> a.txt; echo 'exit 0' > check.sh; echo \"CHECK_COMMAND='exit 0'\" > {}; echo 'exit 0' > {}; export CHECK_COMMAND='exit 0'",
        sh_quote(params.to_str().unwrap()),
        sh_quote(params.with_file_name("run.sh").to_str().unwrap())
    );
    let result = run(&rig, &harness, Some("echo fixed-from-config; exit 6"), 5, 5);
    assert_eq!(result.get("check_rc"), "6");
    assert_eq!(result.get("check_tail"), "fixed-from-config");
    assert_eq!(result.push(), "pushed");
}

#[test]
fn unlimited_check_output_is_capped_and_fails_without_losing_work() {
    let rig = Rig::new();
    let result = run(
        &rig,
        "echo changed >> a.txt",
        Some("yes noisy-output"),
        1,
        5,
    );
    assert_ne!(result.get("check_rc"), "0");
    assert!(!result.get("check_rc").is_empty());
    assert!(result.get("check_tail").len() <= 4096);
    assert_eq!(result.push(), "pushed");
    assert!(!rig.work.path().join("job/check.out").exists());
}
