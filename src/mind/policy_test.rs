//! Tests for [`crate::mind::policy`].

use super::*;

#[test]
fn every_category_round_trips_through_its_config_name() {
    assert_eq!(HumanCategory::ALL.len(), 5);
    for category in HumanCategory::ALL {
        assert_eq!(HumanCategory::parse(category.as_str()), Some(category));
        assert_eq!(category.to_string(), category.as_str());
    }
    assert_eq!(HumanCategory::parse("flow-lock"), None);
    assert_eq!(HumanCategory::parse("spending_accounts"), None);
    assert_eq!(HumanCategory::parse(""), None);
}

#[test]
fn the_default_policy_is_every_category() {
    let policy = HumanOnlyPolicy::default();
    assert!(!policy.is_empty());
    for category in HumanCategory::ALL {
        assert!(policy.requires_human(category), "{category}");
    }
}

#[test]
fn a_custom_list_replaces_the_defaults() {
    let policy = from_config(&["credentials", "product-intent"]).unwrap();
    assert!(policy.requires_human(HumanCategory::Credentials));
    assert!(policy.requires_human(HumanCategory::ProductIntent));
    assert!(!policy.requires_human(HumanCategory::HostRoot));
    assert_eq!(
        policy.categories().collect::<Vec<_>>(),
        vec![
            HumanCategory::Credentials,
            HumanCategory::ProductIntent,
        ]
    );
}

#[test]
fn an_empty_list_permits_no_escalation() {
    let policy = HumanOnlyPolicy::from_config(Some(vec![])).unwrap();
    assert!(policy.is_empty());
    for category in HumanCategory::ALL {
        assert!(!policy.requires_human(category), "{category}");
    }
}

#[test]
fn duplicates_and_unknown_names_are_refused() {
    let error =
        from_config(&["credentials", "credentials", "releases"]).unwrap_err();
    assert!(error.to_string().contains("duplicate"), "{error}");
    let error = from_config(&["flow-lock"]).unwrap_err();
    assert!(error.to_string().contains("unknown category"), "{error}");
    assert!(error.to_string().contains("product-intent"), "{error}");
}

fn from_config(names: &[&str]) -> Result<HumanOnlyPolicy, VivariumError> {
    HumanOnlyPolicy::from_config(Some(names.iter().map(|s| s.to_string()).collect()))
}

fn escalation(category: &str) -> Escalation {
    Escalation {
        category: category.to_string(),
        why: "the pump needs a release tag and the mind may not cut one".into(),
        options: vec!["cut the tag now".into(), "wait for the operator".into()],
    }
}

#[test]
fn a_complete_on_policy_escalation_passes() {
    let policy = HumanOnlyPolicy::default();
    for category in HumanCategory::ALL {
        escalation(category.as_str()).validate(&policy).unwrap();
    }
}

#[test]
fn an_off_policy_category_is_a_typed_category_refusal() {
    let policy = HumanOnlyPolicy::default();
    // `flow-lock` is an action category, not a human-only one: the
    // payload parses fine and validation returns the typed refusal.
    assert_eq!(
        escalation("flow-lock").validate(&policy).unwrap_err(),
        EscalationRefusal::Category {
            category: "flow-lock".into(),
        }
    );
    assert_eq!(
        escalation("nonsense").validate(&policy).unwrap_err(),
        EscalationRefusal::Category {
            category: "nonsense".into(),
        }
    );
    // An empty policy escalates nothing, so even real categories refuse.
    let empty = HumanOnlyPolicy::from_config(Some(vec![])).unwrap();
    assert!(matches!(
        escalation("releases").validate(&empty),
        Err(EscalationRefusal::Category { .. })
    ));
}

#[test]
fn an_incomplete_request_is_a_distinct_refusal() {
    let policy = HumanOnlyPolicy::default();
    let mut request = escalation("credentials");
    request.why = "   ".into();
    assert!(matches!(
        request.validate(&policy),
        Err(EscalationRefusal::Incomplete { .. })
    ));
    for options in [
        vec![],
        vec!["only one".into()],
        vec!["one".into(), "   ".into()],
    ] {
        let mut request = escalation("credentials");
        request.options = options;
        assert!(matches!(
            request.validate(&policy),
            Err(EscalationRefusal::Incomplete { .. })
        ));
    }
    // Blanks do not count, but two nonblank among blanks pass.
    let mut request = escalation("credentials");
    request.options = vec!["".into(), "a".into(), "  ".into(), "b".into()];
    request.validate(&policy).unwrap();
}

#[test]
fn excess_is_rejected_not_clipped() {
    let policy = HumanOnlyPolicy::default();
    let too_long_category = escalation(&"x".repeat(MAX_CATEGORY_LEN + 1));
    assert_eq!(
        too_long_category.validate(&policy).unwrap_err(),
        EscalationRefusal::TooLong {
            field: "category".into(),
            len: MAX_CATEGORY_LEN + 1,
            max: MAX_CATEGORY_LEN,
        }
    );
    let mut request = escalation("credentials");
    request.why = "w".repeat(MAX_WHY_LEN + 1);
    assert!(matches!(
        request.validate(&policy),
        Err(EscalationRefusal::TooLong { field, .. }) if field == "why",
    ));
    let mut request = escalation("credentials");
    request.options = (0..=MAX_OPTIONS).map(|i| format!("option {i}")).collect();
    assert_eq!(
        request.validate(&policy).unwrap_err(),
        EscalationRefusal::TooManyOptions {
            count: MAX_OPTIONS + 1,
            max: MAX_OPTIONS,
        }
    );
    let mut request = escalation("credentials");
    request.options[1] = "o".repeat(MAX_OPTION_LEN + 1);
    assert!(matches!(
        request.validate(&policy),
        Err(EscalationRefusal::TooLong { field, .. }) if field == "options[1]",
    ));
}

#[test]
fn at_the_limits_is_accepted() {
    let policy = HumanOnlyPolicy::default();
    let request = Escalation {
        category: "product-intent".into(),
        why: "w".repeat(MAX_WHY_LEN),
        options: (0..MAX_OPTIONS)
            .map(|i| format!("opt{i}{}", "x".repeat(MAX_OPTION_LEN - 5)))
            .collect(),
    };
    request.validate(&policy).unwrap();
}

#[test]
fn the_payload_round_trips_through_json() {
    let request = escalation("releases");
    let json = serde_json::to_string(&request).unwrap();
    assert_eq!(serde_json::from_str::<Escalation>(&json).unwrap(), request);
}
