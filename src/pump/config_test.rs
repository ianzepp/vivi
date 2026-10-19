//! Tests for [`crate::pump::config`].

use crate::mailspace::MailspaceConfig;
use crate::pump::config::{AlarmConfig, DispatchConfig, Kind, Lane, PumpConfig, PumpTarget};
use std::collections::BTreeMap;
use std::path::Path;

/// A kind dispatching to `pool`.
fn kind(pool: &str) -> Kind {
    Kind {
        pools: if pool.is_empty() {
            Vec::new()
        } else {
            vec![pool.to_string()]
        },
        ..Kind::default()
    }
}

/// The whole `[pump]` block of the design file, verbatim.
const FULL: &str = r#"
[pump]
home        = "/mnt/agents/pump"
repo        = "/mnt/agents/pump/repo"
remote      = "origin"
remote_url  = "git@example.com:org/repo.git"
committer_name  = "Ian Zepp"
committer_email = "ian@example.com"
branch      = "next"
main        = "main"
promote     = true
tick        = 20
shell_prefix = ". /mnt/agents/faber-runs/pharos-env.sh"
hand_rules  = "/mnt/agents/pump/hand-rules.md"
fix_prefix  = "pumpfix"
from        = "operator"
to          = "hand"
reply_from  = "mind"

[pump.board]
from         = ["operator@bd371d96.local"]
since        = "2026-10-14T00:10"
mark         = "pump"
every        = 300
planners_max = 3
plan_timeout = 1800
reply_to     = "operator@bd371d96.local"
pull         = true
push_since   = "30m"

[pump.planner]
run     = "claude -p --model claude-sonnet-5-5 --permission-mode bypassPermissions --append-system-prompt-file {charter}"
charter = "/mnt/agents/pump/planner-charter.md"
split_hard_cap = 5400

[pump.dispatch]
lanes        = [{ model = "deepseek/deepseek-flash", cap = 30 }, { model = "zai/glm-5.3-flash", cap = 10 }]
last_resort  = "zai/glm-5.3"
max_attempts = 3
budget       = 600
overdue_grace = 900
per_tick     = 6
from         = "next"
default_kind = "plain"

[pump.dispatch.kinds.plain]
pools = ["fly4"]

[pump.dispatch.kinds.full]
pools = ["flyfull", "flygate"]
max  = 2
skip_proof = true

[pump.merge]
batch         = 8
format        = "cargo fmt --all"
shared_checks = ["./scripta/test --stage 1", "./scripta/test --stage 2"]
proof_timeout = 300
remote_wait   = 90
remote_limit  = 1800

[pump.gate]
command      = "./scripta/test --stage 4b"
every_merges = 8
every_seconds = 1800
pools        = ["flyfull", "flygate"]
model        = "zai/glm-5.3-flash"
budget       = 1800
"#;

fn mailspace(text: &str) -> MailspaceConfig {
    toml::from_str(text).expect("mailspace file text should parse")
}

fn pump(text: &str) -> PumpConfig {
    mailspace(text).pump.expect("pump table should be present")
}

fn pools(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

#[test]
fn an_absent_pump_table_is_none() {
    assert!(mailspace("name = \"t\"\n").pump.is_none());
}

#[test]
fn a_kind_parses_with_optional_max_and_skip_proof() {
    let none = pump("name = \"t\"\n[pump]\nbranch = \"next\"\n");
    assert!(none.dispatch.kinds.is_empty());
    let set = pump(
        "name = \"t\"\n[pump]\nbranch = \"next\"\n\n[pump.dispatch]\ndefault_kind = \"a\"\n\
         [pump.dispatch.kinds.a]\npools = [\"p\"]\n[pump.dispatch.kinds.b]\npools = [\"q\"]\nmax = 2\n",
    );
    assert_eq!(set.dispatch.kinds["a"].pools, ["p"]);
    assert_eq!(set.dispatch.kinds["a"].max, None, "absent max means no cap");
    assert!(!set.dispatch.kinds["a"].skip_proof);
    assert_eq!(set.dispatch.kinds["b"].max, Some(2));
    assert_eq!(set.dispatch.default_kind, "a");
}

#[test]
fn a_minimal_pump_table_takes_every_default() {
    let cfg = pump("name = \"t\"\n[pump]\nbranch = \"next\"\n");
    assert_eq!(cfg.branch, "next");
    assert_eq!(cfg.remote, "origin");
    assert_eq!(cfg.main, "main");
    assert!(cfg.promote);
    assert_eq!(cfg.tick, 20);
    assert_eq!(cfg.shell_prefix, "");
    assert_eq!(cfg.hand_rules, None);
    assert_eq!(cfg.fix_prefix, "pumpfix");
    assert_eq!(cfg.from, "operator");
    assert_eq!(cfg.to, "hand");
    assert_eq!(cfg.reply_from, "mind");
    assert_eq!(cfg.home, None);
    assert_eq!(cfg.repo, None);
    assert!(cfg.board.from.is_empty());
    assert_eq!(cfg.board.since, "");
    assert_eq!(cfg.board.mark, "pump");
    assert_eq!(cfg.board.every, 300);
    assert_eq!(cfg.board.planners_max, 3);
    assert_eq!(cfg.board.plan_timeout, 1800);
    assert_eq!(cfg.board.reply_to, None);
    assert!(cfg.board.pull);
    assert_eq!(cfg.board.push_since, "30m");
    assert_eq!(cfg.planner.run, "");
    assert_eq!(cfg.planner.charter, "");
    assert_eq!(cfg.planner.split_hard_cap, 5400);
    assert!(!cfg.planner.split_timeout_present);
    assert!(cfg.dispatch.lanes.is_empty());
    assert_eq!(cfg.dispatch.last_resort, "");
    assert_eq!(cfg.dispatch.max_attempts, 3);
    assert_eq!(cfg.dispatch.budget, 600);
    assert_eq!(cfg.dispatch.overdue_grace, 900);
    assert_eq!(cfg.dispatch.per_tick, 6);
    assert_eq!(cfg.dispatch.transient_max, 2, "default transient retry cap");
    assert_eq!(
        cfg.dispatch.transient_backoff, 120,
        "default transient backoff seconds"
    );
    assert_eq!(cfg.dispatch.from, None);
    assert!(cfg.dispatch.kinds.is_empty());
    assert_eq!(cfg.dispatch.default_kind, "");
    assert_eq!(cfg.merge.batch, 8);
    assert_eq!(cfg.merge.lanes, 1);
    assert_eq!(cfg.merge.format, "");
    assert!(cfg.merge.shared_checks.is_empty());
    assert_eq!(cfg.merge.proof_timeout, 300);
    assert_eq!(cfg.merge.remote_wait, 90);
    assert_eq!(cfg.merge.remote_limit, 1800);
    assert_eq!(cfg.gate.command, "");
    assert_eq!(cfg.gate.every_merges, 8);
    assert_eq!(cfg.gate.every_seconds, 1800);
    assert!(cfg.gate.pools.is_empty());
    assert_eq!(cfg.gate.model, "");
    assert_eq!(cfg.gate.budget, 1800);
    assert_eq!(cfg.dispatch_from(), "next");
    assert_eq!(cfg.reply_to(), "");
}

#[test]
fn legacy_split_timeout_loads_as_split_hard_cap() {
    let cfg =
        pump("name = \"t\"\n[pump]\nbranch = \"next\"\n\n[pump.planner]\nsplit_timeout = 1800\n");
    assert_eq!(cfg.planner.split_hard_cap, 1800);
    assert!(
        cfg.planner.split_timeout_present,
        "the legacy key's presence must survive parsing"
    );
}

#[test]
fn split_hard_cap_wins_when_both_planner_keys_are_set() {
    let cap_first = pump(
        "name = \"t\"\n[pump]\nbranch = \"next\"\n\n[pump.planner]\nsplit_hard_cap = 2700\n\
         split_timeout = 1800\n",
    );
    assert_eq!(cap_first.planner.split_hard_cap, 2700);
    assert!(cap_first.planner.split_timeout_present);
    let timeout_first = pump(
        "name = \"t\"\n[pump]\nbranch = \"next\"\n\n[pump.planner]\nsplit_timeout = 1800\n\
         split_hard_cap = 2700\n",
    );
    assert_eq!(timeout_first.planner.split_hard_cap, 2700);
    assert!(timeout_first.planner.split_timeout_present);
}

#[test]
fn transient_retry_knobs_parse_and_take_defaults() {
    let set = pump(
        "name = \"t\"\n[pump]\nbranch = \"next\"\n\n[pump.dispatch]\n\
         transient_max = 5\ntransient_backoff = 30\n",
    );
    assert_eq!(set.dispatch.transient_max, 5);
    assert_eq!(set.dispatch.transient_backoff, 30);
    let none = pump("name = \"t\"\n[pump]\nbranch = \"next\"\n");
    assert_eq!(none.dispatch.transient_max, 2);
    assert_eq!(none.dispatch.transient_backoff, 120);
}

#[test]
fn the_full_design_file_block_parses() {
    let mut cfg = pump(&format!("name = \"t\"\n{FULL}"));
    assert_eq!(cfg.branch, "next");
    assert_eq!(cfg.main, "main");
    assert_eq!(cfg.tick, 20);
    assert_eq!(cfg.board.mark, "pump");
    assert_eq!(cfg.board.planners_max, 3);
    assert_eq!(cfg.dispatch.from, Some("next".to_string()));
    assert_eq!(cfg.dispatch_from(), "next");
    assert_eq!(cfg.dispatch.lanes.len(), 2);
    assert_eq!(cfg.dispatch.lanes[0].model, "deepseek/deepseek-flash");
    assert_eq!(cfg.dispatch.lanes[0].cap, 30);
    assert_eq!(cfg.dispatch.lanes[1].model, "zai/glm-5.3-flash");
    assert_eq!(cfg.dispatch.lanes[1].cap, 10);
    assert_eq!(cfg.dispatch.default_kind, "plain");
    assert_eq!(cfg.dispatch.kinds["plain"].pools, ["fly4"]);
    assert_eq!(cfg.dispatch.kinds["full"].max, Some(2));
    assert!(cfg.dispatch.kinds["full"].skip_proof);
    assert_eq!(cfg.merge.batch, 8);
    assert_eq!(cfg.gate.model, "zai/glm-5.3-flash");
    assert_eq!(cfg.reply_to(), "operator@bd371d96.local");
    assert_eq!(cfg.committer_name, "Ian Zepp");
    assert_eq!(cfg.committer_email, "ian@example.com");
    assert_eq!(
        cfg.remote_url.as_deref(),
        Some("git@example.com:org/repo.git")
    );
    cfg.hand_rules = None; // the design path does not exist on the test host
    assert!(
        cfg.validate(Path::new("."), &pools(&["fly4", "flyfull", "flygate"]))
            .is_ok(),
        "the design values should validate"
    );
}

#[test]
fn a_config_round_trips_through_the_mailspace_file_without_losing_a_key() {
    let cfg = pump(&format!("name = \"t\"\n{FULL}"));
    let before = toml::Value::try_from(&cfg).expect("pump config should serialize");
    let mut file = toml::value::Table::new();
    file.insert("name".into(), toml::Value::from("t"));
    file.insert("pump".into(), before.clone());
    let text = toml::to_string(&file).expect("table should serialize");
    let again = toml::Value::try_from(pump(&text)).expect("pump config should serialize");
    assert_eq!(before, again, "a rewrite must not lose or change a key");
}

#[test]
fn named_targets_keep_repository_paths_and_inherit_shared_merge_and_gate_defaults() {
    let mut cfg = PumpConfig {
        merge: crate::pump::config::MergeConfig {
            shared_checks: vec![String::from("cargo check")],
            ..crate::pump::config::MergeConfig::default()
        },
        gate: crate::pump::config::GateConfig {
            command: String::from("./gate"),
            ..crate::pump::config::GateConfig::default()
        },
        ..PumpConfig::default()
    };
    cfg.targets = BTreeMap::from([
        (
            String::from("radix"),
            PumpTarget {
                path: String::from("radix"),
                branch: String::from("next"),
                promote: false,
                ..PumpTarget::default()
            },
        ),
        (
            String::from("tela"),
            PumpTarget {
                path: String::from("tela"),
                branch: String::from("main"),
                promote: false,
                ..PumpTarget::default()
            },
        ),
    ]);
    let targets = cfg.target_configs();
    assert_eq!(targets["radix"].path, "radix");
    assert_eq!(targets["tela"].path, "tela");
    assert_eq!(
        targets["radix"].merge.as_ref().unwrap().shared_checks,
        ["cargo check"]
    );
    assert_eq!(targets["tela"].gate.as_ref().unwrap().command, "./gate");
}

#[test]
fn an_unknown_key_is_ignored() {
    let cfg = pump(concat!(
        "name = \"t\"\n",
        "[pump]\n",
        "branch = \"next\"\n",
        "wibble = 1\n",
        "[pump.board]\n",
        "nope = true\n",
    ));
    assert_eq!(cfg.branch, "next");
    assert_eq!(cfg.board.mark, "pump");
}

#[test]
fn validate_rejects_a_missing_branch_and_names_the_key() {
    let err = PumpConfig::default()
        .validate(Path::new("."), &[])
        .expect_err("empty branch must fail");
    assert!(err.to_string().contains("[pump] branch"), "{err}");
}

#[test]
fn validate_rejects_empty_lanes_and_zero_caps() {
    let err = PumpConfig::default()
        .validate(Path::new("."), &[])
        .expect_err("no lanes must fail");
    assert!(err.to_string().contains("[pump.dispatch] lanes"), "{err}");
    let zero_cap = PumpConfig {
        dispatch: DispatchConfig {
            lanes: vec![Lane {
                model: "m".into(),
                cap: 0,
            }],
            ..DispatchConfig::default()
        },
        ..PumpConfig::default()
    };
    let err = zero_cap
        .validate(Path::new("."), &pools(&["p"]))
        .expect_err("cap 0 must fail");
    assert!(err.to_string().contains("cap >= 1"), "{err}");
    let empty_model = PumpConfig {
        dispatch: DispatchConfig {
            lanes: vec![Lane {
                model: String::new(),
                cap: 1,
            }],
            ..DispatchConfig::default()
        },
        ..PumpConfig::default()
    };
    let err = empty_model
        .validate(Path::new("."), &pools(&["p"]))
        .expect_err("empty model must fail");
    assert!(err.to_string().contains("model"), "{err}");
}

#[test]
fn validate_allows_empty_lanes_when_a_role_assigns_a_pool() {
    let cfg = PumpConfig {
        committer_name: "Vivi".into(),
        committer_email: "vivi@example.invalid".into(),
        branch: "next".into(),
        promote: false,
        board: crate::pump::config::BoardConfig {
            mark: String::new(),
            ..crate::pump::config::BoardConfig::default()
        },
        gate: crate::pump::config::GateConfig {
            command: "./gate".into(),
            ..crate::pump::config::GateConfig::default()
        },
        ..PumpConfig::default()
    };
    assert!(
        cfg.dispatch.lanes.is_empty(),
        "this fixture must omit [pump.dispatch] lanes"
    );
    let err = cfg
        .validate(Path::new("."), &pools(&["glm"]))
        .expect_err("legacy configs still need lanes");
    assert!(err.to_string().contains("[pump.dispatch] lanes"), "{err}");
    let ok = cfg.validate_for(Path::new("."), &pools(&["glm"]), true);
    assert!(
        ok.is_ok(),
        "role→pool capacity comes from the assigned pool: {ok:?}"
    );
}

#[test]
fn validate_rejects_zero_merge_lanes() {
    let cfg = PumpConfig {
        merge: crate::pump::config::MergeConfig {
            lanes: 0,
            ..crate::pump::config::MergeConfig::default()
        },
        ..PumpConfig::default()
    };
    let err = cfg
        .validate(Path::new("."), &pools(&["p"]))
        .expect_err("zero merge lanes must fail");
    assert!(
        err.to_string().contains("[pump.merge] lanes must be >= 1"),
        "{err}"
    );
}

#[test]
fn checks_for_parses_glob_keys_into_command_lists() {
    let cfg = pump(
        "name = \"t\"\n[pump]\nbranch = \"next\"\n\n[pump.merge.checks_for]\n\
         \"corpus/**/*.fab\" = [\"cargo test -p faber --lib explain_\"]\n\
         \"locale/**\" = [\"./locale-check\", \"cargo test -p radix-module locale_pack\"]\n",
    );
    assert_eq!(
        cfg.merge.checks_for,
        BTreeMap::from([
            (
                "corpus/**/*.fab".to_string(),
                vec!["cargo test -p faber --lib explain_".to_string()]
            ),
            (
                "locale/**".to_string(),
                vec![
                    "./locale-check".to_string(),
                    "cargo test -p radix-module locale_pack".to_string()
                ]
            ),
        ])
    );
}

#[test]
fn checks_for_is_empty_by_default() {
    let cfg = pump("name = \"t\"\n[pump]\nbranch = \"next\"\n");
    assert!(cfg.merge.checks_for.is_empty());
}

#[test]
fn validate_rejects_checks_for_with_blank_commands() {
    let blank = PumpConfig {
        merge: crate::pump::config::MergeConfig {
            shared_checks: vec!["./check".into()],
            checks_for: BTreeMap::from([("src/**".to_string(), vec![" ".into()])]),
            ..crate::pump::config::MergeConfig::default()
        },
        ..PumpConfig::default()
    };
    let err = blank
        .validate(Path::new("."), &pools(&["p"]))
        .expect_err("a blank checks_for command must fail");
    assert!(
        err.to_string()
            .contains("[pump.merge.checks_for] 'src/**' must have non-empty commands"),
        "{err}"
    );
    let no_commands = PumpConfig {
        merge: crate::pump::config::MergeConfig {
            shared_checks: vec!["./check".into()],
            checks_for: BTreeMap::from([("src/**".to_string(), Vec::new())]),
            ..crate::pump::config::MergeConfig::default()
        },
        ..PumpConfig::default()
    };
    let err = no_commands
        .validate(Path::new("."), &pools(&["p"]))
        .expect_err("an empty checks_for list must fail");
    assert!(
        err.to_string()
            .contains("[pump.merge.checks_for] 'src/**' must have non-empty commands"),
        "{err}"
    );
    let mut target_cfg = valid();
    target_cfg.targets = BTreeMap::from([(
        String::from("tela"),
        PumpTarget {
            path: String::from("tela"),
            branch: String::from("main"),
            promote: false,
            merge: Some(crate::pump::config::MergeConfig {
                checks_for: BTreeMap::from([("src/**".to_string(), vec![" ".into()])]),
                ..crate::pump::config::MergeConfig::default()
            }),
            gate: Some(crate::pump::config::GateConfig {
                command: String::from("./gate"),
                ..crate::pump::config::GateConfig::default()
            }),
            ..PumpTarget::default()
        },
    )]);
    let err = target_cfg
        .validate(Path::new("."), &pool_names())
        .expect_err("a target-level blank checks_for command must fail");
    assert!(
        err.to_string()
            .contains("[pump.targets.tela.merge.checks_for] 'src/**' must have non-empty commands"),
        "{err}"
    );
}

#[test]
fn validate_checks_default_kind_and_kind_pools() {
    let mut cfg = PumpConfig {
        branch: "next".into(),
        committer_name: "Test".into(),
        committer_email: "test@example.com".into(),
        board: crate::pump::config::BoardConfig {
            mark: String::new(),
            ..Default::default()
        },
        dispatch: DispatchConfig {
            lanes: vec![Lane {
                model: "m".into(),
                cap: 1,
            }],
            kinds: BTreeMap::from([("a".to_string(), kind("p"))]),
            ..DispatchConfig::default()
        },
        ..PumpConfig::default()
    };
    cfg.gate.command = "./gate".into();
    cfg.merge.shared_checks = vec!["./check".into()];
    let all = pools(&["p"]);
    let err = cfg.validate(Path::new("."), &all).expect_err("no default");
    assert!(
        err.to_string().contains("default_kind must be set"),
        "{err}"
    );
    cfg.dispatch.default_kind = "missing".into();
    let err = cfg.validate(Path::new("."), &all).expect_err("undefined");
    assert!(
        err.to_string()
            .contains("default_kind names undefined kind"),
        "{err}"
    );
    cfg.dispatch.default_kind = "a".into();
    assert!(cfg.validate(Path::new("."), &all).is_ok());
    cfg.dispatch.kinds.get_mut("a").unwrap().max = Some(0);
    let err = cfg.validate(Path::new("."), &all).expect_err("max 0");
    assert!(err.to_string().contains("max must be >= 1"), "{err}");
    cfg.dispatch.kinds.get_mut("a").unwrap().max = None;
    cfg.dispatch.kinds.insert("b".into(), kind(""));
    let err = cfg.validate(Path::new("."), &all).expect_err("empty pool");
    assert!(
        err.to_string().contains("kinds.b] pools must not be empty"),
        "{err}"
    );
}

#[test]
fn validate_rejects_an_unknown_pool_in_a_multi_pool_kind() {
    let cfg = PumpConfig {
        branch: "next".into(),
        dispatch: DispatchConfig {
            lanes: vec![Lane {
                model: "m".into(),
                cap: 1,
            }],
            kinds: BTreeMap::from([(
                "a".to_string(),
                Kind {
                    pools: vec!["p".into(), "nope".into()],
                    ..Kind::default()
                },
            )]),
            default_kind: "a".into(),
            ..DispatchConfig::default()
        },
        ..PumpConfig::default()
    };
    let err = cfg
        .validate(Path::new("."), &pools(&["p"]))
        .expect_err("unknown pool must fail");
    assert!(err.to_string().contains("unknown pool `nope`"), "{err}");
}

#[test]
fn a_kind_parses_a_multi_pool_list() {
    let set = pump(
        "name = \"t\"\n[pump]\nbranch = \"next\"\n[pump.dispatch]\ndefault_kind = \"a\"\n\
         [pump.dispatch.kinds.a]\npools = [\"p\", \"q\"]\n",
    );
    assert_eq!(set.dispatch.kinds["a"].pools, ["p", "q"]);
}

#[test]
fn kind_of_resolves_tools_or_the_default_kind() {
    let cfg = PumpConfig {
        dispatch: DispatchConfig {
            kinds: BTreeMap::from([("a".to_string(), kind("p")), ("b".to_string(), kind("q"))]),
            default_kind: "a".into(),
            ..DispatchConfig::default()
        },
        ..PumpConfig::default()
    };
    assert_eq!(cfg.kind_of("").unwrap().pools, ["p"]);
    assert_eq!(cfg.kind_of("b").unwrap().pools, ["q"]);
    assert!(cfg.kind_of("nope").is_none());
}

#[test]
fn validate_rejects_an_unknown_pool_name() {
    let mut cfg = PumpConfig {
        branch: "next".into(),
        dispatch: DispatchConfig {
            lanes: vec![Lane {
                model: "m".into(),
                cap: 1,
            }],
            kinds: BTreeMap::from([("a".to_string(), kind("fly"))]),
            default_kind: "a".into(),
            ..DispatchConfig::default()
        },
        ..PumpConfig::default()
    };
    let err = cfg
        .validate(Path::new("."), &pools(&["other"]))
        .expect_err("unknown pool must fail");
    assert!(
        err.to_string()
            .contains("[pump.dispatch.kinds.a] pools names unknown pool `fly`"),
        "{err}"
    );
    cfg.dispatch.kinds.clear();
    cfg.dispatch.default_kind = String::new();
    cfg.gate.pools = vec!["flygate".into()];
    let err = cfg
        .validate(Path::new("."), &pools(&["other"]))
        .expect_err("unknown pool must fail");
    assert!(err.to_string().contains("[pump.gate] pools"), "{err}");
    cfg.gate.pools = vec!["flygate".into(), "flyfull".into()];
    cfg.board.mark = String::new(); // not under test here
    cfg.committer_name = "Test".into();
    cfg.committer_email = "test@example.com".into();
    cfg.gate.command = "./gate".into();
    cfg.merge.shared_checks = vec!["./check".into()];
    assert!(
        cfg.validate(Path::new("."), &pools(&["flygate", "flyfull"]))
            .is_ok()
    );
}

/// A config that passes `validate` once the pool names are supplied.
fn valid() -> PumpConfig {
    let mut cfg = pump(&format!("name = \"t\"\n{FULL}"));
    cfg.hand_rules = None;
    cfg
}

fn pool_names() -> Vec<String> {
    pools(&["fly4", "flyfull", "flygate"])
}

#[test]
fn validate_rejects_an_empty_gate_command_even_without_promotion() {
    let mut cfg = valid();
    cfg.gate.command = String::new();
    let err = cfg
        .validate(Path::new("."), &pool_names())
        .expect_err("empty gate");
    assert!(err.to_string().contains("[pump.gate] command"), "{err}");
    cfg.promote = false;
    let err = cfg
        .validate(Path::new("."), &pool_names())
        .expect_err("empty gate");
    assert!(err.to_string().contains("[pump.gate] command"), "{err}");
}

#[test]
fn validate_rejects_empty_shared_checks_only_while_promotion_is_enabled() {
    let mut cfg = valid();
    cfg.merge.shared_checks = Vec::new();
    let err = cfg
        .validate(Path::new("."), &pool_names())
        .expect_err("no checks");
    assert!(
        err.to_string().contains("[pump.merge] shared_checks"),
        "{err}"
    );
    cfg.promote = false;
    assert!(cfg.validate(Path::new("."), &pool_names()).is_ok());
    cfg.promote = true;
    cfg.main = String::new();
    assert!(cfg.validate(Path::new("."), &pool_names()).is_ok());
}

#[test]
fn validate_rejects_a_missing_committer_identity() {
    let mut cfg = valid();
    cfg.committer_name = String::new();
    let err = cfg
        .validate(Path::new("."), &pool_names())
        .expect_err("no name");
    assert!(err.to_string().contains("committer_name"), "{err}");
    cfg.committer_name = "Ian".into();
    cfg.committer_email = " ".into();
    let err = cfg
        .validate(Path::new("."), &pool_names())
        .expect_err("no email");
    assert!(err.to_string().contains("committer_email"), "{err}");
}

#[test]
fn validate_rejects_an_unreadable_hand_rules_file_and_resolves_relative_to_project() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = valid();
    cfg.hand_rules = Some("rules.md".into());
    let err = cfg
        .validate(dir.path(), &pool_names())
        .expect_err("absent rules file");
    assert!(err.to_string().contains("hand_rules"), "{err}");
    std::fs::write(dir.path().join("rules.md"), "rules").unwrap();
    assert!(cfg.validate(dir.path(), &pool_names()).is_ok());
    assert_eq!(
        cfg.hand_rules_path(dir.path()),
        Some(dir.path().join("rules.md"))
    );
}

#[test]
fn home_and_repo_default_under_the_project_and_follow_overrides() {
    let project = std::env::temp_dir().join("vivi-config-test-project");
    let cfg = PumpConfig::default();
    assert_eq!(cfg.home_dir(&project), project.join(".vivi").join("pump"));
    assert_eq!(
        cfg.repo_dir(&project),
        project.join(".vivi").join("pump").join("repo")
    );
    let mut over = PumpConfig {
        home: Some("pump-home".into()),
        ..PumpConfig::default()
    };
    assert_eq!(over.home_dir(&project), project.join("pump-home"));
    assert_eq!(
        over.repo_dir(&project),
        project.join("pump-home").join("repo")
    );
    over.home = Some("/tmp/pump-abs".into());
    assert_eq!(over.home_dir(&project), Path::new("/tmp/pump-abs"));
    over.home = Some("~/.pump".into());
    assert_eq!(over.home_dir(&project), project.join("~").join(".pump"));
    over.repo = Some("pump-repo".into());
    assert_eq!(over.repo_dir(&project), project.join("pump-repo"));
    over.repo = Some("/tmp/repo-abs".into());
    assert_eq!(over.repo_dir(&project), Path::new("/tmp/repo-abs"));
}

#[test]
fn reserved_file_defaults_to_reserved_txt_under_home() {
    let project = Path::new("/p");
    let cfg = PumpConfig::default();
    assert_eq!(cfg.reserved_file, "reserved.txt");
    assert_eq!(
        cfg.reserved_path(project),
        project.join(".vivi/pump/reserved.txt")
    );
    let abs = PumpConfig {
        reserved_file: "/x/held.txt".into(),
        ..PumpConfig::default()
    };
    assert_eq!(abs.reserved_path(project), Path::new("/x/held.txt"));
}

#[test]
fn max_replans_defaults_to_two() {
    assert_eq!(PumpConfig::default().gate.max_replans, 2);
}

#[test]
fn the_retired_priority_file_key_still_parses_and_is_not_written_back() {
    let cfg: PumpConfig = toml::from_str(
        "branch = \"b\"\ncommitter_name = \"n\"\ncommitter_email = \"e\"\npriority_file = \"priority.txt\"\n",
    )
    .unwrap();
    assert_eq!(cfg.retired_priority_file.as_deref(), Some("priority.txt"));
    assert!(!toml::to_string(&cfg).unwrap().contains("priority_file"));
}

#[test]
fn the_alarm_table_carries_the_blocking_thresholds() {
    let cfg: AlarmConfig =
        toml::from_str("blocking_hold_units = 3\nblocking_replan_minutes = 30\n").unwrap();
    assert_eq!(cfg.blocking_hold_units, 3);
    assert_eq!(cfg.blocking_replan_minutes, 30);
}

#[test]
fn the_blocking_thresholds_default_to_ten_units_and_two_hours() {
    let cfg = AlarmConfig::default();
    assert_eq!(cfg.blocking_hold_units, 10);
    assert_eq!(cfg.blocking_replan_minutes, 120);
}

#[test]
fn a_generated_table_entry_parses_paths_regenerate_and_drift() {
    let cfg = pump(concat!(
        "name = \"t\"\n[pump]\nbranch = \"next\"\n",
        "[[pump.merge.generated]]\n",
        "paths = [\"out/one.json\", \"out/two.json\"]\n",
        "regenerate = \"./scripta/gen --all\"\n",
        "drift = [\"generated files are stale\"]\n",
    ));
    let entry = &cfg.merge.generated[0];
    assert_eq!(entry.paths, ["out/one.json", "out/two.json"]);
    assert_eq!(entry.regenerate, "./scripta/gen --all");
    assert_eq!(entry.drift, ["generated files are stale"]);
    assert_eq!(cfg.merge.generated_paths(), entry.paths);
}

#[test]
fn an_inline_generated_array_parses_like_the_table_form() {
    let cfg = pump(concat!(
        "name = \"t\"\n[pump]\nbranch = \"next\"\n",
        "[pump.merge]\n",
        "generated = [{ paths = [\"out/a.json\"], regenerate = \"./gen\" }]\n",
    ));
    assert_eq!(cfg.merge.generated_paths(), ["out/a.json"]);
    assert_eq!(cfg.merge.generated[0].regenerate, "./gen");
    assert!(cfg.merge.generated[0].drift.is_empty());
}

#[test]
fn generated_defaults_to_no_declarations() {
    let cfg = PumpConfig::default();
    assert!(cfg.merge.generated.is_empty());
    assert!(cfg.merge.generated_paths().is_empty());
}

#[test]
fn generated_paths_concatenates_every_entry() {
    let cfg = pump(concat!(
        "name = \"t\"\n[pump]\nbranch = \"next\"\n",
        "[[pump.merge.generated]]\npaths = [\"a.json\"]\nregenerate = \"./a\"\n",
        "[[pump.merge.generated]]\npaths = [\"b.json\", \"c.json\"]\nregenerate = \"./b\"\n",
    ));
    assert_eq!(
        cfg.merge.generated_paths(),
        ["a.json", "b.json", "c.json"],
        "declaration order, every entry"
    );
}

#[test]
fn generated_paths_for_reads_the_workspace_table_for_the_legacy_default_target() {
    let cfg = pump(concat!(
        "name = \"t\"\n[pump]\nbranch = \"next\"\n",
        "[[pump.merge.generated]]\npaths = [\"ledger.json\"]\nregenerate = \"./gen\"\n",
    ));
    assert!(cfg.targets.is_empty());
    assert_eq!(cfg.generated_paths_for("default"), ["ledger.json"]);
    assert_eq!(cfg.generated_paths_for("tela"), ["ledger.json"]);
}

#[test]
fn generated_paths_for_reads_a_named_targets_own_table() {
    let cfg = pump(concat!(
        "name = \"t\"\n[pump]\nbranch = \"next\"\n",
        "committer_name = \"n\"\ncommitter_email = \"e\"\n",
        "[[pump.merge.generated]]\npaths = [\"shared.json\"]\nregenerate = \"./gen\"\n",
        "[pump.targets.tela]\npath = \"tela\"\nbranch = \"main\"\n",
        "[[pump.targets.tela.merge.generated]]\npaths = [\"tela-out.json\"]\nregenerate = \"./tela-gen\"\n",
        "[pump.targets.radix]\npath = \"radix\"\nbranch = \"main\"\n",
    ));
    assert_eq!(cfg.generated_paths_for("tela"), ["tela-out.json"]);
    assert_eq!(
        cfg.generated_paths_for("radix"),
        ["shared.json"],
        "a target without its own table inherits the workspace table"
    );
    assert!(
        cfg.generated_paths_for("nope").is_empty(),
        "unknown target has no generated paths"
    );
}

#[test]
fn validate_rejects_a_generated_entry_with_empty_paths() {
    let mut cfg = valid();
    cfg.merge.generated = vec![crate::pump::config::Generated {
        regenerate: "./gen".into(),
        ..crate::pump::config::Generated::default()
    }];
    let err = cfg
        .validate(Path::new("."), &pool_names())
        .expect_err("empty paths must fail");
    assert!(
        err.to_string().contains("[[pump.merge.generated]]"),
        "{err}"
    );
}

#[test]
fn validate_rejects_a_generated_entry_with_empty_regenerate() {
    let mut cfg = valid();
    cfg.merge.generated = vec![crate::pump::config::Generated {
        paths: vec!["out/a.json".into()],
        regenerate: " ".into(),
        ..crate::pump::config::Generated::default()
    }];
    let err = cfg
        .validate(Path::new("."), &pool_names())
        .expect_err("empty regenerate must fail");
    assert!(
        err.to_string()
            .contains("[[pump.merge.generated]] regenerate"),
        "{err}"
    );
}

#[test]
fn validate_rejects_a_target_level_generated_entry_with_empty_regenerate() {
    let mut cfg = valid();
    cfg.targets = BTreeMap::from([(
        String::from("tela"),
        PumpTarget {
            path: String::from("tela"),
            branch: String::from("main"),
            promote: false,
            merge: Some(crate::pump::config::MergeConfig {
                generated: vec![crate::pump::config::Generated {
                    paths: vec!["out/a.json".into()],
                    ..crate::pump::config::Generated::default()
                }],
                ..crate::pump::config::MergeConfig::default()
            }),
            gate: Some(crate::pump::config::GateConfig {
                command: String::from("./gate"),
                ..crate::pump::config::GateConfig::default()
            }),
            ..PumpTarget::default()
        },
    )]);
    let err = cfg
        .validate(Path::new("."), &pool_names())
        .expect_err("empty regenerate must fail");
    assert!(
        err.to_string()
            .contains("[pump.targets.tela.merge] generated"),
        "{err}"
    );
}

#[test]
fn pool_sources_map_local_worktree_pools() {
    let config = mailspace(
        "name = \"t\"\n\n[pools.wtree]\nprovider = \"local\"\nslots = 2\n\n\
         [pools.wtree.local]\nworktree_of = \"/wtree/repo\"\n\n\
         [pools.bare]\nprovider = \"local\"\nslots = 1\n\n\
         [pools.flyone]\nprovider = \"fly\"\nslots = 1\n",
    );
    let sources = PumpConfig::default().pool_sources(&config, Path::new("/proj"));
    assert_eq!(
        sources.get("wtree"),
        Some(&std::path::PathBuf::from("/wtree/repo")),
        "a local worktree pool commits into its worktree repository"
    );
    assert_eq!(
        sources.get("bare"),
        Some(&std::path::PathBuf::from("/proj")),
        "a local pool without a worktree repository commits into the project"
    );
    assert!(!sources.contains_key("flyone"), "a remote pool has no path");

    let mut targets = BTreeMap::new();
    targets.insert(String::from("tela"), PumpTarget::default());
    let multi = PumpConfig {
        targets,
        ..PumpConfig::default()
    };
    assert!(
        multi.pool_sources(&config, Path::new("/proj")).is_empty(),
        "a multi-target pump reads from per-target integration clones"
    );
}
