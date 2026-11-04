use std::path::Path;
use std::process::Command;

use super::*;

fn pool(extra: &str) -> PoolConfig {
    toml::from_str(&format!("slots = 1\nimage = \"debian:12\"\n{extra}")).unwrap()
}

#[test]
fn parse_symref_reads_the_head_branch() {
    let out = "ref: refs/heads/trunk\tHEAD\n0123456789abcdef0123456789abcdef01234567\tHEAD\n";
    assert_eq!(parse_symref(out).as_deref(), Some("trunk"));
    assert_eq!(parse_symref("abc\tHEAD\n"), None);
}

#[test]
fn pick_commit_takes_the_first_wanted_ref_listed() {
    let out = "aaa\trefs/tags/v1\nbbb\trefs/tags/v1^{}\nccc\trefs/heads/v1\n";
    assert_eq!(
        pick_commit(out, &["refs/heads/v1", "refs/tags/v1^{}"]).as_deref(),
        Some("ccc")
    );
    assert_eq!(
        pick_commit(out, &["refs/heads/x", "refs/tags/v1^{}", "refs/tags/v1"]).as_deref(),
        Some("bbb")
    );
    assert_eq!(pick_commit(out, &["refs/heads/x"]), None);
}

#[test]
fn a_remote_url_is_a_plain_git_source_and_the_address_jobs_use() {
    let config = pool("remote_url = \"git@hub:lanes/faberlang/radix.git\"");
    assert!(config.has_source());
    match config.require_source("p", "dispatch").unwrap() {
        RepoSource::Git(url) => assert_eq!(url, "git@hub:lanes/faberlang/radix.git"),
        RepoSource::Github(_) => panic!("expected a plain Git source"),
    }
    assert_eq!(config.remote_address(), "git@hub:lanes/faberlang/radix.git");
}

#[test]
fn repo_and_remote_url_together_are_refused() {
    let config = pool("repo = \"o/r\"\nremote_url = \"git@hub:r.git\"");
    let said = config
        .require_source("p", "dispatch")
        .unwrap_err()
        .to_string();
    assert!(
        said.contains("both repo") && said.contains("keep one"),
        "{said}"
    );
}

#[test]
fn an_https_remote_url_is_refused_in_favour_of_repo() {
    let config = pool("remote_url = \"https://github.com/o/r.git\"");
    let said = config
        .require_source("p", "dispatch")
        .unwrap_err()
        .to_string();
    assert!(
        said.contains("http(s) address") && said.contains("github"),
        "{said}"
    );
    for bad in ["", "-oProxyCommand=x", "git@hub:a b.git"] {
        let config = pool(&format!("remote_url = {bad:?}"));
        assert!(config.require_source("p", "dispatch").is_err(), "{bad:?}");
    }
}

#[test]
fn a_pool_with_no_source_still_needs_one_to_dispatch() {
    let config = pool("");
    assert!(!config.has_source());
    assert!(config.require_source("p", "dispatch").is_err());
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "init.defaultBranch=main",
        ])
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A bare remote with `main`, a `feature` branch one commit ahead, and an
/// annotated tag `v1` on `main`; returns its `file://` address and commits.
fn remote() -> (tempfile::TempDir, String, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    let bare = dir.path().join("remote.git");
    std::fs::create_dir(&work).unwrap();
    git(&work, &["init", "-q"]);
    git(&work, &["commit", "-q", "--allow-empty", "-m", "one"]);
    let main = git(&work, &["rev-parse", "HEAD"]);
    git(&work, &["tag", "-a", "v1", "-m", "v1"]);
    git(&work, &["checkout", "-q", "-b", "feature"]);
    git(&work, &["commit", "-q", "--allow-empty", "-m", "two"]);
    let feature = git(&work, &["rev-parse", "HEAD"]);
    git(
        dir.path(),
        &[
            "clone",
            "-q",
            "--bare",
            work.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    git(&bare, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    let url = format!("file://{}", bare.display());
    (dir, url, main, feature)
}

#[test]
fn refs_resolve_against_a_real_remote() {
    let (_dir, url, main, feature) = remote();
    assert_eq!(default_branch(&url).unwrap(), "main");
    assert_eq!(resolve_base(&url, None).unwrap(), main);
    assert_eq!(resolve_base(&url, Some("main")).unwrap(), main);
    assert_eq!(resolve_base(&url, Some("feature")).unwrap(), feature);
    assert_eq!(
        resolve_base(&url, Some("v1")).unwrap(),
        main,
        "annotated tag peels"
    );
    assert_eq!(
        resolve_base(&url, Some(&feature.to_uppercase())).unwrap(),
        feature
    );
}

#[test]
fn unknown_abbreviated_and_unsafe_refs_are_refused() {
    let (_dir, url, main, _) = remote();
    let unknown = resolve_base(&url, Some("nope")).unwrap_err().to_string();
    assert!(unknown.contains("no branch or tag \"nope\""), "{unknown}");
    let short = resolve_base(&url, Some(&main[..12]))
        .unwrap_err()
        .to_string();
    assert!(short.contains("abbreviated"), "{short}");
    assert!(resolve_base(&url, Some("--upload-pack=x")).is_err());
    assert!(resolve_base(&url, Some("a b")).is_err());
}

#[test]
fn an_unreachable_remote_names_itself() {
    let said = default_branch("file:///nonexistent/vivi-plain-git-test.git")
        .unwrap_err()
        .to_string();
    assert!(said.contains("git ls-remote file:///nonexistent"), "{said}");
}
