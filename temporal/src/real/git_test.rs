use super::*;

#[test]
fn ls_remote_matches_the_exact_ref() {
    let out = "aaa\trefs/heads/temporal/main\nbbb\trefs/heads/temporal/main2\n";
    assert_eq!(
        parse_ls_remote(out, "refs/heads/temporal/main"),
        Some("aaa".to_string())
    );
    assert_eq!(parse_ls_remote(out, "refs/heads/other"), None);
    assert_eq!(parse_ls_remote("", "refs/heads/x"), None);
}

#[test]
fn lease_refusals_are_fenced() {
    assert!(is_fenced(" ! [rejected]  x -> y (stale info)"));
    assert!(is_fenced("! [rejected] main -> main (non-fast-forward)"));
    assert!(!is_fenced(
        "ssh: connect to host hub.local port 22: Connection refused"
    ));
}

/// The fence end to end against a throwaway bare repository.
#[tokio::test]
async fn fenced_push_refuses_a_stale_expectation() {
    let dir = std::env::temp_dir().join(format!("vt-git-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let bare = dir.join("lane.git");
    let work = dir.join("work");
    let remote = bare.display().to_string();
    git_ok(&dir, &["init", "-q", "--bare", &remote])
        .await
        .unwrap();
    git_ok(&dir, &["init", "-q", &work.display().to_string()])
        .await
        .unwrap();
    git_ok(&work, &["commit", "-q", "--allow-empty", "-m", "one"])
        .await
        .unwrap();
    let one = git_ok(&work, &["rev-parse", "HEAD"]).await.unwrap();
    git_ok(&work, &["commit", "-q", "--allow-empty", "-m", "two"])
        .await
        .unwrap();
    let two = git_ok(&work, &["rev-parse", "HEAD"]).await.unwrap();

    let first = push_fenced(&work, &remote, &one, "b", None).await.unwrap();
    assert_eq!(first, Pushed::Accepted);
    // A second "create" is fenced: the branch exists now.
    let again = push_fenced(&work, &remote, &two, "b", None).await.unwrap();
    assert!(matches!(again, Pushed::Fenced(_)), "{again:?}");
    let ok = push_fenced(&work, &remote, &two, "b", Some(&one))
        .await
        .unwrap();
    assert_eq!(ok, Pushed::Accepted);
    // A lost attempt that still expects `one` cannot move it back.
    let stale = push_fenced(&work, &remote, &one, "b", Some(&one))
        .await
        .unwrap();
    assert!(matches!(stale, Pushed::Fenced(_)), "{stale:?}");
    let head = remote_head(&work, &remote, "b").await.unwrap();
    assert_eq!(head, Some(two));
    let _ = std::fs::remove_dir_all(&dir);
}
