//! Tests for [`crate::pump::config`].

use crate::mailspace::MailspaceConfig;
use crate::pump::config::{DispatchConfig, Kind, PumpConfig, PumpTarget};
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
format        = "cargo fmt --all"
remote_limit  = 1800
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
    assert_eq!(cfg.remote, "origin");
    assert_eq!(cfg.main, "main");
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
    assert_eq!(cfg.board.retired_since, None);
    assert_eq!(cfg.board.retired_mark, None);
    assert_eq!(cfg.board.every, 300);
    assert_eq!(cfg.board.planners_max, 3);
    assert_eq!(cfg.board.plan_timeout, 1800);
    assert_eq!(cfg.board.reply_to, None);
    assert!(cfg.board.pull);
    assert_eq!(cfg.board.push_since, "30m");
    assert_eq!(cfg.planner.run, "");
    assert_eq!(cfg.planner.charter, "");
    assert_eq!(cfg.planner.split_hard_cap, 5400);
    assert_eq!(cfg.planner.planners_max, 3);
    assert!(cfg.dispatch.lanes.is_empty());
    assert_eq!(cfg.dispatch.last_resort, "");
    assert_eq!(cfg.dispatch.max_attempts, 3);
    assert_eq!(cfg.dispatch.budget, 600);
    assert_eq!(cfg.dispatch.overdue_grace, 900);
    assert_eq!(cfg.dispatch.per_tick, 6);
    assert_eq!(cfg.dispatch.parallel, 6, "default parallel dispatches");
    assert_eq!(cfg.dispatch.transient_max, 2, "default transient retry cap");
    assert_eq!(
        cfg.dispatch.transient_backoff, 120,
        "default transient backoff seconds"
    );
    assert_eq!(cfg.dispatch.from, None);
    assert!(cfg.dispatch.kinds.is_empty());
    assert_eq!(cfg.dispatch.default_kind, "");
    assert_eq!(cfg.merge.format, "");
    assert_eq!(cfg.merge.remote_limit, 1800);
    assert_eq!(cfg.dispatch_from(), "main");
    assert_eq!(cfg.reply_to(), "");
}

#[test]
fn planner_planners_max_reads_toml_and_defaults_to_3() {
    let set = pump("name = \"t\"\n[pump]\nbranch = \"next\"\n\n[pump.planner]\nplanners_max = 5\n");
    assert_eq!(set.planner.planners_max, 5);
    let none = pump("name = \"t\"\n[pump]\nbranch = \"next\"\n");
    assert_eq!(none.planner.planners_max, 3);
}

#[test]
fn pump_proof_allow_key_parses_and_defaults_empty() {
    let configured = pump(
        "name = \"t\"\n[pump]\nbranch = \"next\"\n\n[pump.proof]\nallow = [\"faber\", \"python3.11\"]\n",
    );
    assert_eq!(configured.proof.allow, ["faber", "python3.11"]);
    let defaulted = pump("name = \"t\"\n[pump]\nbranch = \"next\"\n");
    assert!(defaulted.proof.allow.is_empty());
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
    assert_eq!(cfg.main, "main");
    assert_eq!(cfg.tick, 20);
    assert_eq!(cfg.board.retired_mark, None);
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
    assert_eq!(cfg.merge.format, "cargo fmt --all");
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
fn named_targets_keep_repository_paths_and_inherit_shared_merge_defaults() {
    let mut cfg = PumpConfig {
        merge: crate::pump::config::MergeConfig {
            format: String::from("cargo fmt"),
            ..crate::pump::config::MergeConfig::default()
        },
        ..PumpConfig::default()
    };
    cfg.targets = BTreeMap::from([
        (
            String::from("radix"),
            PumpTarget {
                path: String::from("radix"),
                ..PumpTarget::default()
            },
        ),
        (
            String::from("tela"),
            PumpTarget {
                path: String::from("tela"),
                ..PumpTarget::default()
            },
        ),
    ]);
    let targets = cfg.target_configs();
    assert_eq!(targets["radix"].path, "radix");
    assert_eq!(targets["tela"].path, "tela");
    assert_eq!(targets["radix"].merge.as_ref().unwrap().format, "cargo fmt");
    assert_eq!(targets["tela"].merge.as_ref().unwrap().format, "cargo fmt");
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
    assert_eq!(cfg.board.retired_mark, None);
}

#[test]
fn parallel_parses_and_must_be_at_least_one() {
    let cfg: PumpConfig = toml::from_str("[dispatch]\nparallel = 2\n").unwrap();
    assert_eq!(cfg.dispatch.parallel, 2);
    let mut zero = PumpConfig::default();
    zero.dispatch.parallel = 0;
    let err = zero
        .validate(Path::new("."), &pools(&["p"]))
        .expect_err("zero parallel must fail");
    assert!(
        err.to_string()
            .contains("[pump.dispatch] parallel must be >= 1"),
        "{err}"
    );
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
fn a_retired_pump_gate_table_still_parses_and_is_not_written_back() {
    let cfg = pump(
        "name = \"t\"\n[pump]\nbranch = \"next\"\n[pump.gate]\ncommand = \"./gate\"\n\
         [pump.targets.app]\npath = \"app\"\n[pump.targets.app.gate]\ncommand = \"./gate\"\n",
    );
    let text = toml::to_string(&cfg).unwrap();
    assert!(!text.contains("gate"), "{text}");
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
            merge: Some(crate::pump::config::MergeConfig {
                generated: vec![crate::pump::config::Generated {
                    paths: vec!["out/a.json".into()],
                    ..crate::pump::config::Generated::default()
                }],
                ..crate::pump::config::MergeConfig::default()
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

#[test]
fn findings_report_absent_items_apart_from_wrong_values() {
    let found = PumpConfig::default().findings_for(Path::new("."), &[]);
    assert!(found.errors.is_empty(), "{:?}", found.errors);
    let names: Vec<&str> = found
        .missing
        .iter()
        .map(|item| item.name.as_str())
        .collect();
    for expected in ["pump.committer_name", "pump.committer_email"] {
        assert!(names.contains(&expected), "{expected} not in {names:?}");
    }
}

#[test]
fn findings_keep_wrong_values_as_errors_and_report_nothing_missing() {
    let mut cfg = valid();
    cfg.tick = 0;
    cfg.dispatch.parallel = 0;
    let found = cfg.findings_for(Path::new("."), &pool_names());
    assert!(found.missing.is_empty(), "{:?}", found.missing);
    assert_eq!(found.errors.len(), 2, "{:?}", found.errors);
    assert!(found.errors.iter().any(|e| e.contains("tick")));
    assert!(found.errors.iter().any(|e| e.contains("parallel")));
}

#[test]
fn findings_for_a_complete_config_are_clean() {
    let found = valid().findings_for(Path::new("."), &pool_names());
    assert!(found.is_clean(), "{found:?}");
}

#[test]
fn a_target_with_only_a_path_reports_no_gap() {
    let mut cfg = valid();
    cfg.targets.insert(
        String::from("app"),
        PumpTarget {
            path: String::from("app"),
            ..PumpTarget::default()
        },
    );
    let found = cfg.findings_for(Path::new("."), &pool_names());
    assert!(found.errors.is_empty(), "{:?}", found.errors);
    let names: Vec<&str> = found
        .missing
        .iter()
        .map(|item| item.name.as_str())
        .collect();
    assert!(
        !names
            .iter()
            .any(|name| name.starts_with("pump.targets.app")),
        "{names:?}"
    );
}

/// A config that validates clean apart from what the caller changes.
fn valid_pump(extra: &str) -> PumpConfig {
    pump(&format!(
        "name = \"t\"\n[pump]\nbranch = \"next\"\ncommitter_name = \"Test\"\n\
         committer_email = \"test@example.com\"\npromote = false\n\
         [pump.dispatch]\nlanes = [{{ model = \"m\", cap = 1 }}]\n\
         {extra}"
    ))
}

#[test]
fn a_board_table_without_the_retired_keys_validates() {
    let cfg = valid_pump("[pump.board]\nevery = 60\n");
    cfg.validate(Path::new("."), &pools(&[]))
        .expect("no retired key is set");
}

#[test]
fn the_retired_board_mark_is_an_error_naming_the_replacement() {
    for value in ["\"pump\"", "\"\""] {
        let cfg = valid_pump(&format!("[pump.board]\nmark = {value}\n"));
        let err = cfg
            .validate(Path::new("."), &pools(&[]))
            .expect_err("a present mark must not be silently ignored")
            .to_string();
        assert!(err.contains("[pump.board] mark is retired"), "{err}");
        assert!(err.contains("vivi pump send --need <handle>"), "{err}");
    }
}

#[test]
fn the_retired_board_since_is_an_error_naming_the_replacement() {
    let cfg = valid_pump("[pump.board]\nsince = \"2026-10-14T00:10\"\n");
    let err = cfg
        .validate(Path::new("."), &pools(&[]))
        .expect_err("a present since must not be silently ignored")
        .to_string();
    assert!(err.contains("[pump.board] since is retired"), "{err}");
    assert!(err.contains("vivi pump send --need <handle>"), "{err}");
}

#[test]
fn the_retired_board_keys_are_never_written_back() {
    let cfg = valid_pump("[pump.board]\nmark = \"pump\"\nsince = \"2026-10-14\"\n");
    let text = toml::to_string(&cfg).unwrap();
    for key in ["mark", "since"] {
        assert!(
            text.lines().all(|line| !line.trim_start().starts_with(key)),
            "{key} was written back: {text}"
        );
    }
}

/// `config validate` refuses these keys, but the pump must keep starting on a
/// file that still carries them: the file parses and the pump's own validation
/// passes.
#[test]
fn a_pump_table_carrying_keys_validate_refuses_still_parses_and_validates() {
    let cfg = pump(
        "name = \"t\"\n[pump]\ncommitter_name = \"P\"\ncommitter_email = \"p@example.com\"\n\
         reserved_file = \"r.txt\"\nfix_prefix = \"fx\"\n\
         [pump.planner]\nrun = \"x\"\ncharter = \"c.md\"\n\
         [pump.proof]\nallow = [\"cargo\"]\n\
         [pump.alarm]\nmerge_idle_minutes = 5\n\
         [pump.board]\nplanners_max = 2\nplan_timeout = 60\n\
         [pump.dispatch]\nmax_attempts = 2\nper_tick = 3\ntransient_max = 1\n\
         transient_backoff = 10\noverdue_grace = 30\nlast_resort = \"m\"\n\
         [[pump.dispatch.lanes]]\nmodel = \"standin\"\ncap = 1\n\
         [pump.merge]\nrepair_budget = 7\nformat = \"cargo fmt\"\n\
         [pump.targets.other]\npath = \"other\"\n[pump.targets.other.merge]\nrepair_budget = 3\n",
    );
    assert_eq!(cfg.merge.format, "cargo fmt");
    cfg.validate(Path::new("."), &[])
        .expect("the pump's own validation does not read the refused keys");
}
