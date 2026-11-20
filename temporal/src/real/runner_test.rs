use super::*;

use std::path::PathBuf;
use std::time::Instant;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vt-runner-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn agent(slots_dir: &Path) -> AgentConfig {
    AgentConfig {
        repo: "/unused".into(),
        slots_dir: slots_dir.display().to_string(),
        model: "m1".into(),
        budget_s: 60,
        harness: "pi --model {model} @{brief}".into(),
        review_harness: "pi --tools read --model {model} @{brief}".into(),
    }
}

#[tokio::test]
async fn claims_take_free_slots_and_release_on_drop() {
    let dir = scratch("claim");
    let (a, lock_a) = claim(&dir, 2).unwrap();
    let (b, _lock_b) = claim(&dir, 2).unwrap();
    assert_eq!((a, b), (1, 2));
    assert!(claim(&dir, 2).is_err(), "a third job finds no worktree");
    drop(lock_a);
    assert_eq!(
        claim_waiting(&dir, 2).await.unwrap().0,
        1,
        "a released slot is free again"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn harness_fills_model_and_brief() {
    let a = agent(Path::new("/s"));
    let brief = Path::new("/s/slot-1.brief.md");
    assert_eq!(
        harness_command(&a, false, brief),
        "pi --model m1 @/s/slot-1.brief.md"
    );
    assert_eq!(
        harness_command(&a, true, brief),
        "pi --tools read --model m1 @/s/slot-1.brief.md"
    );
}

#[test]
fn a_throttled_agent_fails_as_provider() {
    let ran = Ran {
        rc: Some(1),
        receipt: "Error: 429 Too Many Requests (retry-after: 45)".into(),
        ..Ran::default()
    };
    assert!(!ran.agent_ok());
    let err = format!("{:?}", agent_failure("implement", &ran));
    assert!(err.contains("provider"), "{err}");
}

/// A worktree is made once, reset between jobs (untracked files go,
/// `target/` stays), and a job's changes become one commit.
#[tokio::test]
async fn worktree_reset_keeps_target_and_commits_changes() {
    let dir = scratch("tree");
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git_ok(&repo, &["init", "-q"]).await.unwrap();
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "target/\n").unwrap();
    git_ok(&repo, &["add", "-A"]).await.unwrap();
    git_ok(&repo, &["commit", "-q", "-m", "base"])
        .await
        .unwrap();
    let base = git_ok(&repo, &["rev-parse", "HEAD"]).await.unwrap();

    let tree = dir.join("slots/slot-1");
    std::fs::create_dir_all(dir.join("slots")).unwrap();
    reset(&repo, &tree, &base).await.unwrap();
    assert_eq!(commit(&tree, "nothing").await.unwrap(), None);

    std::fs::write(tree.join("a.txt"), "two\n").unwrap();
    std::fs::create_dir_all(tree.join("target")).unwrap();
    std::fs::write(tree.join("target/warm"), "x").unwrap();
    let sha = commit(&tree, "need s1 round 1").await.unwrap().unwrap();
    let subject = git_ok(&repo, &["log", "-1", "--format=%s", &sha])
        .await
        .unwrap();
    assert_eq!(subject, "need s1 round 1");

    std::fs::write(tree.join("stray.txt"), "left over").unwrap();
    reset(&repo, &tree, &base).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(tree.join("a.txt")).unwrap(),
        "one\n"
    );
    assert!(
        !tree.join("stray.txt").exists(),
        "untracked files are cleaned"
    );
    assert!(tree.join("target/warm").exists(), "target/ stays warm");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Killing the group stops the harness and what it started.
#[tokio::test]
async fn killing_the_group_stops_children_too() {
    let mut child = Command::new("sh")
        .args(["-c", "sleep 30 & sleep 30; wait"])
        .process_group(0)
        .spawn()
        .unwrap();
    let group = child.id();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let started = Instant::now();
    kill_group(group);
    child.wait().await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    let pgid = group.unwrap().to_string();
    let left = std::process::Command::new("pgrep")
        .args(["-g", &pgid])
        .output()
        .unwrap();
    assert!(
        left.stdout.is_empty(),
        "group {pgid} still has {:?}",
        left.stdout
    );
}

#[test]
fn the_agents_message_is_taken_and_removed() {
    let dir = scratch("msg");
    let forbid = vec!["8a2cd8ba".to_string(), "temporal/8a2cd8ba/s1".to_string()];
    std::fs::write(
        dir.join(MESSAGE_FILE),
        "Keep a NaN's sign through f16 rounding\n\nBody.\n",
    )
    .unwrap();
    assert_eq!(
        take_message(&dir, &forbid),
        "Keep a NaN's sign through f16 rounding\n\nBody."
    );
    assert!(
        !dir.join(MESSAGE_FILE).exists(),
        "the file is not committed"
    );
    assert_eq!(take_message(&dir, &forbid), FALLBACK_MESSAGE, "no file");
    std::fs::write(dir.join(MESSAGE_FILE), "Fix 8a2cd8ba\n").unwrap();
    assert_eq!(
        take_message(&dir, &forbid),
        FALLBACK_MESSAGE,
        "names the need"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
