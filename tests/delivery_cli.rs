use std::path::Path;
use std::process::{Command, Output};

fn vivi(project: &Path, args: &[&str]) -> Output {
    let mut full = args.to_vec();
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

fn git(project: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(project)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn delivery_pushes_under_mind_prefix_and_merge_role_closes_with_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir(&project).unwrap();
    ok(&project, &["mailspace", "init"]);
    ok(&project, &["role", "add", "mind", "--kind", "mind"]);
    git(&project, &["init", "-b", "main"]);
    git(&project, &["config", "user.name", "Delivery Test"]);
    git(&project, &["config", "user.email", "delivery@example.test"]);
    std::fs::write(project.join("source.txt"), "base\n").unwrap();
    git(&project, &["add", "source.txt"]);
    git(&project, &["commit", "-m", "base"]);
    let base = git(&project, &["rev-parse", "HEAD"]);
    git(&project, &["checkout", "-b", "review"]);
    std::fs::write(project.join("source.txt"), "tip\n").unwrap();
    git(&project, &["commit", "-am", "tip"]);
    let tip = git(&project, &["rev-parse", "HEAD"]);
    let bare = tmp.path().join("origin.git");
    let init_bare = Command::new("git")
        .args(["init", "--bare", bare.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        init_bare.status.success(),
        "{}",
        String::from_utf8_lossy(&init_bare.stderr)
    );
    git(
        &project,
        &["remote", "add", "origin", bare.to_str().unwrap()],
    );

    let (need, _) = {
        let output = ok(
            &project,
            &[
                "need",
                "send",
                "--from",
                "mind",
                "--to",
                "merge",
                "--subject",
                "unit need",
                "--body",
                "done_when: unit is merged",
            ],
        );
        let parse = |prefix: &str| {
            output
                .lines()
                .find_map(|line| line.strip_prefix(prefix))
                .unwrap()
                .trim()
                .to_string()
        };
        (parse("created merge "), parse("sent "))
    };
    let delivered = ok(
        &project,
        &[
            "delivery",
            "send",
            "--from",
            "mind",
            "--to",
            "merge",
            "--branch",
            "review",
            "--tip",
            &tip,
            "--base",
            &base,
            "--gate",
            "cargo test -p vivi",
            "--gate-result",
            "passed",
            "--closes",
            &need,
            "--blocked",
            "ab12cd34=waiting on external review",
            "--filed",
            "ef56ab78",
        ],
    );
    let delivery = delivered
        .lines()
        .find_map(|line| line.strip_prefix("delivery "))
        .unwrap()
        .trim()
        .to_string();
    assert!(
        delivered.contains("branch mind/review pushed"),
        "{delivered}"
    );
    assert_eq!(
        git(
            &project,
            &[
                "--git-dir",
                bare.to_str().unwrap(),
                "rev-parse",
                "refs/heads/mind/review"
            ]
        ),
        tip
    );

    let shown = ok(&project, &["need", "show", &delivery, "--json"]);
    let thread: serde_json::Value = serde_json::from_str(&shown).unwrap();
    let root = thread
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["handle"] == delivery)
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(root["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["branch"], "mind/review");
    assert_eq!(body["tip"], tip);
    assert_eq!(body["base"], base);
    assert_eq!(body["push"]["status"], "pushed");
    assert_eq!(body["gate"]["result"], "passed");
    assert_eq!(body["closes"][0], need);
    assert_eq!(body["blocked"][0]["reason"], "waiting on external review");
    assert_eq!(body["filed"][0], "ef56ab78");

    let merge_commit = "1234567890abcdef";
    ok(
        &project,
        &[
            "delivery",
            "close",
            &delivery,
            "--for",
            "merge",
            "--merge-commit",
            merge_commit,
        ],
    );
    let close = ok(&project, &["need", "show", &delivery]);
    assert!(
        close.contains(&format!("Done on: main {merge_commit}")),
        "{close}"
    );

    let pending = ok(
        &project,
        &[
            "delivery",
            "send",
            "--from",
            "mind",
            "--to",
            "merge",
            "--branch",
            "missing-local-branch",
            "--tip",
            &tip,
            "--base",
            &base,
            "--gate",
            "cargo test -p vivi",
            "--gate-result",
            "passed",
        ],
    );
    let pending = pending
        .lines()
        .find_map(|line| line.strip_prefix("delivery "))
        .unwrap()
        .trim()
        .to_string();
    let shown = ok(&project, &["need", "show", &pending, "--json"]);
    let thread: serde_json::Value = serde_json::from_str(&shown).unwrap();
    let root = thread
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["handle"] == pending)
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(root["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["push"]["status"], "not_pushed");
    assert_eq!(
        body["push"]["detail"],
        "local branch is unavailable in the mailspace checkout"
    );
}
