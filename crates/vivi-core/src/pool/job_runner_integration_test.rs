//! Every fetch checkout gets a fresh, credential-free integration ref.
use super::*;

fn run_integration_job(rig: &Rig, main: &str, targeted: bool, read_only: bool) -> JobResult {
    let expected = git(&rig.f.repo, &["rev-parse", main]);
    let output = rig.work.path().join("integration-seen");
    let remote = format!("file://{}", rig.f.repo.display());
    let harness = format!(
        "set -e; test \"$(git remote get-url origin)\" = {}; test \"$(git rev-parse origin/{main})\" = {expected}; git merge --ff-only origin/{main}; printf '%s' {expected} > {}",
        sh_quote(&remote),
        sh_quote(&output.display().to_string())
    );
    let params = format!(
        "FROM='{}'\nINTEGRATION_REF='{main}'\nREAD_ONLY='{}'\nBRANCH='pool/integration-check'\n",
        rig.start,
        if read_only { "1" } else { "" }
    );
    let key = "a".repeat(64);
    let (result, log) = rig.run_in_context_logged(
        &harness,
        "build",
        &[],
        &rig.f.repo,
        targeted.then_some(("a", key.as_str())),
        &params,
    );
    let agent_err =
        std::fs::read_to_string(rig.work.path().join("job/agent.err")).unwrap_or_default();
    assert_eq!(
        result.get("agent_rc"),
        "0",
        "{:?}\n{log}\n{agent_err}",
        result.fields
    );
    assert_eq!(std::fs::read_to_string(output).unwrap(), expected);
    let repo = if targeted {
        rig.work.path().join("targets").join(key).join("repo")
    } else {
        rig.repo()
    };
    assert_eq!(
        git(&repo, &["rev-parse", &format!("origin/{main}")]),
        expected
    );
    assert!(!repo.join(".git/FETCH_HEAD").exists());
    result
}

#[test]
fn integration_ref_is_fetched_in_fresh_and_reused_checkouts_with_cargo_on_or_off() {
    for enabled in [false, true] {
        for targeted in [false, true] {
            for read_only in [false, true] {
                for main in ["vivi/main", "release/trunk"] {
                    let rig = Rig::new();
                    // Match the seed and target paths: git init, no remote and
                    // no tracking refs, unlike the clones older tests used.
                    std::fs::remove_dir_all(rig.repo()).unwrap();
                    std::fs::create_dir(rig.repo()).unwrap();
                    git(&rig.repo(), &["init", "-q"]);
                    let cache = tempfile::tempdir().unwrap();
                    if enabled {
                        install(&rig, &cache.path().join("slot-01"), false);
                    }
                    git(&rig.f.repo, &["branch", main]);
                    run_integration_job(&rig, main, targeted, read_only);
                    let checkout = if targeted {
                        rig.work
                            .path()
                            .join("targets")
                            .join("a".repeat(64))
                            .join("repo")
                    } else {
                        rig.repo()
                    };
                    git(
                        &checkout,
                        &[
                            "remote",
                            "set-url",
                            "origin",
                            "file:///missing-stale-origin",
                        ],
                    );
                    // A warm/reused cache must refresh main, not merely retain
                    // the ref established on its first job. FROM stays pinned.
                    std::fs::write(rig.f.repo.join("a.txt"), "integration advanced\n").unwrap();
                    git(&rig.f.repo, &["commit", "-q", "-am", "advance integration"]);
                    git(&rig.f.repo, &["branch", "-f", main, "HEAD"]);
                    run_integration_job(&rig, main, targeted, read_only);
                }
            }
        }
    }
}

#[test]
fn a_missing_integration_ref_stops_before_the_agent_even_with_cargo_enabled() {
    let rig = Rig::new();
    let cache = tempfile::tempdir().unwrap();
    install(&rig, &cache.path().join("slot-01"), false);
    let marker = rig.work.path().join("agent-ran");
    let (result, _) = rig.run_in_context_logged(
        &format!("touch {}", sh_quote(&marker.display().to_string())),
        "build",
        &[],
        &rig.f.repo,
        Some(("a", &"a".repeat(64))),
        "INTEGRATION_REF='missing/integration'\n",
    );
    assert_eq!(result.push(), "fetch_failed", "{:?}", result.fields);
    assert_eq!(result.get("agent_rc"), "");
    assert!(!marker.exists());
}

#[test]
fn integration_fetch_keeps_credentials_out_of_origin_and_fetch_head() {
    let rig = Rig::new();
    let remote = format!("file://{}", rig.f.repo.display());
    // Stand in for an authenticated server only at the runner's network
    // boundary. The harness still uses real Git and reads the actual config.
    let params = format!(
        "REMOTE='https://x-access-token:integration-secret@example.invalid/repo.git'\n\
         git() {{ if [ \"$1\" != fetch ]; then command git \"$@\"; return $?; fi; local arg; local -a args=(); for arg in \"$@\"; do case \"$arg\" in https://*) arg={} ;; esac; args+=(\"$arg\"); done; command git \"${{args[@]}}\"; }}\n",
        sh_quote(&remote)
    );
    let (result, _) = rig.run_in_context_logged(
        "test \"$(git remote get-url origin)\" = https://example.invalid/repo.git && test ! -f .git/FETCH_HEAD && ! grep -q integration-secret .git/config",
        "build", &[], &rig.f.repo, Some(("a", &"a".repeat(64))), &params,
    );
    assert_eq!(result.get("agent_rc"), "0", "{:?}", result.fields);
    let config = std::fs::read_to_string(
        rig.work
            .path()
            .join("targets")
            .join("a".repeat(64))
            .join("repo/.git/config"),
    )
    .unwrap();
    assert!(!config.contains("integration-secret"));
    assert!(!config.contains("x-access-token"));
}

#[test]
fn a_remote_backed_worktree_refreshes_its_inherited_integration_ref() {
    let rig = Rig::new();
    std::fs::remove_dir_all(rig.repo()).unwrap();
    git(
        &rig.f.repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            rig.repo().to_str().unwrap(),
            &rig.start,
        ],
    );
    git(
        &rig.f.repo,
        &[
            "remote",
            "add",
            "origin",
            &format!("file://{}", rig.f.repo.display()),
        ],
    );
    git(&rig.f.repo, &["branch", "vivi/main"]);
    let params = format!(
        "REMOTE=''\nLOCAL_ACTION='detached'\nLOCAL_START='{}'\nINTEGRATION_REF='vivi/main'\n",
        rig.start
    );
    let (result, _) = rig.run_in_context_logged(
        "test \"$(git rev-parse origin/vivi/main)\" = \"$(git rev-parse HEAD)\"",
        "build",
        &[],
        &rig.f.repo,
        None,
        &params,
    );
    assert_eq!(result.get("agent_rc"), "0", "{:?}", result.fields);
    assert_eq!(
        git(&rig.repo(), &["rev-parse", "origin/vivi/main"]),
        rig.start
    );
}

#[test]
fn integration_remains_mergeable_after_shallow_extra_ref_fetches() {
    let mut rig = Rig::new();
    git(&rig.f.repo, &["branch", "vivi/main"]);
    for n in 0..8 {
        git(
            &rig.f.repo,
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                &format!("advance {n}"),
            ],
        );
    }
    rig.start = git(&rig.f.repo, &["rev-parse", "HEAD"]);
    git(&rig.f.repo, &["checkout", "-q", "-b", "feature", "HEAD~1"]);
    git(
        &rig.f.repo,
        &["commit", "-q", "--allow-empty", "-m", "extra branch"],
    );
    git(&rig.f.repo, &["checkout", "-q", "main"]);
    let params = format!(
        "FROM='{}'\nINTEGRATION_REF='vivi/main'\nprintf '%s\\n' 'feature -' > \"$J/with.list\"\n",
        rig.start
    );
    let (result, log) = rig.run_in_context_logged(
        "git merge-base HEAD origin/feature && git merge --ff-only origin/vivi/main",
        "build",
        &[],
        &rig.f.repo,
        Some(("a", &"a".repeat(64))),
        &params,
    );
    let err = std::fs::read_to_string(rig.work.path().join("job/agent.err")).unwrap();
    assert_eq!(
        result.get("agent_rc"),
        "0",
        "{:?}\n{log}\n{err}",
        result.fields
    );
}

#[test]
fn a_worktree_cannot_silently_skip_a_configured_integration_ref_without_origin() {
    let rig = Rig::new();
    std::fs::remove_dir_all(rig.repo()).unwrap();
    git(
        &rig.f.repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            rig.repo().to_str().unwrap(),
            &rig.start,
        ],
    );
    let params = format!(
        "REMOTE=''\nLOCAL_ACTION='detached'\nLOCAL_START='{}'\nINTEGRATION_REF='vivi/main'\n",
        rig.start
    );
    let marker = rig.work.path().join("agent-ran");
    let (result, _) = rig.run_in_context_logged(
        &format!("touch {}", sh_quote(&marker.display().to_string())),
        "build",
        &[],
        &rig.f.repo,
        None,
        &params,
    );
    assert_eq!(result.push(), "fetch_failed", "{:?}", result.fields);
    assert!(!marker.exists());
}
