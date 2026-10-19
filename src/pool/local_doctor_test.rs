//! The checks of the `local` host, from facts and from this machine.

use super::*;
use crate::pool::config::PoolConfig;
use crate::pool::doctor::Status;
use crate::pool::local_fixture::fixture;

fn facts() -> LocalFacts {
    LocalFacts {
        root: "/r/lanes".into(),
        system: "macos aarch64".into(),
        missing: vec![],
        processes: Ok(412),
        root_exists: true,
        root_writable: true,
        anchor: "/r/lanes".into(),
        free_kb: Some(50 * KIB_PER_GIB),
        source: None,
        harness_env: 0,
        unset_env: vec![],
    }
}

fn by_name<'a>(list: &'a [Check], name: &str) -> &'a Check {
    list.iter().find(|c| c.name == name).unwrap()
}

#[test]
fn a_healthy_machine_passes_every_check() {
    let list = checks(&facts());
    assert_eq!(list.len(), 7);
    for c in &list {
        assert_eq!(c.status, Status::Ok, "{}: {}", c.name, c.detail);
        assert_eq!(c.group, GROUP);
    }
    assert!(by_name(&list, "system").detail.contains("no isolation"));
    assert!(by_name(&list, "source").detail.contains("fetch mode"));
}

#[test]
fn missing_tools_and_no_ps_fail_and_name_what_to_install() {
    let mut f = facts();
    f.missing = vec!["git".into(), "tar".into()];
    f.processes = Err("failed to run `ps`: not found".into());
    let list = checks(&f);
    let tools = by_name(&list, "tools");
    assert_eq!(tools.status, Status::Fail);
    assert!(tools.detail.contains("git, tar"));
    let ps = by_name(&list, "process control");
    assert_eq!(ps.status, Status::Fail);
    assert!(ps.detail.contains("not found"));
}

#[test]
fn the_root_is_made_on_create_but_must_be_writable_where_it_would_go() {
    let mut f = facts();
    f.root_exists = false;
    let list = checks(&f);
    assert_eq!(by_name(&list, "root").status, Status::Ok);
    assert!(by_name(&list, "root").detail.contains("will be created"));
    f.root_writable = false;
    let list = checks(&f);
    assert_eq!(by_name(&list, "root").status, Status::Fail);
    assert!(by_name(&list, "root").detail.contains("not writable"));
}

#[test]
fn low_or_unreadable_disk_is_a_warning_not_a_failure() {
    let mut f = facts();
    f.free_kb = Some(2 * KIB_PER_GIB + KIB_PER_GIB / 2);
    let list = checks(&f);
    assert_eq!(by_name(&list, "disk").status, Status::Warn);
    assert!(by_name(&list, "disk").detail.contains("2.5 GiB free"));
    f.free_kb = None;
    assert_eq!(by_name(&checks(&f), "disk").status, Status::Warn);
    f.free_kb = Some(5 * KIB_PER_GIB);
    assert_eq!(by_name(&checks(&f), "disk").status, Status::Warn);
    f.free_kb = Some(20 * KIB_PER_GIB);
    assert_eq!(by_name(&checks(&f), "disk").status, Status::Ok);
}

#[test]
fn the_source_check_reports_the_repository_of_a_worktree_pool() {
    let mut f = facts();
    let repo = RepoFacts {
        worktrees: 3,
        default_branch: Some("main".into()),
        primary_branch: Some("work".into()),
    };
    f.source = Some(("/r/project".into(), Ok(repo)));
    let list = checks(&f);
    let source = by_name(&list, "source");
    assert_eq!(source.status, Status::Ok);
    for part in [
        "worktree mode",
        "/r/project",
        "3 worktree(s)",
        "primary branch work",
        "default branch main",
    ] {
        assert!(source.detail.contains(part), "{part}: {}", source.detail);
    }
    f.source = Some(("/r/project".into(), Err("not a Git repository".into())));
    let source = checks(&f);
    assert_eq!(by_name(&source, "source").status, Status::Fail);
}

#[test]
fn unset_harness_names_warn_and_are_named_without_values() {
    let mut f = facts();
    f.harness_env = 2;
    assert_eq!(by_name(&checks(&f), "harness env").status, Status::Ok);
    f.unset_env = vec!["ALPHA_KEY".into()];
    let list = checks(&f);
    let env = by_name(&list, "harness env");
    assert_eq!(env.status, Status::Warn);
    assert!(env.detail.contains("ALPHA_KEY"));
}

fn provider_for(toml: &str) -> LocalProvider {
    let config: PoolConfig = toml::from_str(toml).unwrap();
    LocalProvider::new(&config, &ctx())
}

/// A host context for an absolute-root pool; only its paths are used.
fn ctx() -> crate::pool::runpod::HostContext {
    crate::pool::runpod::HostContext {
        dir: std::path::PathBuf::from("/tmp/vivi-test-project/.vivi"),
        mailspace: "space".to_string(),
        pool: "lanes".to_string(),
        pools: vec!["lanes".to_string()],
    }
}

#[test]
fn the_provider_probes_this_machine_without_creating_anything() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("not-yet/lanes");
    let p = provider_for(&format!(
        "provider = \"local\"\nslots = 1\n[local]\nroot = \"{}\"\n",
        root.display()
    ));
    let list = p.doctor_checks(&["VIVI_TEST_SURELY_UNSET_NAME".to_string()]);
    assert!(
        !root.exists() && !root.parent().unwrap().exists(),
        "doctor reads only"
    );
    assert_eq!(
        by_name(&list, "tools").status,
        Status::Ok,
        "{}",
        by_name(&list, "tools").detail
    );
    assert_eq!(by_name(&list, "process control").status, Status::Ok);
    assert_eq!(by_name(&list, "root").status, Status::Ok);
    assert_eq!(by_name(&list, "disk").status, Status::Ok);
    assert_eq!(by_name(&list, "harness env").status, Status::Warn);
}

#[test]
fn the_provider_reads_a_real_repository_for_a_worktree_pool_and_fails_on_a_missing_one() {
    let fx = fixture();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("lanes");
    let p = provider_for(&format!(
        "provider = \"local\"\nslots = 1\n[local]\nroot = \"{}\"\nworktree_of = \"{}\"\n",
        root.display(),
        fx.repo.display()
    ));
    let list = p.doctor_checks(&[]);
    let source = by_name(&list, "source");
    assert_eq!(source.status, Status::Ok, "{}", source.detail);
    assert!(
        source.detail.contains("1 worktree(s)") && source.detail.contains("primary branch main"),
        "{}",
        source.detail
    );

    let missing = fx.dir.path().join("nothing-here");
    let p = provider_for(&format!(
        "provider = \"local\"\nslots = 1\n[local]\nroot = \"{}\"\nworktree_of = \"{}\"\n",
        root.display(),
        missing.display()
    ));
    let list = p.doctor_checks(&[]);
    assert_eq!(by_name(&list, "source").status, Status::Fail);
    let plain = fx.dir.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    let p = provider_for(&format!(
        "provider = \"local\"\nslots = 1\n[local]\nroot = \"{}\"\nworktree_of = \"{}\"\n",
        root.display(),
        plain.display()
    ));
    assert_eq!(
        by_name(&p.doctor_checks(&[]), "source").status,
        Status::Fail
    );
}
