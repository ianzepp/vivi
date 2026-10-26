//! `vivi config`, `vivi pool` writers, and `vivi role pool`.

use std::fs;
use std::process::{Command, Output};

use tempfile::tempdir;

fn vivi(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(output),
        stderr(output)
    );
}

fn project_with(toml: &str) -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    let vivi = dir.path().join(".vivi");
    fs::create_dir_all(&vivi).unwrap();
    fs::write(vivi.join("mailspace.toml"), toml).unwrap();
    dir
}

fn faber_project() -> tempfile::TempDir {
    let raw = include_str!("fixtures/faber-mailspace.toml");
    project_with(raw)
}

#[test]
fn config_validate_and_show_carry_no_legacy_or_gate_routing() {
    let dir = project_with(
        r#"
name = "demo"
id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"

[[identities]]
name = "hand"
kind = "hand"
pool = "glm"
model = "P2-S0"

[[identities]]
name = "merge"
kind = "merge"
pool = "glm"
model = "P2-S0"

[[identities]]
name = "operator"
kind = "operator"

[[identities]]
name = "mind"
kind = "mind"

[[identities]]
name = "pump-planner"
kind = "planner"
pool = "glm"
model = "P2-S0"

[[identities]]
name = "auditor"
kind = "auditor"
pool = "glm"
model = "P2-S0"

[[identities]]
name = "researcher"
kind = "researcher"
pool = "glm"
model = "P2-S0"

[[identities]]
name = "admitter"
kind = "admitter"
pool = "glm"
model = "P2-S0"

[[identities]]
name = "slicer"
kind = "slicer"
pool = "glm"
model = "P2-S0"

[pools.glm]
provider = "local"
slots = 2

[pools.glm.harness]
name = "fake"
run = "fake --model {model} @{brief}"
models = ["m/ok"]

[pools.glm.bands]
P2 = [{ model = "m/ok", reasoning = "high" }]

[pump]
to = "hand"
committer_name = "t"
committer_email = "t@t"
[pump.rungs]
task = "true"
need = "true"
main = "true"
pools = ["glm"]
"#,
    );
    let root = dir.path().to_str().unwrap();
    let validate = vivi(&["config", "validate", "--project", root]);
    assert_success(&validate);
    let text = stdout(&validate);
    assert!(!text.contains("legacy"), "{text}");
    assert!(!text.contains("gate"), "{text}");

    let show = vivi(&["config", "show", "--porcelain", "--project", root]);
    assert_success(&show);
    let shown = stdout(&show);
    assert!(shown.contains("role hand"), "{shown}");
    assert!(!shown.contains("pump.dispatch.kinds"), "{shown}");
    assert!(!shown.contains("gate"), "{shown}");
    let flow = shown
        .lines()
        .find(|line| line.starts_with("flow "))
        .unwrap_or_default();
    assert_eq!(flow.split_whitespace().count(), 5, "{shown}");
}

#[test]
fn config_validate_fails_on_unknown_pool() {
    let dir = project_with(
        r#"
name = "demo"
id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
[[identities]]
name = "hand"
kind = "hand"
pool = "ghost"
[pump]
to = "hand"
"#,
    );
    let root = dir.path().to_str().unwrap();
    let out = vivi(&["config", "validate", "--project", root]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("unknown pool") || stdout(&out).contains("unknown pool"),
        "stdout={} stderr={}",
        stdout(&out),
        stderr(&out)
    );
}

#[test]
fn config_validate_fails_on_a_pool_every_pool_command_refuses() {
    for models in ["[\"\"]", "[\"~deepseek/flash\"]"] {
        let dir = project_with(&format!(
            r#"
name = "demo"
id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
[[identities]]
name = "hand"
kind = "hand"
pool = "p"
[pools.p]
provider = "local"
slots = 1
[pools.p.local]
worktree_of = "/tmp/t"
[pools.p.harness]
name = "fake"
run = "fake --model {{model}} @{{brief}}"
models = {models}
[pump]
to = "hand"
"#
        ));
        let root = dir.path().to_str().unwrap();
        let out = vivi(&["config", "validate", "--project", root]);
        assert!(!out.status.success(), "{models}: {}", stdout(&out));
        let text = format!("{}{}", stdout(&out), stderr(&out));
        assert!(
            text.contains("[pools.p] harness.models holds an empty slug"),
            "{models}: {text}"
        );
        // The same table the pool commands refuse.
        let status = vivi(&["pool", "status", "--project", root]);
        assert!(!status.status.success(), "{models}");
    }
}

#[test]
fn role_and_pool_writers_keep_comments() {
    let dir = project_with(
        r#"
# keep-me
name = "demo"
id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"

[[identities]]
name = "hand"
kind = "hand"
"#,
    );
    let root = dir.path().to_str().unwrap();
    assert_success(&vivi(&[
        "pool",
        "add",
        "glm",
        "--provider",
        "local",
        "--slots",
        "4",
        "--project",
        root,
    ]));
    assert_success(&vivi(&["role", "pool", "hand", "glm", "--project", root]));
    assert_success(&vivi(&[
        "role",
        "set",
        "hand",
        "--read-only",
        "false",
        "--project",
        root,
    ]));
    let raw = fs::read_to_string(dir.path().join(".vivi/mailspace.toml")).unwrap();
    assert!(raw.contains("# keep-me"), "{raw}");
    assert!(raw.contains("pool = \"glm\""), "{raw}");
    assert!(raw.contains("[pools.glm]"), "{raw}");
    assert!(raw.contains("read_only = false"), "{raw}");
}

#[test]
fn pool_remove_refuses_a_referenced_pool() {
    let dir = project_with(
        r#"
name = "demo"
id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
[[identities]]
name = "hand"
kind = "hand"
pool = "glm"
[pools.glm]
provider = "local"
slots = 2
"#,
    );
    let root = dir.path().to_str().unwrap();
    let refused = vivi(&["pool", "remove", "glm", "--yes", "--project", root]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("still referenced"),
        "{}",
        stderr(&refused)
    );
    let config = dir.path().join(".vivi/mailspace.toml");
    let before = fs::read_to_string(&config).unwrap();
    let forced = vivi(&[
        "pool",
        "remove",
        "glm",
        "--yes",
        "--force",
        "--project",
        root,
    ]);
    assert!(!forced.status.success());
    let err = stderr(&forced);
    assert!(err.contains("rolled back"), "{err}");
    assert!(err.contains("names unknown pool `glm`"), "{err}");
    let raw = fs::read_to_string(&config).unwrap();
    assert!(raw.contains("[pools.glm]"), "{raw}");
    assert_eq!(raw, before);
    fs::write(&config, before.replace("pool = \"glm\"\n", "")).unwrap();
    assert_success(&vivi(&[
        "pool",
        "remove",
        "glm",
        "--yes",
        "--project",
        root,
    ]));
    let raw = fs::read_to_string(&config).unwrap();
    assert!(!raw.contains("[pools.glm]"), "{raw}");
}

#[test]
fn pool_set_of_a_value_that_breaks_loading_is_rolled_back() {
    let dir = project_with(
        r#"
name = "demo"
id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
[[identities]]
name = "hand"
kind = "hand"
pool = "glm"
[pools.glm]
provider = "local"
slots = 2
"#,
    );
    let root = dir.path().to_str().unwrap();
    let config = dir.path().join(".vivi/mailspace.toml");
    let before = fs::read_to_string(&config).unwrap();
    let out = vivi(&["pool", "set", "glm", "slots", "many", "--project", root]);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(stderr(&out).contains("rolled back"), "{}", stderr(&out));
    assert_eq!(fs::read_to_string(&config).unwrap(), before);
}

#[test]
fn config_migrate_dry_run_and_write_on_faber_fixture() {
    let dir = faber_project();
    let root = dir.path().to_str().unwrap();
    let dry = vivi(&["config", "migrate", "--project", root]);
    assert_success(&dry);
    let text = stdout(&dry);
    assert!(text.contains("set role hand pool=glm"), "{text}");
    assert!(text.contains("set role merge pool=host"), "{text}");
    assert!(text.contains("add [pools.host]"), "{text}");
    assert!(!text.contains("add role gate"), "{text}");
    assert!(text.contains("dry-run"), "{text}");
    let before = fs::read_to_string(dir.path().join(".vivi/mailspace.toml")).unwrap();
    assert!(!hand_has_pool(&before), "fixture must start unmigrated");

    let write = vivi(&["config", "migrate", "--write", "--project", root]);
    assert_success(&write);
    assert!(stdout(&write).contains("backup"), "{}", stdout(&write));
    let after = fs::read_to_string(dir.path().join(".vivi/mailspace.toml")).unwrap();
    assert!(hand_has_pool(&after), "{after}");
    assert!(after.contains("[pools.host]"), "{after}");
    assert!(!after.contains("name = \"gate\""), "{after}");
    let bak = fs::read_dir(dir.path().join(".vivi"))
        .unwrap()
        .filter_map(|entry| entry.ok())
        .any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("mailspace.toml.bak.")
        });
    assert!(bak, "expected a timestamped .bak");
}

fn hand_has_pool(raw: &str) -> bool {
    let mut in_hand = false;
    for line in raw.lines() {
        if line.trim() == "[[identities]]" {
            in_hand = false;
        }
        if line.trim() == "name = \"hand\"" {
            in_hand = true;
        }
        if in_hand && line.trim() == "pool = \"glm\"" {
            return true;
        }
    }
    false
}

#[test]
fn config_show_resolved_lists_role_rows() {
    let dir = project_with(
        r#"
name = "demo"
id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
[[identities]]
name = "hand"
kind = "hand"
pool = "glm"
[pools.glm]
provider = "local"
slots = 4
[pump]
to = "hand"
main = "main"
"#,
    );
    let root = dir.path().to_str().unwrap();
    let out = vivi(&["config", "show", "--json", "--project", root]);
    assert_success(&out);
    let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(json["roles"][0]["role"], "hand");
    assert_eq!(json["roles"][0]["pool"], "glm");
    assert_eq!(json["roles"][0]["source"], "role.pool");
}

fn init_project() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    assert_success(&vivi(&["mailspace", "init", "--project", root]));
    dir
}

#[test]
fn mailspace_init_writes_a_starter_pump_table() {
    let dir = init_project();
    let text = fs::read_to_string(dir.path().join(".vivi/mailspace.toml")).unwrap();
    assert!(text.contains("\n[pump]\n"), "{text}");
    assert!(!text.contains("[pump.gate]"), "{text}");
    assert!(text.contains("# Starter pump configuration."), "{text}");
}

#[test]
fn a_second_init_does_not_add_a_pump_table_to_an_existing_mailspace() {
    let dir = project_with("name = \"legacy\"\n");
    let root = dir.path().to_str().unwrap();
    assert_success(&vivi(&["mailspace", "init", "--project", root]));
    let text = fs::read_to_string(dir.path().join(".vivi/mailspace.toml")).unwrap();
    assert!(!text.contains("[pump]"), "{text}");
}

#[test]
fn config_validate_says_not_ready_and_exits_3_on_a_fresh_project() {
    let dir = init_project();
    let root = dir.path().to_str().unwrap();
    let out = vivi(&["config", "validate", "--project", root]);
    assert_eq!(out.status.code(), Some(3), "{}", stdout(&out));
    let text = stdout(&out);
    assert!(text.contains("not ready: "), "{text}");
    assert!(text.contains("item(s) missing"), "{text}");
    assert!(text.contains("- pump.committer_name:"), "{text}");
    assert!(text.contains("- hand pool:"), "{text}");
    assert!(!text.contains("error:"), "{text}");
}

#[test]
fn a_hand_role_without_a_pool_names_the_role_pool_command_not_the_lanes_key() {
    let dir = init_project();
    let root = dir.path().to_str().unwrap();
    assert_success(&vivi(&[
        "role",
        "add",
        "hand",
        "--kind",
        "hand",
        "--project",
        root,
    ]));
    let out = vivi(&["config", "validate", "--project", root]);
    assert_eq!(out.status.code(), Some(3), "{}", stdout(&out));
    let text = stdout(&out);
    assert!(
        text.contains(
            "- hand pool: role hand has no pool; set one with `vivi role pool hand <pool>`"
        ),
        "{text}"
    );
    assert!(!text.contains("lanes"), "{text}");
}

#[test]
fn config_validate_json_marks_missing_items_not_ready() {
    let dir = init_project();
    let root = dir.path().to_str().unwrap();
    let out = vivi(&["config", "validate", "--json", "--project", root]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let rows: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    let missing = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["item"] == "pump.committer_name")
        .unwrap_or_else(|| panic!("{rows}"));
    assert_eq!(missing["severity"], "not_ready");
}

#[test]
fn config_validate_still_fails_with_exit_1_on_a_wrong_value() {
    let dir = init_project();
    let root = dir.path().to_str().unwrap();
    let path = dir.path().join(".vivi/mailspace.toml");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace("[pump]\n", "[pump]\ntick = 0\n")).unwrap();
    let out = vivi(&["config", "validate", "--project", root]);
    assert_eq!(out.status.code(), Some(1), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("tick must be >= 1"),
        "{}",
        stderr(&out)
    );
    assert!(stdout(&out).contains("not ready: "), "{}", stdout(&out));
}

#[test]
fn config_validate_exits_0_for_a_project_without_a_pump() {
    let dir = project_with("name = \"legacy\"\n");
    let root = dir.path().to_str().unwrap();
    let out = vivi(&["config", "validate", "--project", root]);
    assert_success(&out);
}

const RETIRED_KEYS: &str = r#"
name = "demo"
id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
[[identities]]
name = "hand"
kind = "hand"
pool = "glm"
[pools.glm]
provider = "local"
slots = 2
[pump]
branch = "next"
promote = true
to = "hand"
reserved_file = "reserved.txt"
fix_prefix = "fx"
[pump.board]
planners_max = 2
plan_timeout = 60
[pump.planner]
run = "x"
split_hard_cap = 100
[pump.proof]
allow = ["cargo"]
[[pump.plan.proof_rules]]
scope = "src"
proof = "cargo test"
[pump.alarm]
merge_idle_minutes = 5
[pump.dispatch]
default_kind = "plain"
max_attempts = 2
per_tick = 3
transient_max = 1
transient_backoff = 10
overdue_grace = 30
last_resort = "m/ok"
[[pump.dispatch.lanes]]
model = "m/ok"
cap = 1
[pump.dispatch.kinds.plain]
pools = ["glm"]
[pump.merge]
batch = 4
shared_checks = ["true"]
repair_budget = 5
[pump.need]
repair_rounds = 2
[pump.gate]
command = "true"
[pump.targets.other]
repo = "/tmp/other"
branch = "next"
gate = { command = "true" }
[pump.targets.other.merge]
shared_checks = ["true"]
batch = 2
repair_budget = 4
"#;

/// Every `[table] key:` line `config validate` must print for `RETIRED_KEYS`.
const RETIRED_KEY_LINES: [&str; 31] = [
    "[pump] branch:",
    "[pump] promote:",
    "[pump] gate:",
    "[pump] need:",
    "[pump] planner:",
    "[pump] proof:",
    "[pump] plan:",
    "[pump] alarm:",
    "[pump] reserved_file:",
    "[pump] fix_prefix:",
    "[pump.board] planners_max:",
    "[pump.board] plan_timeout:",
    "[pump.dispatch] kinds:",
    "[pump.dispatch] default_kind:",
    "[pump.dispatch] lanes:",
    "[pump.dispatch] max_attempts:",
    "[pump.dispatch] per_tick:",
    "[pump.dispatch] transient_max:",
    "[pump.dispatch] transient_backoff:",
    "[pump.dispatch] overdue_grace:",
    "[pump.dispatch] last_resort:",
    "[pump.merge] batch:",
    "[pump.merge] shared_checks:",
    "[pump.merge] repair_budget:",
    "[pump.targets.other] branch:",
    "[pump.targets.other] gate:",
    "[pump.targets.other.merge] shared_checks:",
    "[pump.targets.other.merge] batch:",
    "[pump.targets.other.merge] repair_budget:",
    "retired: ",
    "not read by vivi 21: ",
];

#[test]
fn config_validate_names_each_retired_key_and_still_reports_readiness() {
    let dir = project_with(RETIRED_KEYS);
    let root = dir.path().to_str().unwrap();
    let out = vivi(&["config", "validate", "--project", root]);
    let text = stdout(&out);
    for expected in RETIRED_KEY_LINES {
        assert!(text.contains(expected), "{expected} missing from:\n{text}");
    }
    for line in text.lines().filter(|line| line.contains("] ")) {
        if line.starts_with("error: [") {
            assert!(
                line.contains("vivi config migrate --write") && line.contains("delete the key"),
                "no fix named: {line}"
            );
        }
    }
    assert!(text.contains("retired: "), "{text}");
    assert_eq!(out.status.code(), Some(1), "{text}{}", stderr(&out));
    assert!(text.contains("error: [pump] gate:"), "{text}");
    assert!(text.contains("not ready: "), "{text}");
}

#[test]
fn config_migrate_removes_each_retired_key_with_its_reason() {
    let dir = project_with(RETIRED_KEYS);
    let root = dir.path().to_str().unwrap();
    let dry = vivi(&["config", "migrate", "--project", root]);
    assert_success(&dry);
    let text = stdout(&dry);
    assert!(
        text.contains("remove [pump] gate = (table: command)"),
        "{text}"
    );
    assert!(text.contains("remove [pump.targets.other] gate"), "{text}");
    assert!(text.contains("there is no integration branch"), "{text}");
    assert_success(&vivi(&["config", "migrate", "--write", "--project", root]));
    let after = fs::read_to_string(dir.path().join(".vivi/mailspace.toml")).unwrap();
    for gone in [
        "branch =",
        "promote =",
        "batch =",
        "shared_checks",
        "repair_rounds",
        "[pump.gate]",
        "[pump.need]",
        "gate =",
        "reserved_file",
        "fix_prefix",
        "planners_max",
        "plan_timeout",
        "[pump.planner]",
        "[pump.proof]",
        "proof_rules",
        "[pump.alarm]",
        "default_kind",
        "max_attempts",
        "per_tick",
        "transient_",
        "overdue_grace",
        "last_resort",
        "lanes",
        "repair_budget",
    ] {
        assert!(!after.contains(gone), "{gone} still in:\n{after}");
    }
    assert!(after.contains("repo = \"/tmp/other\""), "{after}");
    let again = vivi(&["config", "validate", "--project", root]);
    assert!(
        !stdout(&again).contains("vivi config migrate --write"),
        "validate still refuses a key after migrate:\n{}",
        stdout(&again)
    );
}

#[test]
fn config_validate_refuses_the_faber_fixture_keys_and_migrate_clears_them() {
    let dir = faber_project();
    let root = dir.path().to_str().unwrap();
    let out = vivi(&["config", "validate", "--project", root]);
    assert_eq!(out.status.code(), Some(1), "{}", stdout(&out));
    let text = stdout(&out);
    for expected in [
        "error: [pump] reserved_file:",
        "error: [pump] fix_prefix:",
        "error: [pump] priority_file:",
        "error: [pump.board] planners_max:",
        "error: [pump.board] plan_timeout:",
        "error: [pump] planner:",
        "error: [pump.dispatch] last_resort:",
        "error: [pump.dispatch] max_attempts:",
        "error: [pump.dispatch] overdue_grace:",
        "error: [pump.dispatch] per_tick:",
        "error: [pump.dispatch] default_kind:",
        "error: [pump.dispatch] lanes:",
        "error: [pump.dispatch] kinds:",
    ] {
        assert!(text.contains(expected), "{expected} missing from:\n{text}");
    }
    for kept in [
        "[pump.dispatch] budget",
        "[pump.board] every",
        "[pump.merge] format",
    ] {
        assert!(!text.contains(kept), "{kept} is read, not refused:\n{text}");
    }
    assert_success(&vivi(&["config", "migrate", "--write", "--project", root]));
    let after = vivi(&["config", "validate", "--project", root]);
    // The fixture's hand_rules file does not exist here, so other errors may
    // remain; none may be a refused key.
    assert!(
        !stdout(&after).contains("vivi config migrate --write"),
        "validate still refuses a key after migrate:\n{}",
        stdout(&after)
    );
    let written = fs::read_to_string(dir.path().join(".vivi/mailspace.toml")).unwrap();
    assert!(written.contains("hand_rules"), "{written}");
    assert!(written.contains("remote_limit = 1800"), "{written}");
}

#[test]
fn config_validate_names_a_missing_task_rung_and_exits_3() {
    let dir = project_with(
        r#"
name = "demo"
id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
[pools.glm]
provider = "local"
slots = 2
[pump]
[pump.rungs]
need = "true"
main = "true"
pools = ["glm"]
"#,
    );
    let root = dir.path().to_str().unwrap();
    let out = vivi(&["config", "validate", "--project", root]);
    assert_eq!(out.status.code(), Some(3), "{}", stdout(&out));
    let text = stdout(&out);
    assert!(text.contains("- pump.rungs.task:"), "{text}");
    assert!(!text.contains("- pump.rungs.need:"), "{text}");
    assert!(!text.contains("- pump.rungs.main:"), "{text}");
}

fn config_file(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join(".vivi/mailspace.toml")
}

#[test]
fn config_set_writes_a_typed_value_and_a_backup() {
    let dir = init_project();
    let root = dir.path().to_str().unwrap();
    let out = vivi(&["config", "set", "pump.tick", "30", "--project", root]);
    assert_success(&out);
    assert_eq!(stdout(&out).trim(), "set pump.tick = 30 (integer)");
    let text = fs::read_to_string(config_file(&dir)).unwrap();
    assert!(text.contains("tick = 30"), "{text}");
    let err = stderr(&out);
    let backup = err
        .lines()
        .find_map(|line| line.strip_prefix("backup "))
        .unwrap_or_else(|| panic!("{err}"));
    assert!(backup.contains("mailspace.toml.bak."), "{backup}");
    assert!(std::path::Path::new(backup).exists(), "{backup}");
}

#[test]
fn config_set_stores_a_bare_word_as_a_string() {
    let dir = init_project();
    let root = dir.path().to_str().unwrap();
    let out = vivi(&["config", "set", "pump.main", "develop", "--project", root]);
    assert_success(&out);
    assert!(stdout(&out).contains("(string)"), "{}", stdout(&out));
    let text = fs::read_to_string(config_file(&dir)).unwrap();
    assert!(text.contains("main = \"develop\""), "{text}");
}

#[test]
fn config_set_rolls_back_a_write_that_adds_an_error() {
    let dir = init_project();
    let root = dir.path().to_str().unwrap();
    let before = fs::read(config_file(&dir)).unwrap();
    let out = vivi(&["config", "set", "pump.tick", "0", "--project", root]);
    assert!(!out.status.success(), "{}", stdout(&out));
    let err = stderr(&out);
    assert!(err.contains("rolled back"), "{err}");
    assert!(err.contains("tick must be >= 1"), "{err}");
    assert_eq!(fs::read(config_file(&dir)).unwrap(), before);
}

#[test]
fn config_set_reports_errors_that_were_already_there() {
    let dir = init_project();
    let root = dir.path().to_str().unwrap();
    let path = config_file(&dir);
    let text = fs::read_to_string(&path).unwrap();
    let broken = text.replace("[pump]\n", "[pump]\ntick = 0\n");
    fs::write(&path, format!("{broken}\n[pump.dispatch]\nparallel = 0\n")).unwrap();
    let out = vivi(&["config", "set", "pump.tick", "5", "--project", root]);
    assert_success(&out);
    assert!(
        stderr(&out).contains("still invalid: [pump.dispatch] parallel must be >= 1"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn config_set_repairs_a_config_that_does_not_load() {
    let dir = init_project();
    let root = dir.path().to_str().unwrap();
    let path = config_file(&dir);
    let text = fs::read_to_string(&path).unwrap();
    let broken = text.replace("[pump]\n", "[pump]\ntick = \"x\"\n");
    fs::write(&path, format!("{broken}\n[pump.dispatch]\nparallel = 0\n")).unwrap();
    let out = vivi(&["config", "set", "pump.tick", "5", "--project", root]);
    assert_success(&out);
    assert!(
        stderr(&out).contains("still invalid: [pump.dispatch] parallel must be >= 1"),
        "{}",
        stderr(&out)
    );
    assert!(
        fs::read_to_string(&path).unwrap().contains("tick = 5"),
        "{}",
        fs::read_to_string(&path).unwrap()
    );
}

#[test]
fn config_unset_removes_a_key_and_a_second_unset_fails() {
    let dir = init_project();
    let root = dir.path().to_str().unwrap();
    assert_success(&vivi(&[
        "config",
        "set",
        "pump.tick",
        "30",
        "--project",
        root,
    ]));
    let out = vivi(&["config", "unset", "pump.tick", "--project", root]);
    assert_success(&out);
    assert_eq!(stdout(&out).trim(), "unset pump.tick");
    let text = fs::read_to_string(config_file(&dir)).unwrap();
    assert!(!text.contains("tick = 30"), "{text}");
    let again = vivi(&["config", "unset", "pump.tick", "--project", root]);
    assert!(!again.status.success());
    assert!(stderr(&again).contains("is not set"), "{}", stderr(&again));
}

#[test]
fn config_set_refuses_the_role_roster() {
    let dir = init_project();
    let root = dir.path().to_str().unwrap();
    let out = vivi(&["config", "set", "identities.x", "1", "--project", root]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("vivi role"), "{}", stderr(&out));
}

#[test]
fn config_set_is_refused_only_while_the_pump_is_running() {
    let dir = init_project();
    let root = dir.path().to_str().unwrap();
    assert_success(&vivi(&["pump", "pause", "--project", root]));
    let lock_path = dir.path().join(".vivi/pump/lock");
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .unwrap();
    lock.try_lock().unwrap();
    assert_success(&vivi(&[
        "config",
        "set",
        "pump.tick",
        "30",
        "--project",
        root,
    ]));
    drop(lock);

    assert_success(&vivi(&["pump", "resume", "--project", root]));
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .unwrap();
    lock.try_lock().unwrap();
    let before = fs::read(config_file(&dir)).unwrap();
    let out = vivi(&["config", "set", "pump.tick", "31", "--project", root]);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("the pump is running: run vivi pump pause first"),
        "{}",
        stderr(&out)
    );
    assert_eq!(fs::read(config_file(&dir)).unwrap(), before);
    drop(lock);
}
