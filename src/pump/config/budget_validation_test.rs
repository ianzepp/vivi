//! Tests for the dispatch-budget half of [`super::findings_for`]: fallback
//! bounds against [`crate::pool::dispatch::BUDGET_RANGE`], per-step headroom,
//! the correction (envelope-fix) range, and the deserialization gate.

use std::path::Path;

use crate::VivariumError;
use crate::pool::dispatch::BUDGET_RANGE;
use crate::pump::config::{PumpConfig, StepBudgets};

fn pump(text: &str) -> PumpConfig {
    let mail: crate::mailspace::MailspaceConfig =
        toml::from_str(text).expect("mailspace text should parse");
    mail.pump.expect("pump table should be present")
}

/// A config that validates clean (its dispatch budgets are in range).
fn valid() -> PumpConfig {
    pump("name = \"t\"\n[pump]\ncommitter_name = \"T\"\ncommitter_email = \"t@t\"\n")
}

/// Invalid budgets are errors, never missing items, and `validate` must
/// classify them as [`VivariumError::Config`] naming `needle`.
fn assert_config_error(cfg: &PumpConfig, needle: &str) {
    let found = cfg.findings_for(Path::new("."), &[]);
    assert!(
        found.missing.is_empty(),
        "budget problems are errors: {:?}",
        found.missing
    );
    assert!(
        found.errors.iter().any(|e| e.contains(needle)),
        "{needle}: {:?}",
        found.errors
    );
    let err = cfg
        .validate(Path::new("."), &[])
        .expect_err("invalid budgets must fail validate");
    assert!(matches!(err, VivariumError::Config(_)), "{err}");
    assert!(err.to_string().contains(needle), "{err}");
}

/// A fallback outside the pool range yields exactly one root-cause finding
/// (no per-step headroom findings on top of it).
fn assert_fallback_out_of_range(budget: u64) {
    let mut cfg = valid();
    cfg.dispatch.budget = budget;
    assert_config_error(&cfg, "[pump.dispatch] budget");
    let found = cfg.findings_for(Path::new("."), &[]);
    assert_eq!(
        found.errors,
        vec![format!(
            "[pump.dispatch] budget must be between {} and {} seconds",
            BUDGET_RANGE.start(),
            BUDGET_RANGE.end()
        )],
        "budget = {budget}"
    );
}

/// A fallback at the pool ceiling inherits into all seven thinking steps;
/// each finding names that step's canonical key (including `head-cto`).
fn assert_ceiling_fallback_names_every_step_key() {
    let mut cfg = valid();
    cfg.dispatch.budget = *BUDGET_RANGE.end();
    let found = cfg.findings_for(Path::new("."), &[]);
    assert_eq!(found.errors.len(), 7, "{:?}", found.errors);
    for step in [
        "planner",
        "admitter",
        "slicer",
        "auditor",
        "head-cto",
        "researcher",
        "judge",
    ] {
        let key = format!("pump.dispatch.step_budgets.{step}");
        assert!(
            found
                .errors
                .iter()
                .any(|e| e.contains(&key) && e.contains("[pump.dispatch] budget")),
            "{step}: {:?}",
            found.errors
        );
    }
}

/// With a valid lower override on every thinking step, a ceiling fallback is
/// raisable and the config validates.
fn assert_all_lower_overrides_raise_a_ceiling_fallback() {
    let below = *BUDGET_RANGE.end() - 1;
    let mut cfg = valid();
    cfg.dispatch.budget = *BUDGET_RANGE.end();
    cfg.dispatch.step_budgets = StepBudgets {
        planner: Some(below),
        admitter: Some(below),
        slicer: Some(below),
        auditor: Some(below),
        head_cto: Some(below),
        researcher: Some(below),
        judge: Some(below),
        envelope_fix: None,
    };
    cfg.validate(Path::new("."), &[])
        .expect("seven lower overrides make the ceiling fallback raisable");
}

/// Wrong types and unknown step names never reach validation: the
/// `[pump.dispatch.step_budgets]` table refuses to deserialize, so no value
/// silently falls back. Direct `PumpConfig` text takes root-level tables;
/// the `[pump]` header belongs to the mailspace file.
fn assert_deserialization_gate() {
    let control = "[dispatch]\nbudget = 600\n[dispatch.step_budgets]\nplanner = 30\n";
    toml::from_str::<PumpConfig>(control).expect("control should parse");
    for text in [
        "[dispatch.step_budgets]\nplanner = \"long\"\n",
        "[dispatch.step_budgets]\nplanner = -5\n",
        "[dispatch.step_budgets]\nplanner = 1.5\n",
        "[dispatch.step_budgets]\ncto = 30\n",
        "[dispatch.step_budgets]\nhead_cto = 30\n",
    ] {
        assert!(
            toml::from_str::<PumpConfig>(text).is_err(),
            "should not parse: {text}"
        );
    }
}

#[test]
fn step_budget_validation_rejects_invalid_and_unraisable_values() {
    for budget in [0, *BUDGET_RANGE.end() + 1] {
        assert_fallback_out_of_range(budget);
    }
    assert_ceiling_fallback_names_every_step_key();
    assert_all_lower_overrides_raise_a_ceiling_fallback();
    for bad in [0, *BUDGET_RANGE.end()] {
        let mut cfg = valid();
        cfg.dispatch.step_budgets.head_cto = Some(bad);
        assert_config_error(&cfg, "pump.dispatch.step_budgets.head-cto");
    }
    let max = *BUDGET_RANGE.end();
    for bad in [0, max + 1] {
        let mut cfg = valid();
        cfg.dispatch.step_budgets.envelope_fix = Some(bad);
        assert_config_error(&cfg, "pump.dispatch.step_budgets.envelope-fix");
    }
    let mut cfg = valid();
    cfg.dispatch.step_budgets.envelope_fix = Some(max);
    cfg.validate(Path::new("."), &[])
        .expect("correction may use the full pool range, ceiling included");
    assert_deserialization_gate();
}
