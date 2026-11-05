use super::{fetch_pinned, observe};
use crate::pump::config::PumpConfig;
use crate::pump::host::test_ctx;
use tempfile::tempdir;

const SHA: &str = "0123456789012345678901234567890123456789";

#[test]
fn slash_branch_observation_uses_full_literal_refs_and_keeps_missing_separate() {
    let dir = tempdir().unwrap();
    let mut cfg = PumpConfig::default();
    let mut target = crate::pump::config::PumpTarget::default();
    target.main = "peer/main".into();
    target.intake = vec!["peer/intake".into()];
    cfg.targets.insert("default".into(), target);
    let (ctx, host) = test_ctx(dir.path(), cfg);
    host.reply("ls-remote", 0, &format!("{SHA}\trefs/heads/peer/main\n"));
    let heads = observe(&ctx);
    assert_eq!(heads.len(), 2);
    assert!(
        heads
            .iter()
            .any(|h| h.branch == "peer/main" && h.sha.as_deref() == Some(SHA))
    );
    assert!(
        heads
            .iter()
            .any(|h| h.branch == "peer/intake" && h.sha.is_none() && h.error.is_none())
    );
    assert!(host.commands()[0].contains("refs/heads/peer/main"));
    assert!(host.commands()[0].contains("refs/heads/peer/intake"));
}

#[test]
fn fetch_checks_resolved_sha_against_observation() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.reply("fetch", 0, "");
    host.reply("rev-parse", 0, &format!("{}\n", "f".repeat(40)));
    let head = super::BranchHead {
        target: "default".into(),
        branch: "main".into(),
        sha: Some(SHA.into()),
        error: None,
    };
    assert!(fetch_pinned(&ctx, &head).unwrap_err().contains("mismatch"));
}

#[test]
fn one_failed_observation_does_not_require_fetching_handled_tips() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.reply("ls-remote", 0, &format!("{SHA}\trefs/heads/main\n"));
    let heads = observe(&ctx);
    assert_eq!(heads[0].sha.as_deref(), Some(SHA));
    assert_eq!(
        host.commands()
            .iter()
            .filter(|cmd| cmd.contains("git ls-remote"))
            .count(),
        1
    );
    assert!(host.commands().iter().all(|cmd| !cmd.contains("fetch")));
}
