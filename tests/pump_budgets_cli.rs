//! Invalid `[pump.dispatch.step_budgets]` tables through `vivi config validate`.
//!
//! Configuration findings reach `config validate` as error-severity issues
//! (exit 1); a table that fails to deserialize reports through stderr and
//! also exits 1. A ready project with a valid override table stays at exit 0.

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

/// A project whose pump validates clean (see the same shape in
/// `config_cli.rs`), plus a `[pump.dispatch]` block spliced in before the
/// rungs.
fn ready_project_with(dispatch_block: &str) -> tempfile::TempDir {
    project_with(
        &READY_PROJECT.replace("[pump.rungs]", &format!("{dispatch_block}\n\n[pump.rungs]")),
    )
}

/// Messages of the error-severity rows `config validate --json` printed.
/// Warnings and not-ready items are unrelated here and ignored.
fn error_messages(output: &Output) -> Vec<String> {
    let rows: serde_json::Value = serde_json::from_str(&stdout(output))
        .unwrap_or_else(|err| panic!("json issues: {err}\nstdout:\n{}", stdout(output)));
    rows.as_array()
        .unwrap()
        .iter()
        .filter(|row| row["severity"] == "error")
        .map(|row| row["message"].as_str().unwrap().to_owned())
        .collect()
}

/// The configuration deserialized but validate must reject it: exit 1 with
/// exactly one error-severity issue naming `needle`.
fn assert_single_json_error(output: &Output, needle: &str) {
    assert_eq!(output.status.code(), Some(1), "stdout:\n{}", stdout(output));
    let errors = error_messages(output);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(
        errors[0].contains(needle),
        "expected {needle:?} in {:?}",
        errors[0]
    );
}

/// The configuration failed to deserialize: exit 1, the offending key named
/// on stderr, and no JSON issue array to inspect.
fn assert_deserialize_failure(output: &Output, needle: &str) {
    assert_eq!(output.status.code(), Some(1), "stdout:\n{}", stdout(output));
    assert!(
        stderr(output).contains(needle),
        "expected {needle:?} in stderr:\n{}",
        stderr(output)
    );
}

#[test]
fn fallback_outside_the_pool_range_is_reported_once_as_the_root_cause() {
    for budget in ["0", "43201"] {
        let dir = ready_project_with(&format!("[pump.dispatch]\nbudget = {budget}"));
        let root = dir.path().to_str().unwrap();
        let out = vivi(&["config", "validate", "--json", "--project", root]);
        assert_eq!(out.status.code(), Some(1), "budget = {budget}");
        let errors = error_messages(&out);
        assert_eq!(
            errors,
            vec!["[pump.dispatch] budget must be between 1 and 43200 seconds".to_owned()],
            "budget = {budget}"
        );
    }
}

#[test]
fn a_zero_planner_override_is_an_error() {
    let dir = ready_project_with(
        "[pump.dispatch]\nbudget = 600\n[pump.dispatch.step_budgets]\nplanner = 0",
    );
    let root = dir.path().to_str().unwrap();
    let out = vivi(&["config", "validate", "--json", "--project", root]);
    assert_single_json_error(&out, "pump.dispatch.step_budgets.planner");
}

#[test]
fn a_planner_override_at_the_pool_ceiling_is_an_error() {
    let dir = ready_project_with(
        "[pump.dispatch]\nbudget = 600\n[pump.dispatch.step_budgets]\nplanner = 43200",
    );
    let root = dir.path().to_str().unwrap();
    let out = vivi(&["config", "validate", "--json", "--project", root]);
    assert_single_json_error(&out, "pump.dispatch.step_budgets.planner");
}

#[test]
fn an_oversized_planner_override_is_an_error() {
    let dir = ready_project_with(
        "[pump.dispatch]\nbudget = 600\n[pump.dispatch.step_budgets]\nplanner = 43201",
    );
    let root = dir.path().to_str().unwrap();
    let out = vivi(&["config", "validate", "--json", "--project", root]);
    assert_single_json_error(&out, "pump.dispatch.step_budgets.planner");
}

#[test]
fn envelope_fix_bounds_are_checked_independently() {
    for value in ["0", "43201"] {
        let dir = ready_project_with(&format!(
            "[pump.dispatch]\nbudget = 600\n[pump.dispatch.step_budgets]\nplanner = 120\nenvelope-fix = {value}"
        ));
        let root = dir.path().to_str().unwrap();
        let out = vivi(&["config", "validate", "--json", "--project", root]);
        assert_single_json_error(&out, "pump.dispatch.step_budgets.envelope-fix");
    }
    // The correction step has no headroom rule: the ceiling itself is valid.
    let dir = ready_project_with(
        "[pump.dispatch]\nbudget = 600\n[pump.dispatch.step_budgets]\nplanner = 120\nenvelope-fix = 43200",
    );
    let root = dir.path().to_str().unwrap();
    let out = vivi(&["config", "validate", "--json", "--project", root]);
    assert_success(&out);
    assert!(error_messages(&out).is_empty(), "{}", stdout(&out));
}

#[test]
fn mistyped_override_values_fail_to_deserialize() {
    for value in ["-5", "1.5", "\"ten\"", "true"] {
        let dir = ready_project_with(&format!(
            "[pump.dispatch]\nbudget = 600\n[pump.dispatch.step_budgets]\nplanner = {value}"
        ));
        let root = dir.path().to_str().unwrap();
        let out = vivi(&["config", "validate", "--json", "--project", root]);
        assert_deserialize_failure(&out, "pump.dispatch.step_budgets.planner");
    }
}

#[test]
fn unknown_and_misspelled_step_names_fail_to_deserialize() {
    for name in ["cto", "head_cto", "head-ceo", "pump-planner", "planners"] {
        let dir = ready_project_with(&format!(
            "[pump.dispatch]\nbudget = 600\n[pump.dispatch.step_budgets]\n{name} = 30"
        ));
        let root = dir.path().to_str().unwrap();
        let out = vivi(&["config", "validate", "--json", "--project", root]);
        assert_deserialize_failure(&out, &format!("unknown field `{name}`"));
    }
}

#[test]
fn a_valid_lower_override_with_a_changed_fallback_validates_clean() {
    let dir = ready_project_with(
        "[pump.dispatch]\nbudget = 600\n[pump.dispatch.step_budgets]\nplanner = 120\nhead-cto = 120",
    );
    let root = dir.path().to_str().unwrap();
    let out = vivi(&["config", "validate", "--json", "--project", root]);
    assert_success(&out);
    assert!(error_messages(&out).is_empty(), "{}", stdout(&out));
}

const READY_PROJECT: &str = r#"
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
"#;
