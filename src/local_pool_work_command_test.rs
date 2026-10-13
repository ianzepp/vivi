//! The extra refs of a dispatch on a worktree pool: resolved in the source
//! repository before any slot is claimed. A throwaway Git repository; nothing
//! is launched.

use super::*;
use vivi::pool::config::PoolCommitter;

/// A repository with `main` and `review/aaa`; returns the directory and the
/// repository path.
fn repo_with_branch() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", "T")
            .env("GIT_AUTHOR_EMAIL", "t@localhost")
            .env("GIT_COMMITTER_NAME", "T")
            .env("GIT_COMMITTER_EMAIL", "t@localhost")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "first"]);
    git(&["branch", "review/aaa"]);
    (dir, repo)
}

fn plan_with(names: &[&str]) -> job::JobPlan {
    job::JobPlan {
        job: "0123456789ab".into(),
        remote: String::new(),
        checkout: None,
        task: "abcd1234".into(),
        subject: "Subject".into(),
        brief: "body".into(),
        rules: String::new(),
        refs: job::Refs {
            from: "main".into(),
            branch: "factory/x".into(),
        },
        with: names
            .iter()
            .map(|n| job::WithRef {
                name: (*n).to_string(),
                commit: String::new(),
            })
            .collect(),
        model: "m/x".into(),
        budget: 600,
        depth: 1,
        harness_cmd: "fake".into(),
        env_names: Vec::new(),
        committer: PoolCommitter {
            name: "bot".into(),
            email: "bot@example.com".into(),
            unknown: std::collections::BTreeMap::new(),
        },
        read_only: false,
        facts: Vec::new(),
        restore: Vec::new(),
    }
}

fn commit_of(repo: &std::path::Path, rev: &str) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", rev])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn extra_refs_resolve_in_the_source_repository_before_any_slot_is_claimed() {
    let (_dir, repo) = repo_with_branch();
    let main = commit_of(&repo, "main");
    let mut plan = plan_with(&["review/aaa", &main[..8]]);
    resolve_with_local(&repo, &mut plan, &Progress::silent()).unwrap();
    assert_eq!(
        plan.with,
        vec![
            job::WithRef {
                name: "review/aaa".into(),
                commit: commit_of(&repo, "review/aaa"),
            },
            job::WithRef {
                name: main.clone(),
                commit: main,
            },
        ],
        "a branch resolves to its tip; an abbreviated hash becomes the full commit"
    );
    assert!(
        plan.with
            .iter()
            .all(|w| w.commit.len() == 40 && w.commit.chars().all(|c| c.is_ascii_hexdigit()))
    );
}

#[test]
fn an_unknown_ref_is_an_error_naming_the_ref_and_the_repository() {
    let (_dir, repo) = repo_with_branch();
    let mut plan = plan_with(&["nope/ref"]);
    let err = resolve_with_local(&repo, &mut plan, &Progress::silent())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("nope/ref") && err.contains(repo.to_str().unwrap()),
        "{err}"
    );
}

/// A finished dispatch record, the way `dispatch` builds one: the `with`
/// entries already carry their resolved commit, cut to 12 hex digits.
fn record(with: &[String]) -> vivi::pool::record::DispatchRecord {
    vivi::pool::record::DispatchRecord {
        pool: "p".into(),
        slot: "p-slot-1".into(),
        machine: "m1".into(),
        job: "0123456789ab".into(),
        model: "m/x".into(),
        from: "main".into(),
        branch: "factory/a".into(),
        start: "a".repeat(40),
        with: with.to_vec(),
        budget: 600,
        at: 1000,
        read_only: false,
        hardware: String::new(),
        abandoned: 0,
        abandoned_cost_micros: 0,
    }
}

#[test]
fn the_human_dispatch_line_carries_the_extra_refs_when_there_are_some() {
    let made = record(&[
        "review/aaa@aaaa1111aaaa".into(),
        "bb034af1@bbbb2222bbbb".into(),
    ]);
    let text = dispatched_text("abcd1234", &made, "");
    assert!(
        text.contains("  with review/aaa@aaaa1111aaaa bb034af1@bbbb2222bbbb"),
        "{text}"
    );
}

#[test]
fn the_human_dispatch_line_has_no_with_segment_when_the_job_has_none() {
    let text = dispatched_text("abcd1234", &record(&[]), "");
    assert!(!text.contains("with"), "{text}");
}
