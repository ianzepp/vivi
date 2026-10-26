//! Tests for [`crate::pump::config::StepBudgets`] and the step budget
//! accessors.

use std::path::Path;

use crate::VivariumError;
use crate::pool::dispatch::BUDGET_RANGE;
use crate::pump::config::{DispatchConfig, PumpConfig, StepBudgets};
use crate::pump::envelope::roles::RoleKind;

/// A `StepBudgets` with exactly one override set.
fn only(role: RoleKind, secs: u64) -> StepBudgets {
    let mut budgets = StepBudgets::default();
    match role {
        RoleKind::Planner => budgets.planner = Some(secs),
        RoleKind::Admitter => budgets.admitter = Some(secs),
        RoleKind::Slicer => budgets.slicer = Some(secs),
        RoleKind::Auditor => budgets.auditor = Some(secs),
        RoleKind::HeadCeo => budgets.head_cto = Some(secs),
        RoleKind::Researcher => budgets.researcher = Some(secs),
        RoleKind::Judge => budgets.judge = Some(secs),
        RoleKind::Cto | RoleKind::Merge | RoleKind::Hand | RoleKind::Repairer => {}
    }
    budgets
}

/// The seven thinking steps, in declaration order.
const THINKING: [RoleKind; 7] = [
    RoleKind::Researcher,
    RoleKind::Planner,
    RoleKind::Admitter,
    RoleKind::Slicer,
    RoleKind::Auditor,
    RoleKind::Judge,
    RoleKind::HeadCeo,
];

fn dispatch(text: &str) -> DispatchConfig {
    toml::from_str(text).expect("dispatch table should parse")
}

#[test]
fn step_budgets_override_only_the_named_kind() {
    for named in THINKING {
        let cfg = DispatchConfig {
            budget: 611,
            step_budgets: only(named, 77),
            ..DispatchConfig::default()
        };
        for role in RoleKind::ALL {
            let expected = if role == named { 77 } else { 611 };
            assert_eq!(cfg.step_budget(role), expected, "role {role:?}");
        }
        // A changed fallback moves every non-overridden kind with it.
        let moved = DispatchConfig {
            budget: 222,
            step_budgets: only(named, 77),
            ..DispatchConfig::default()
        };
        for role in RoleKind::ALL {
            let expected = if role == named { 77 } else { 222 };
            assert_eq!(moved.step_budget(role), expected, "role {role:?}");
        }
    }
}

#[test]
fn correction_is_independent_of_every_other_budget() {
    let cfg = DispatchConfig {
        budget: 611,
        step_budgets: StepBudgets {
            envelope_fix: Some(45),
            ..only(RoleKind::Planner, 77)
        },
        ..DispatchConfig::default()
    };
    assert_eq!(cfg.envelope_fix_budget(), 45);
    assert_ne!(cfg.envelope_fix_budget(), cfg.budget);
    assert_ne!(
        cfg.envelope_fix_budget(),
        cfg.step_budget(RoleKind::Planner)
    );
    // The default is 300 and ignores a changed shared budget or overrides.
    assert_eq!(DispatchConfig::default().envelope_fix_budget(), 300);
    let shared = DispatchConfig {
        budget: 611,
        ..DispatchConfig::default()
    };
    assert_eq!(shared.envelope_fix_budget(), 300);
    let overridden = DispatchConfig {
        budget: 611,
        step_budgets: only(RoleKind::Judge, 999),
        ..DispatchConfig::default()
    };
    assert_eq!(overridden.envelope_fix_budget(), 300);
}

#[test]
fn head_cto_has_one_configuration_spelling() {
    let cfg = dispatch("[step_budgets]\nhead-cto = 99\n");
    assert_eq!(cfg.step_budgets.head_cto, Some(99));
    assert_eq!(cfg.step_budget(RoleKind::HeadCeo), 99);

    // The same override through the full `[pump.dispatch.step_budgets]` path.
    let pump: PumpConfig = toml::from_str("[dispatch]\n[dispatch.step_budgets]\nhead-cto = 99\n")
        .expect("pump table should parse");
    assert_eq!(pump.dispatch.step_budget(RoleKind::HeadCeo), 99);

    for spelling in ["head-ceo", "head_ceo", "headcto"] {
        let text = format!("[step_budgets]\n{spelling} = 99\n");
        let err =
            toml::from_str::<DispatchConfig>(&text).expect_err("`head-cto` is the only spelling");
        assert!(err.to_string().contains("unknown field"), "{err}");
    }
}

#[test]
fn step_budgets_reject_unknown_keys_and_mistyped_values() {
    for spelling in ["cto", "pump-planner", "merge", "repairer", "planners"] {
        let text = format!("[step_budgets]\n{spelling} = 30\n");
        let err =
            toml::from_str::<DispatchConfig>(&text).expect_err("only the canonical keys parse");
        assert!(err.to_string().contains("unknown field"), "{err}");
    }
    for value in ["-1", "1.5", "\"soon\"", "true"] {
        let text = format!("[step_budgets]\nplanner = {value}\n");
        assert!(
            toml::from_str::<DispatchConfig>(&text).is_err(),
            "`planner = {value}` must not parse"
        );
    }
}

#[test]
fn step_budgets_round_trip_without_materializing_overrides() {
    assert!(StepBudgets::default().is_empty());
    // No overrides: the table is not written at all.
    let text = toml::to_string(&DispatchConfig::default()).expect("serialize");
    assert!(!text.contains("step_budgets"), "{text}");

    // Set keys are written; absent ones are not materialized.
    let set = StepBudgets {
        head_cto: Some(99),
        envelope_fix: Some(45),
        ..StepBudgets::default()
    };
    assert!(!set.is_empty());
    let text = toml::to_string(&set).expect("serialize");
    assert!(text.contains("head-cto = 99"), "{text}");
    assert!(text.contains("envelope-fix = 45"), "{text}");
    assert!(!text.contains("planner"), "{text}");
    let back: StepBudgets = toml::from_str(&text).expect("deserialize");
    assert_eq!(back, set);

    // A written dispatch table parses back to the same budgets.
    let cfg = dispatch("[step_budgets]\nhead-cto = 99\n");
    assert_eq!(cfg.step_budgets.head_cto, Some(99));
    assert_eq!(cfg.step_budget(RoleKind::HeadCeo), 99);
    assert_eq!(cfg.step_budget(RoleKind::Hand), cfg.budget);
}

/// A `PumpConfig` that validates clean: committer set, default budgets.
fn valid_pump() -> PumpConfig {
    PumpConfig {
        committer_name: "T".into(),
        committer_email: "t@t".into(),
        ..PumpConfig::default()
    }
}

/// Budget findings are errors (never missing items) and `validate` fails
/// with [`VivariumError::Config`] naming `needle`.
fn assert_config_error(cfg: &PumpConfig, needle: &str) {
    let found = cfg.findings_for(Path::new("."), &[]);
    assert!(
        found.missing.is_empty(),
        "budget problems are errors: {:?}",
        found.missing
    );
    assert!(
        found.errors.iter().any(|message| message.contains(needle)),
        "{needle}: {:?}",
        found.errors
    );
    let err = cfg
        .validate(Path::new("."), &[])
        .expect_err("invalid budgets must fail validate");
    assert!(matches!(err, VivariumError::Config(_)), "{err}");
    assert!(err.to_string().contains(needle), "{err}");
}

/// A fallback at the pool ceiling inherits into all seven thinking steps,
/// and each finding names that step's canonical key (`head-cto` included).
fn assert_ceiling_fallback_names_all_steps(cfg: &PumpConfig) {
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
            found.errors.iter().any(|message| message.contains(&key)),
            "{step}: {:?}",
            found.errors
        );
    }
}

/// Every thinking step overridden to the same value; correction unset.
fn all_thinking(secs: u64) -> StepBudgets {
    StepBudgets {
        planner: Some(secs),
        admitter: Some(secs),
        slicer: Some(secs),
        auditor: Some(secs),
        head_cto: Some(secs),
        researcher: Some(secs),
        judge: Some(secs),
        envelope_fix: None,
    }
}

#[test]
fn step_budget_validation_rejects_invalid_and_unraisable_values() {
    let max = *BUDGET_RANGE.end();

    // Zero and oversized fallbacks sit outside the pool range itself.
    for budget in [0, max + 1] {
        let mut cfg = valid_pump();
        cfg.dispatch.budget = budget;
        assert_config_error(&cfg, "[pump.dispatch] budget");
    }

    // A fallback at the ceiling is unraisable on its own: every thinking
    // step inherits it and is named by its canonical key.
    let mut cfg = valid_pump();
    cfg.dispatch.budget = max;
    assert_ceiling_fallback_names_all_steps(&cfg);

    // All-overrides fallback-at-maximum: a lower override on every thinking
    // step raises the ceiling fallback, so the config validates.
    let mut raisable = valid_pump();
    raisable.dispatch.budget = max;
    raisable.dispatch.step_budgets = all_thinking(max - 1);
    raisable
        .validate(Path::new("."), &[])
        .expect("seven lower overrides raise the ceiling fallback");

    // Thinking overrides: zero, the ceiling and oversized values are errors
    // on the named key; maximum-minus-one is the largest valid override.
    for secs in [0, max, max + 1] {
        let mut cfg = valid_pump();
        cfg.dispatch.step_budgets = only(RoleKind::Planner, secs);
        assert_config_error(&cfg, "pump.dispatch.step_budgets.planner");
    }
    let mut cfg = valid_pump();
    cfg.dispatch.step_budgets = only(RoleKind::Planner, max - 1);
    cfg.validate(Path::new("."), &[])
        .expect("maximum-minus-one keeps retry headroom");

    // Correction (envelope-fix) may use the full pool range, ceiling
    // included; zero and oversized values are errors on its own key.
    for secs in [0, max + 1] {
        let mut cfg = valid_pump();
        cfg.dispatch.step_budgets.envelope_fix = Some(secs);
        assert_config_error(&cfg, "pump.dispatch.step_budgets.envelope-fix");
    }
    let mut cfg = valid_pump();
    cfg.dispatch.step_budgets.envelope_fix = Some(max);
    cfg.validate(Path::new("."), &[])
        .expect("correction may sit at the ceiling");

    // Negatives, fractions and strings never reach validation: the table
    // refuses to deserialize, so no value silently falls back.
    for value in ["-1", "1.5", "\"soon\""] {
        let text = format!("[dispatch.step_budgets]\nplanner = {value}\n");
        assert!(
            toml::from_str::<PumpConfig>(&text).is_err(),
            "`planner = {value}` must not parse"
        );
    }
    // Unknown names are the same deserialization gate, not a finding.
    for name in ["cto", "planners"] {
        let text = format!("[dispatch.step_budgets]\n{name} = 30\n");
        let err = toml::from_str::<PumpConfig>(&text).expect_err("unknown keys must not parse");
        assert!(err.to_string().contains("unknown field"), "{err}");
    }
}
