use super::spec;
use crate::pump::config::{PumpConfig, PumpTarget, RungsConfig};
use crate::pump::host::test_ctx;
use crate::pump::jobs::{PoolJob, pool_command};

fn config() -> PumpConfig {
    let mut cfg = PumpConfig::default();
    cfg.rungs = RungsConfig {
        need: Some(String::from("cargo test")),
        timeout: 900,
        pools: vec![String::from("build-pool")],
        ..RungsConfig::default()
    };
    cfg
}

#[test]
fn spec_carries_the_rung_command_tip_and_budget() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), config());

    let job = spec(&ctx, "core", "need", "abc123", "step9").unwrap();

    assert_eq!(job.kind, "rung");
    assert!(job.read_only);
    assert_eq!(job.command.as_deref(), Some("cargo test"));
    assert_eq!(job.budget, 900);
    assert_eq!(job.pool_candidates, vec![String::from("build-pool")]);
    assert_eq!(job.from_ref.as_deref(), Some("abc123"));
    assert_eq!(job.target.as_deref(), Some("core"));
    assert_eq!(
        job.payload,
        serde_json::json!({"rung": "need", "tip": "abc123", "target": "core"})
    );
}

#[test]
fn an_unconfigured_rung_gives_none() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), config());

    assert!(spec(&ctx, "core", "main", "abc123", "step9").is_none());
    assert!(spec(&ctx, "core", "nonsense", "abc123", "step9").is_none());
}

#[test]
fn a_target_rung_overrides_the_workspace_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config();
    cfg.targets.insert(
        String::from("core"),
        PumpTarget {
            rungs: RungsConfig {
                need: Some(String::from("make check")),
                timeout: 60,
                pools: vec![String::from("own-pool")],
                ..RungsConfig::default()
            },
            ..PumpTarget::default()
        },
    );
    let (ctx, _host) = test_ctx(dir.path(), cfg);

    let job = spec(&ctx, "core", "need", "abc123", "step9").unwrap();

    assert_eq!(job.command.as_deref(), Some("make check"));
    assert_eq!(job.budget, 60);
    assert_eq!(job.pool_candidates, vec![String::from("own-pool")]);
}

#[test]
fn pool_command_emits_command_only_for_command_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), config());
    let job = PoolJob::from(spec(&ctx, "core", "need", "abc123", "step9").unwrap());

    let line = pool_command(&job, "build-pool", "task1");
    assert!(line.contains("--command 'cargo test'"), "{line}");
    assert!(line.contains("--read-only"), "{line}");

    let mut plain = job;
    plain.command = None;
    assert!(!pool_command(&plain, "build-pool", "task1").contains("--command"));
}
