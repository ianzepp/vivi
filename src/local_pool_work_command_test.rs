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
        target: None,
        extras: Vec::new(),
        prepared_cache_key: None,
        prepared_local_source: None,
        ramdisk_size_gib: None,
        ramdisk_env: Vec::new(),
        job: "0123456789ab".into(),
        remote: String::new(),
        checkout: None,
        task: "abcd1234".into(),
        need: None,
        kind: None,
        subject: "Subject".into(),
        brief: "body".into(),
        rules: String::new(),
        refs: job::Refs {
            from: "main".into(),
            branch: "factory/x".into(),
        },
        publish_ref: None,
        publish_expect: None,
        publish_contains: None,
        delete_ref: None,
        delete_expect: None,
        publish_remote: None,
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
        bridge: None,
    }
}

#[test]
fn builtin_auditor_marker_replaces_the_recipient_charter_for_one_task() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Mailspace::init(Some(tmp.path())).unwrap();
    space.add_identity("hand").unwrap();
    space
        .set_charter("hand", "Hand-only standing instructions")
        .unwrap();
    let task = PoolTask {
        handle: String::from("abcd1234"),
        subject: String::from("audit"),
        body: vivi::letter::render(
            &vivi::letter::work::TaskFront {
                job: Some(vivi::letter::work::JobFront {
                    id: String::from("audit-1"),
                    kind: String::from("auditor:range"),
                    builtin_role: Some(String::from("auditor")),
                }),
                ..Default::default()
            },
            "Audit this range.",
        )
        .unwrap(),
        addressee: Some(String::from("hand")),
    };

    let role = role_facts(&space, &task, None).unwrap();

    assert_eq!(role.kind.as_deref(), Some("auditor"));
    assert!(role.read_only.unwrap_or(false));
    assert!(role.charter.contains("# Auditor"));
    assert!(!role.charter.contains("Hand-only standing instructions"));
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
fn remote_dispatch_does_not_load_a_pump_reset_base() {
    let tmp = tempfile::tempdir().unwrap();
    let space = Mailspace::init(Some(tmp.path())).unwrap();
    let config: PoolConfig = toml::from_str("provider = 'fly'\nslots = 1\n").unwrap();

    assert_eq!(dispatch_reset_base(&config, &space), None);
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

/// A bare `file://` origin with `main`, the need branch `needs/42` and the
/// archive branch `archive/prior-42`, each on its own commit; returns the
/// directory, the address, and the main, need and archive commits.
fn origin_with_branches() -> (tempfile::TempDir, String, String, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    let bare = dir.path().join("origin.git");
    std::fs::create_dir(&work).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&work)
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
    std::fs::write(work.join("a.txt"), "one\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "first"]);
    let main = commit_of(&work, "main");
    git(&["checkout", "-q", "-b", "needs/42"]);
    std::fs::write(work.join("a.txt"), "need\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "need"]);
    let need = commit_of(&work, "needs/42");
    git(&["checkout", "-q", "-b", "archive/prior-42", "main"]);
    std::fs::write(work.join("a.txt"), "prior\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "prior"]);
    let archive = commit_of(&work, "archive/prior-42");
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir.path())
        .args(["clone", "-q", "--bare"])
        .arg(&work)
        .arg(&bare)
        .output()
        .unwrap();
    assert!(out.status.success());
    (
        dir,
        format!("file://{}", bare.display()),
        main,
        need,
        archive,
    )
}

#[test]
fn git_start_resolves_slash_containing_from_and_extra_refs_against_the_origin() {
    let (_dir, url, _main, need, archive) = origin_with_branches();
    let mut plan = plan_with(&["archive/prior-42"]);
    plan.read_only = true;
    plan.target = Some(dispatch::target::JobTarget {
        id: "t".into(),
        source: url.clone(),
        publish_remote: None,
        cache_key: "k".into(),
        local_source: None,
        seed: Vec::new(),
        requires: Vec::new(),
        build_dir: None,
    });
    plan.refs.from = "needs/42".into();

    let (secret, start) = git_start(
        &url,
        &mut plan,
        &DispatchFlags::default(),
        &Progress::silent(),
    )
    .unwrap();

    assert_eq!(start, need, "the returned start is the need branch tip");
    assert_eq!(secret.expose(), "", "a plain Git remote carries no token");
    assert_eq!(
        plan.refs.from, need,
        "an explicitly targeted plan carries the resolved commit as from"
    );
    assert_eq!(
        plan.with,
        vec![job::WithRef {
            name: "archive/prior-42".into(),
            commit: archive.clone(),
        }],
        "the extra keeps its name and carries its branch tip"
    );
    assert_ne!(archive, need, "the branches hold different commits");
}

#[test]
fn git_start_refuses_an_unknown_from_before_any_submission() {
    let (_dir, url, ..) = origin_with_branches();
    let mut plan = plan_with(&[]);
    plan.read_only = true;
    plan.refs.from = "needs/nope".into();

    let err = git_start(
        &url,
        &mut plan,
        &DispatchFlags::default(),
        &Progress::silent(),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("no branch or tag \"needs/nope\""), "{err}");
}

#[test]
fn git_start_refuses_an_unknown_extra_ref_before_any_submission() {
    let (_dir, url, ..) = origin_with_branches();
    let mut plan = plan_with(&["archive/nope"]);
    plan.read_only = true;
    plan.refs.from = "needs/42".into();

    let err = git_start(
        &url,
        &mut plan,
        &DispatchFlags::default(),
        &Progress::silent(),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("no branch or tag \"archive/nope\""), "{err}");
}

/// A finished dispatch record, the way `dispatch` builds one: the `with`
/// entries already carry their resolved commit, cut to 12 hex digits.
fn record(with: &[String]) -> vivi::pool::record::DispatchRecord {
    vivi::pool::record::DispatchRecord {
        target: None,
        repository: None,
        pool: "p".into(),
        need: None,
        kind: None,
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
    let text = dispatched_text("abcd1234", &made, None, "");
    assert!(
        text.contains("  with review/aaa@aaaa1111aaaa bb034af1@bbbb2222bbbb"),
        "{text}"
    );
}

#[test]
fn the_human_dispatch_line_has_no_with_segment_when_the_job_has_none() {
    let text = dispatched_text("abcd1234", &record(&[]), None, "");
    assert!(!text.contains("with"), "{text}");
}

#[test]
fn the_named_role_wins_over_the_task_addressee() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Mailspace::init(Some(tmp.path())).unwrap();
    let kind = |kind: &str| vivi::mailspace::RoleUpdate {
        kind: Some(Some(kind.into())),
        ..Default::default()
    };
    space.add_role("pump", kind("pump")).unwrap();
    space.add_role("merge", kind("merge")).unwrap();
    let task = PoolTask {
        handle: String::from("abcd1234"),
        subject: String::from("step"),
        body: vivi::letter::render(&vivi::letter::work::TaskFront::step("n", 1, "merge"), "")
            .unwrap(),
        addressee: Some(String::from("pump")),
    };

    let named = role_facts(&space, &task, Some("merge")).unwrap();
    assert_eq!(named.name, "merge");
    assert_eq!(named.kind.as_deref(), Some("merge"));

    let default = role_facts(&space, &task, None).unwrap();
    assert_eq!(default.name, "pump");
    assert_eq!(default.kind.as_deref(), Some("pump"));

    assert!(role_facts(&space, &task, Some("nobody")).is_none());
}
