//! Tests for target-local intake configuration and validation.

use std::collections::BTreeMap;
use std::path::Path;

use super::super::{PumpConfig, PumpTarget};

fn target(path: &str) -> PumpTarget {
    PumpTarget {
        path: path.to_owned(),
        ..PumpTarget::default()
    }
}

#[test]
fn intake_defaults_disabled() {
    let config = PumpConfig::default();
    let legacy = config.target_configs().remove("default").unwrap();
    assert!(legacy.intake.is_empty());
    assert_eq!(legacy.intake_rung, "need");

    let explicit = PumpTarget::default();
    assert!(explicit.intake.is_empty());
    assert_eq!(explicit.intake_rung, "need");
}

#[test]
fn intake_invalid_branch() {
    let mut config = PumpConfig::default();
    let mut configured = target("repo");
    configured.main = String::from("trunk");
    configured.intake = [
        "trunk",
        "refs/heads/peer/main",
        "main~1",
        "main^",
        "source:destination",
        "feature/*",
        "bad//component",
        "bad/../component",
        "name.lock",
        "vivi/need/work",
        "pump/merge-main/work",
        "release",
        "release",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    configured.intake.push(String::from("peer/main"));
    config.targets = BTreeMap::from([(String::from("one"), configured)]);

    let found = config.findings_for(Path::new("."), &[]);
    assert!(
        found.errors.iter().any(|error| error.contains("duplicate")),
        "{:?}",
        found.errors
    );
    for invalid in [
        "trunk",
        "refs/heads/peer/main",
        "main~1",
        "main^",
        "source:destination",
        "feature/*",
        "bad//component",
        "bad/../component",
        "name.lock",
        "vivi/need/work",
        "pump/merge-main/work",
    ] {
        assert!(
            found.errors.iter().any(|error| error.contains(invalid)),
            "{invalid}: {:?}",
            found.errors
        );
    }
    assert!(
        found
            .errors
            .iter()
            .all(|error| !error.contains("invalid intake branch `peer/main`")),
        "peer/main is a literal valid branch, not a remote selector: {:?}",
        found.errors
    );
}

#[test]
fn intake_invalid_rung() {
    for rung in ["", "release", "Task", "task "] {
        let mut config = PumpConfig::default();
        let mut configured = target("repo");
        configured.intake_rung = rung.to_owned();
        config.targets.insert(String::from("one"), configured);
        let found = config.findings_for(Path::new("."), &[]);
        assert!(
            found
                .errors
                .iter()
                .any(|error| error.contains("intake_rung")),
            "{rung:?}: {:?}",
            found.errors
        );
    }
}

#[test]
fn intake_target_isolation() {
    let mut config = PumpConfig::default();
    config.rungs.task = Some(String::from("workspace task"));
    config.rungs.need = Some(String::from("workspace need"));
    config.rungs.main = Some(String::from("workspace main"));
    config.rungs.pools = vec![String::from("pool")];
    let mut alpha = target("alpha");
    alpha.intake = vec![String::from("peer/main")];
    alpha.intake_rung = String::from("task");
    alpha.rungs.task = Some(String::from("alpha task"));
    let mut beta = target("beta");
    beta.intake = vec![String::from("release")];
    config.targets = BTreeMap::from([(String::from("alpha"), alpha), (String::from("beta"), beta)]);

    assert_eq!(config.rung("alpha", "task"), Some("alpha task"));
    assert_eq!(config.rung("beta", "task"), Some("workspace task"));
    assert_eq!(config.missing_rungs(), Vec::new());
    let pools = vec![String::from("pool")];
    let found = config.findings_for(Path::new("."), &pools);
    assert!(found.errors.is_empty(), "{:?}", found.errors);

    config.rungs.main = None;
    assert!(
        config
            .missing_rungs()
            .iter()
            .any(|missing| missing.name == "pump.targets.alpha.rungs.main")
    );
    assert!(
        config
            .missing_rungs()
            .iter()
            .any(|missing| missing.name == "pump.targets.beta.rungs.main")
    );

    config.rungs.main = Some(String::from("workspace main"));
    config.rungs.pools.clear();
    let found = config.findings_for(Path::new("."), &[]);
    assert!(
        found
            .errors
            .iter()
            .any(|error| error.contains("effective pools are empty")),
        "a configured rung without a pool must not be ready: {:?}",
        found.errors
    );
}
