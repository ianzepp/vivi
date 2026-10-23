//! Tests for [`crate::pump::config::RungsConfig`] and the `rung` accessor.

use std::path::Path;

use crate::pump::config::{DEFAULT_RUNG_TIMEOUT, PumpConfig, RungsConfig};

fn pump(text: &str) -> PumpConfig {
    toml::from_str(text).expect("pump table should parse")
}

/// Rung findings only: the rest of a bare config is unfinished on purpose.
fn rung_errors(cfg: &PumpConfig, pools: &[String]) -> Vec<String> {
    cfg.findings_for(Path::new("."), pools, true)
        .errors
        .into_iter()
        .filter(|message| message.contains("rungs"))
        .collect()
}

#[test]
fn defaults_are_empty_with_a_half_hour_timeout() {
    let cfg = PumpConfig::default();
    assert!(cfg.rungs.is_empty());
    assert_eq!(cfg.rungs.timeout, DEFAULT_RUNG_TIMEOUT);
    assert_eq!(cfg.rungs.timeout, 1800);
    assert!(cfg.rungs.pools.is_empty());
    for name in ["task", "need", "main", "release"] {
        assert_eq!(cfg.rung("default", name), None);
    }
}

#[test]
fn parses_the_workspace_rungs_table() {
    let cfg = pump(concat!(
        "[rungs]\n",
        "task = \"cargo test --lib\"\n",
        "need = \"cargo test\"\n",
        "main = \"./gate\"\n",
        "release = \"./release-gate\"\n",
        "timeout = 600\n",
        "pools = [\"p\"]\n",
    ));
    assert_eq!(cfg.rungs.timeout, 600);
    assert_eq!(cfg.rungs.pools, vec!["p".to_string()]);
    assert_eq!(cfg.rung("any", "task"), Some("cargo test --lib"));
    assert_eq!(cfg.rung("any", "need"), Some("cargo test"));
    assert_eq!(cfg.rung("any", "main"), Some("./gate"));
    assert_eq!(cfg.rung("any", "release"), Some("./release-gate"));
    assert_eq!(cfg.rung("any", "bogus"), None);
}

#[test]
fn a_target_rung_wins_over_the_workspace_rung() {
    let cfg = pump(concat!(
        "[rungs]\n",
        "task = \"workspace task\"\n",
        "need = \"workspace need\"\n",
        "[targets.tela]\n",
        "path = \"tela\"\n",
        "branch = \"next\"\n",
        "[targets.tela.rungs]\n",
        "task = \"tela task\"\n",
        "[targets.cista]\n",
        "path = \"cista\"\n",
        "branch = \"next\"\n",
    ));
    assert_eq!(cfg.rung("tela", "task"), Some("tela task"));
    assert_eq!(cfg.rung("tela", "need"), Some("workspace need"));
    assert_eq!(cfg.rung("cista", "task"), Some("workspace task"));
    assert_eq!(cfg.rung("unknown", "task"), Some("workspace task"));
}

#[test]
fn an_empty_rungs_table_is_not_written_back() {
    let text = toml::to_string(&PumpConfig::default()).expect("serialize");
    assert!(!text.contains("rungs"), "{text}");
    let set = PumpConfig {
        rungs: RungsConfig {
            task: Some("t".into()),
            ..RungsConfig::default()
        },
        ..PumpConfig::default()
    };
    let text = toml::to_string(&set).expect("serialize");
    assert!(text.contains("[rungs]"), "{text}");
}

#[test]
fn validation_rejects_an_unknown_pool_and_a_zero_timeout() {
    let cfg = pump("[rungs]\ntimeout = 0\npools = [\"nope\", \"p\"]\n");
    let errors = rung_errors(&cfg, &["p".to_string()]);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("[pump.rungs] pools names unknown pool `nope`")),
        "{errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("[pump.rungs] timeout must be >= 1")),
        "{errors:?}"
    );
    assert_eq!(errors.len(), 2, "{errors:?}");
}

#[test]
fn validation_checks_a_target_override_too() {
    let cfg = pump(concat!(
        "[targets.tela]\n",
        "path = \"tela\"\n",
        "branch = \"next\"\n",
        "[targets.tela.rungs]\n",
        "pools = [\"nope\"]\n",
        "task = \"  \"\n",
    ));
    let errors = rung_errors(&cfg, &[]);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("[pump.targets.tela.rungs] pools names unknown pool `nope`")),
        "{errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("[pump.targets.tela.rungs] task must not be empty")),
        "{errors:?}"
    );
}

#[test]
fn a_valid_rungs_table_has_no_rung_errors() {
    let cfg = pump("[rungs]\ntask = \"x\"\npools = [\"p\"]\n");
    assert!(rung_errors(&cfg, &["p".to_string()]).is_empty());
}

#[test]
fn an_unknown_rung_name_is_a_parse_error() {
    let err = toml::from_str::<PumpConfig>("[rungs]\nnightly = \"x\"\n")
        .expect_err("only the four rung names parse");
    assert!(err.to_string().contains("nightly"), "{err}");
}
