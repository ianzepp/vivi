//! Tests for [`crate::pump::config::StepBudgets`] and the step budget
//! accessors.

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
