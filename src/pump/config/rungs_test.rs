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
    assert_eq!(cfg.rungs.timeout, None);
    assert_eq!(
        cfg.effective_rungs(None).timeout,
        Some(DEFAULT_RUNG_TIMEOUT)
    );
    assert_eq!(DEFAULT_RUNG_TIMEOUT, 1800);
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
    assert_eq!(cfg.rungs.timeout, Some(600));
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

#[test]
fn effective_rungs_takes_the_target_timeout_and_pools_over_the_workspace() {
    let cfg = pump(concat!(
        "[rungs]\n",
        "need = \"workspace need\"\n",
        "timeout = 600\n",
        "pools = [\"shared\"]\n",
        "[targets.tela]\n",
        "path = \"tela\"\n",
        "branch = \"next\"\n",
        "[targets.tela.rungs]\n",
        "timeout = 1800\n",
        "pools = [\"own\"]\n",
        "[targets.cista]\n",
        "path = \"cista\"\n",
        "branch = \"next\"\n",
    ));
    let tela = cfg.effective_rungs(Some("tela"));
    assert_eq!(tela.timeout, Some(1800));
    assert_eq!(tela.pools, vec!["own".to_string()]);
    assert_eq!(tela.need.as_deref(), Some("workspace need"));
    let cista = cfg.effective_rungs(Some("cista"));
    assert_eq!(cista.timeout, Some(600));
    assert_eq!(cista.pools, vec!["shared".to_string()]);
}

#[test]
fn show_with_a_target_prints_the_target_timeout_and_pools() {
    let project = tempfile::tempdir().unwrap();
    crate::mailspace::Mailspace::init(Some(project.path())).expect("init");
    let config = project.path().join(".vivi").join("mailspace.toml");
    let mut raw = std::fs::read_to_string(&config).unwrap();
    raw.push_str(concat!(
        "\n[pump.rungs]\ntimeout = 600\npools = [\"shared\"]\n",
        "[pump.targets.web]\npath = \"web\"\n",
        "[pump.targets.web.rungs]\ntimeout = 1800\npools = [\"own\"]\n",
    ));
    std::fs::write(&config, raw).unwrap();

    let web = RungsConfig::show(project.path(), Some("web")).unwrap();
    assert!(web.contains("timeout: 1800"), "{web}");
    assert!(web.contains("pools: own"), "{web}");
    let shared = RungsConfig::show(project.path(), None).unwrap();
    assert!(shared.contains("timeout: 600"), "{shared}");
    assert!(shared.contains("pools: shared"), "{shared}");
}

#[test]
fn validation_reports_a_set_rung_with_empty_effective_pools() {
    let cfg = pump("[rungs]\ntask = \"x\"\n");
    let errors = rung_errors(&cfg, &[]);
    assert!(
        errors
            .iter()
            .any(|e| e
                .contains("[pump.rungs] sets a rung command but its effective pools are empty")),
        "{errors:?}"
    );

    let cfg = pump(concat!(
        "[targets.tela]\n",
        "path = \"tela\"\n",
        "branch = \"next\"\n",
        "[targets.tela.rungs]\n",
        "task = \"x\"\n",
    ));
    let errors = rung_errors(&cfg, &[]);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("[pump.targets.tela.rungs] sets a rung command")),
        "{errors:?}"
    );

    let cfg = pump(concat!(
        "[rungs]\npools = [\"p\"]\n",
        "[targets.tela]\n",
        "path = \"tela\"\n",
        "branch = \"next\"\n",
        "[targets.tela.rungs]\n",
        "task = \"x\"\n",
    ));
    assert!(rung_errors(&cfg, &["p".to_string()]).is_empty());
}

#[test]
fn targets_with_their_own_pools_need_no_workspace_pools() {
    let cfg = pump(concat!(
        "[rungs]\ntask = \"x\"\n",
        "[targets.tela]\n",
        "path = \"tela\"\n",
        "branch = \"next\"\n",
        "[targets.tela.rungs]\npools = [\"p\"]\n",
        "[targets.cista]\n",
        "path = \"cista\"\n",
        "branch = \"next\"\n",
        "[targets.cista.rungs]\npools = [\"p\"]\n",
    ));
    assert!(rung_errors(&cfg, &["p".to_string()]).is_empty());
}

#[test]
fn a_target_inheriting_a_command_without_pools_is_reported_alone() {
    let cfg = pump(concat!(
        "[rungs]\ntask = \"x\"\n",
        "[targets.tela]\n",
        "path = \"tela\"\n",
        "branch = \"next\"\n",
        "[targets.tela.rungs]\npools = [\"p\"]\n",
        "[targets.cista]\n",
        "path = \"cista\"\n",
        "branch = \"next\"\n",
    ));
    let errors = rung_errors(&cfg, &["p".to_string()]);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("[pump.targets.cista.rungs] sets a rung command"),
        "{errors:?}"
    );
}

fn missing_names(cfg: &PumpConfig) -> Vec<String> {
    cfg.missing_rungs().into_iter().map(|m| m.name).collect()
}

#[test]
fn missing_rungs_reports_all_three_when_no_rungs_are_set() {
    let cfg = PumpConfig::default();
    assert_eq!(
        missing_names(&cfg),
        ["pump.rungs.task", "pump.rungs.need", "pump.rungs.main"]
    );
    assert!(cfg.missing_rungs()[0].detail.contains("task is not set"));
}

#[test]
fn missing_rungs_is_empty_when_the_workspace_sets_them_all() {
    let cfg = pump("[rungs]\ntask = \"t\"\nneed = \"n\"\nmain = \"m\"\n");
    assert!(cfg.missing_rungs().is_empty());
}

#[test]
fn missing_rungs_counts_what_a_target_inherits() {
    let cfg = pump(concat!(
        "[rungs]\ntask = \"t\"\nneed = \"n\"\nmain = \"m\"\n",
        "[targets.web]\npath = \"web\"\n",
        "[targets.web.rungs]\npools = [\"own\"]\n",
    ));
    assert!(cfg.missing_rungs().is_empty());
}

#[test]
fn missing_rungs_names_the_target_that_lacks_main() {
    let cfg = pump(concat!(
        "[targets.web]\npath = \"web\"\n",
        "[targets.web.rungs]\ntask = \"t\"\nneed = \"n\"\npools = [\"own\"]\n",
    ));
    assert_eq!(missing_names(&cfg), ["pump.targets.web.rungs.main"]);
}
