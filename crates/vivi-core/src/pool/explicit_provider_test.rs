use super::*;
use crate::pool::claim::claim_repository_slot;
use crate::pool::docker_cli::{RunOutput, Runner, finish};
use crate::pool::slot::{PoolId, slot_state};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Shell(PathBuf);
impl Runner for Shell {
    fn run(
        &self,
        args: &[String],
        input: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<RunOutput, VivariumError> {
        let child = Command::new("sh")
            .args(["-c", &args[0]])
            .env("HOME", &self.0)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        finish(child, input, timeout, "local ssh")
    }
}

fn provider(home: &Path, space: &str, declarations: &[(&str, &str)]) -> ExplicitProvider {
    let mut config: PoolConfig = toml::from_str("provider='ssh'\nslots=20\n").unwrap();
    config.explicit_slots = declarations
        .iter()
        .map(|(name, repo)| SlotConfig {
            name: (*name).into(),
            repository: (*repo).into(),
            host: "studio".into(),
            dir: home
                .join(format!("trees/{space}/{name}"))
                .display()
                .to_string(),
            pools: vec![],
        })
        .collect();
    let ctx = HostContext {
        mailspace: space.into(),
        pool: "studio".into(),
        ..HostContext::default()
    };
    ExplicitProvider::with_runners(&config, &ctx, |_| Box::new(Shell(home.to_path_buf())))
}

fn budget(home: &Path, limit: u32) {
    std::fs::create_dir_all(home.join("vivi-worker")).unwrap();
    std::fs::write(
        home.join("vivi-worker/worker.conf"),
        format!("max_busy={limit}\n"),
    )
    .unwrap();
}
fn claim(p: &ExplicitProvider, space: &str, repo: &str) -> super::super::claim::Claim {
    claim_repository_slot(p, &PoolId::keyed(space, "studio"), "t1", 100, Some(repo)).unwrap()
}

#[test]
fn claims_only_a_free_slot_of_the_repository_by_declared_name() {
    let home = tempfile::tempdir().unwrap();
    let p = provider(
        home.path(),
        "faberlang",
        &[
            ("gradus-1", "faberlang/gradus"),
            ("radix-1", "faberlang/radix"),
            ("radix-2", "faberlang/radix"),
        ],
    );
    let one = claim(&p, "faberlang", "git@hub:faberlang/radix.git");
    let two = claim(&p, "faberlang", "https://hub/faberlang/radix.git");
    assert_eq!(one.slot.name, "radix-1");
    assert_eq!(two.slot.name, "radix-2");
    assert!(
        claim_repository_slot(
            &p,
            &PoolId::keyed("faberlang", "studio"),
            "t3",
            100,
            Some("git@hub:faberlang/radix.git")
        )
        .is_err()
    );
    assert_eq!(slot_state(&p.machine("gradus-1").unwrap()), "free");
    assert!(!home.path().join("trees/faberlang/gradus-1").exists());
    p.unclaim("radix-1").unwrap();
    p.unclaim("radix-2").unwrap();
}

#[test]
fn host_budget_is_shared_between_mailspaces_released_and_dead_holders_reclaimed() {
    let home = tempfile::tempdir().unwrap();
    budget(home.path(), 1);
    let a = provider(home.path(), "faberlang", &[("radix-1", "faberlang/radix")]);
    let b = provider(home.path(), "vivi", &[("vivi-1", "ianzepp/vivi")]);
    claim(&a, "faberlang", "faberlang/radix");
    assert!(
        b.claim_repository("vivi-1", "t2", 101, Some("ianzepp/vivi"))
            .unwrap()
            .is_none()
    );
    a.unclaim("radix-1").unwrap();
    claim(&b, "vivi", "ianzepp/vivi");
    b.stop("vivi-1").unwrap();
    b.unclaim("vivi-1").unwrap();
    std::fs::write(
        home.path().join("vivi-worker/.busy/dead"),
        "99999999 vanished\n",
    )
    .unwrap();
    claim(&a, "faberlang", "faberlang/radix");
    assert!(!home.path().join("vivi-worker/.busy/dead").exists());
    a.unclaim("radix-1").unwrap();
}

fn git(path: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}

fn run_job(p: &ExplicitProvider, home: &Path, source: &Path, number: u32) {
    use crate::pool::{dispatch, github::Secret};
    let mut cfg = p.config.clone();
    cfg.remote_url = Some(format!("file://{}", source.display()));
    cfg.committer = Some(toml::from_str("name='Bot'\nemail='bot@example.invalid'").unwrap());
    let flags = dispatch::DispatchFlags {
        command: Some("mkdir -p target; echo warm > target/cache; test -f tracked.txt".into()),
        from: Some("main".into()),
        read_only: true,
        ..dispatch::DispatchFlags::default()
    };
    let mut plan = dispatch::plan_job(
        &cfg,
        "studio",
        ("t1", "Review", &vivi_mail::letter::fixture::task_body("")),
        &flags,
        "rules".into(),
        format!("run{number}"),
    )
    .unwrap();
    plan.remote = format!("file://{}", source.display());
    plan.target = Some(crate::pool::dispatch::target::JobTarget {
        id: "alias".into(),
        source: plan.remote.clone(),
        publish_remote: None,
        cache_key: "a".repeat(64),
        local_source: None,
        seed: vec![],
        requires: vec![],
        build_dir: Some("target".into()),
        check: None,
    });
    let token = Secret::new(String::new());
    let progress = crate::pool::progress::Progress::silent();
    let launch = dispatch::Launch {
        config: &cfg,
        plan: &plan,
        token: &token,
        start: "main",
        progress: &progress,
        ready_timeout: None,
        reset_base: None,
    };
    let record = dispatch::dispatch(
        p,
        &PoolId::keyed("faberlang", "studio"),
        &launch,
        100,
        &crate::pool::collect::Pacing::default(),
    )
    .unwrap();
    assert_eq!(record.slot, "radix-1");
    let records = home.join("trees/faberlang/radix-1/.vivi");
    let deadline = Instant::now() + Duration::from_secs(15);
    while !records.join("job/result").exists()
        || !std::fs::read_to_string(records.join("job/result"))
            .unwrap_or_default()
            .contains(&format!("job=run{number}"))
    {
        assert!(
            Instant::now() < deadline,
            "no result: {:?}",
            std::fs::read_to_string(records.join("boot.out"))
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let result = std::fs::read_to_string(records.join("job/result")).unwrap();
    assert!(
        result.contains("agent_rc=0"),
        "{result}: {:?}",
        std::fs::read_to_string(records.join("job/run.log"))
    );
    while records.join("busy-token").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    p.stop("radix-1").unwrap();
    p.unclaim("radix-1").unwrap();
}

#[test]
fn two_jobs_reuse_exact_dir_preserve_mtimes_output_and_exclude_records() {
    let home = tempfile::tempdir().unwrap();
    budget(home.path(), 1);
    let source = home.path().join("remotes/faberlang/radix");
    std::fs::create_dir_all(&source).unwrap();
    git(&source, &["init", "-q", "-b", "main"]);
    std::fs::write(source.join("tracked.txt"), "unchanged").unwrap();
    std::fs::write(source.join(".gitignore"), "target/\n").unwrap();
    git(&source, &["add", "."]);
    git(
        &source,
        &[
            "-c",
            "user.name=Bot",
            "-c",
            "user.email=bot@localhost",
            "commit",
            "-qm",
            "Initial",
        ],
    );
    let p = provider(home.path(), "faberlang", &[("radix-1", "faberlang/radix")]);
    run_job(&p, home.path(), &source, 1);
    let dir = home.path().join("trees/faberlang/radix-1");
    let before = std::fs::metadata(dir.join("tracked.txt"))
        .unwrap()
        .modified()
        .unwrap();
    std::fs::write(dir.join("target/sentinel"), "keep").unwrap();
    run_job(&p, home.path(), &source, 2);
    assert_eq!(
        before,
        std::fs::metadata(dir.join("tracked.txt"))
            .unwrap()
            .modified()
            .unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("target/sentinel")).unwrap(),
        "keep"
    );
    assert_eq!(git(&dir, &["status", "--porcelain"]), "");
    assert_eq!(git(&dir, &["check-ignore", ".vivi/claim"]), ".vivi/claim");
    for file in ["source", "boot.log", "job/result", "prepared"] {
        assert!(dir.join(".vivi").join(file).exists(), "{file}");
    }
    assert!(!dir.join("repo").exists());
    assert!(!dir.join("targets").exists());
    assert!(
        std::fs::read_dir(home.path().join("vivi-worker/.busy"))
            .unwrap()
            .next()
            .is_none()
    );
    p.destroy("radix-1").unwrap();
    assert!(dir.join("tracked.txt").is_file());
}

#[test]
fn concurrent_mailspace_claims_cannot_exceed_one_host_place() {
    let home = tempfile::tempdir().unwrap();
    budget(home.path(), 1);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let winners = std::thread::scope(|scope| {
        let joins: Vec<_> = (0..8)
            .map(|n| {
                let home = home.path();
                let barrier = barrier.clone();
                scope.spawn(move || {
                    let space = format!("space{n}");
                    let p = provider(home, &space, &[("radix-1", "faberlang/radix")]);
                    barrier.wait();
                    let won = p
                        .claim_repository("radix-1", "t1", 100, Some("faberlang/radix"))
                        .unwrap()
                        .is_some();
                    (space, won)
                })
            })
            .collect();
        joins
            .into_iter()
            .map(|j| j.join().unwrap())
            .filter(|(_, won)| *won)
            .collect::<Vec<_>>()
    });
    assert_eq!(winners.len(), 1);
    let p = provider(
        home.path(),
        &winners[0].0,
        &[("radix-1", "faberlang/radix")],
    );
    p.unclaim("radix-1").unwrap();
}
