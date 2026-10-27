use std::collections::BTreeMap;

use super::*;

#[test]
fn the_provider_key_is_the_prefix_before_the_first_slash() {
    assert_eq!(provider_key("zai/glm-5.3-flash"), "zai");
    assert_eq!(provider_key("openrouter/anthropic/claude"), "openrouter");
}

#[test]
fn an_unqualified_slug_is_its_own_provider_key() {
    assert_eq!(provider_key("glm-5.3-flash"), "glm-5.3-flash");
    assert_eq!(provider_key(""), "");
}

#[test]
fn slugs_of_one_provider_group_under_one_key() {
    let models = ["zai/glm-5.3-flash", "zai/glm-5-turbo", "glm-5.3-flash"];
    let mut groups: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for model in models {
        groups.entry(provider_key(model)).or_default().push(model);
    }
    assert_eq!(groups.get("zai").map(Vec::len), Some(2));
    assert_eq!(groups.get("glm-5.3-flash").map(Vec::len), Some(1));
}

#[test]
fn failure_classes_answer_with_their_stable_words_and_round_trip() {
    assert_eq!(FailureClass::RateLimit.word(), "rate-limit");
    assert_eq!(FailureClass::Quota.word(), "quota");
    for class in [FailureClass::RateLimit, FailureClass::Quota] {
        let text = serde_json::to_string(&class).unwrap();
        assert_eq!(serde_json::from_str::<FailureClass>(&text).unwrap(), class);
    }
}

#[test]
fn evidence_carries_the_provider_the_deadline_and_the_class() {
    let evidence = Evidence {
        provider: "zai".into(),
        until: 1_700_000_100,
        class: FailureClass::RateLimit,
    };
    let text = serde_json::to_string(&evidence).unwrap();
    assert_eq!(serde_json::from_str::<Evidence>(&text).unwrap(), evidence);
    assert_eq!(evidence.until, 1_700_000_100);
}
