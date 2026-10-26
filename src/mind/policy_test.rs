//! Tests for [`crate::mind::policy`].

use super::*;

fn policy(list: &[HumanOnlyCategory]) -> HumanOnlyPolicy {
    HumanOnlyPolicy {
        categories: list.to_vec(),
    }
}

fn request(category: &str) -> EscalationRequest {
    EscalationRequest {
        category: category.to_string(),
        request: "rotate the deploy key on the hub".to_string(),
        why: "credential creation is reserved for the operator".to_string(),
        options: ["rotate now".to_string(), "leave it".to_string()],
    }
}

#[test]
fn every_category_has_a_distinct_name_and_meaning() {
    let mut names: Vec<_> = HumanOnlyCategory::ALL.iter().map(|c| c.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), HumanOnlyCategory::ALL.len());
    for category in HumanOnlyCategory::ALL {
        assert_eq!(HumanOnlyCategory::parse(category.as_str()), Some(*category));
        assert!(!category.meaning().is_empty());
    }
    assert_eq!(HumanOnlyCategory::parse("flow-lock"), None);
    assert_eq!(HumanOnlyCategory::parse("Credentials"), None);
}

#[test]
fn the_default_policy_reserves_all_five_categories() {
    let default = HumanOnlyPolicy::default();
    assert_eq!(
        default.categories,
        vec![
            HumanOnlyCategory::SpendingAccounts,
            HumanOnlyCategory::Credentials,
            HumanOnlyCategory::HostRoot,
            HumanOnlyCategory::Releases,
            HumanOnlyCategory::ProductIntent,
        ]
    );
    for category in HumanOnlyCategory::ALL {
        assert!(default.contains(category.as_str()));
        assert!(default.ensure_human_only(category.as_str()).is_ok());
    }
    assert!(!default.contains("flow-lock"));
}

#[test]
fn an_empty_policy_permits_no_escalation() {
    let empty = HumanOnlyPolicy { categories: vec![] };
    for category in HumanOnlyCategory::ALL {
        assert!(!empty.contains(category.as_str()));
    }
    let refusal = empty.ensure_human_only("releases").unwrap_err();
    assert_eq!(
        refusal,
        PolicyRefusal::NotHumanOnly {
            category: "releases".into()
        }
    );
}

#[test]
fn a_custom_list_keeps_the_order_given() {
    let custom =
        HumanOnlyPolicy::from_names(&["releases".to_string(), "host-root".to_string()]).unwrap();
    assert_eq!(
        custom.categories,
        vec![HumanOnlyCategory::Releases, HumanOnlyCategory::HostRoot]
    );
    assert!(custom.contains("host-root"));
    assert!(!custom.contains("credentials"));
}

#[test]
fn duplicate_and_unknown_names_are_refused() {
    let duplicate =
        HumanOnlyPolicy::from_names(&["releases".to_string(), "releases".to_string()]).unwrap_err();
    assert!(duplicate.to_string().contains("duplicate"), "{duplicate}");
    let unknown = HumanOnlyPolicy::from_names(&["cash".to_string()]).unwrap_err();
    let said = unknown.to_string();
    assert!(said.contains("unknown category \"cash\""), "{said}");
    assert!(said.contains("spending-accounts"), "{said}");
}

#[test]
fn a_flow_lock_category_refuses_typed_not_unparsed() {
    let refusal = HumanOnlyPolicy::default()
        .ensure_human_only("flow-lock")
        .unwrap_err();
    assert_eq!(
        refusal,
        PolicyRefusal::NotHumanOnly {
            category: "flow-lock".into()
        }
    );
    assert!(refusal.to_string().contains("flow-lock"));
}

#[test]
fn a_complete_request_validates_to_its_category() {
    let done = request("credentials")
        .validate(&HumanOnlyPolicy::default())
        .unwrap();
    assert_eq!(done, HumanOnlyCategory::Credentials);
}

#[test]
fn incomplete_requests_name_the_field() {
    let policy = HumanOnlyPolicy::default();
    let blank = |field: &str, mut req: EscalationRequest| {
        match field {
            "category" => req.category = "  ".into(),
            "request" => req.request = " \n".into(),
            "why" => req.why = String::new(),
            _ => req.options = vec!["only one".to_string()],
        }
        req.validate(&policy).unwrap_err()
    };
    for field in ["category", "request", "why", "options"] {
        assert_eq!(
            blank(field, request("credentials")),
            PolicyRefusal::Incomplete { field },
            "{field}"
        );
    }
}

#[test]
fn field_bounds_refuse_excess_instead_of_clipping() {
    let policy = HumanOnlyPolicy::default();
    let mut req = request("credentials");
    req.request = "r".repeat(MAX_TEXT_CHARS);
    assert_eq!(
        req.validate(&policy).unwrap(),
        HumanOnlyCategory::Credentials
    );
    req.request.push('x');
    assert_eq!(
        req.validate(&policy),
        Err(PolicyRefusal::Incomplete { field: "request" })
    );
    let mut req = request("credentials");
    req.why = "w".repeat(MAX_TEXT_CHARS + 1);
    assert_eq!(
        req.validate(&policy),
        Err(PolicyRefusal::Incomplete { field: "why" })
    );
    let mut req = request("credentials");
    req.options = (0..MAX_OPTIONS).map(|i| format!("option {i}")).collect();
    assert!(req.validate(&policy).is_ok());
    req.options.push("one too many".to_string());
    assert_eq!(
        req.validate(&policy),
        Err(PolicyRefusal::Incomplete { field: "options" })
    );
    let mut req = request("credentials");
    req.options = vec!["o".repeat(MAX_OPTION_CHARS), "leave it".into()];
    assert!(req.validate(&policy).is_ok());
    req.options = vec!["o".repeat(MAX_OPTION_CHARS + 1), "leave it".into()];
    assert_eq!(
        req.validate(&policy),
        Err(PolicyRefusal::Incomplete { field: "options" })
    );
}

#[test]
fn options_need_two_nonblank_alternatives() {
    let policy = HumanOnlyPolicy::default();
    let mut req = request("credentials");
    req.options = vec!["act".into(), "  ".into()];
    assert_eq!(
        req.validate(&policy),
        Err(PolicyRefusal::Incomplete { field: "options" })
    );
    req.options = vec!["act".into(), "wait".into()];
    assert!(req.validate(&policy).is_ok());
}

#[test]
fn membership_is_checked_after_shape() {
    let policy = policy(&[HumanOnlyCategory::Releases]);
    let mut req = request("flow-lock");
    req.request = String::new();
    assert_eq!(
        req.validate(&policy),
        Err(PolicyRefusal::Incomplete { field: "request" })
    );
    req.request = "unlock the flow run".into();
    assert_eq!(
        req.validate(&policy),
        Err(PolicyRefusal::NotHumanOnly {
            category: "flow-lock".into()
        })
    );
    req.category = "releases".into();
    assert_eq!(req.validate(&policy).unwrap(), HumanOnlyCategory::Releases);
}
