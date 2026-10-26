use super::*;

use serde_json::json;

/// Every defer kind keeps its stable word, and the word parses back.
#[test]
fn defer_kind_words_round_trip() {
    let pairs = [
        (DeferKind::ModelCap, "model-cap"),
        (DeferKind::ProviderCooldown, "provider-cooldown"),
        (DeferKind::BandUnavailable, "band-unavailable"),
        (DeferKind::DispatchBusy, "dispatch-busy"),
    ];
    for (kind, word) in pairs {
        assert_eq!(kind.word(), word);
        assert_eq!(DeferKind::parse(word), Some(kind));
    }
}

/// Unknown or wrong-case words parse to no defer kind at all.
#[test]
fn defer_kind_parse_refuses_unknown_words() {
    assert_eq!(DeferKind::parse("model_cap"), None);
    assert_eq!(DeferKind::parse("Model-Cap"), None);
    assert_eq!(DeferKind::parse("cooldown"), None);
    assert_eq!(DeferKind::parse(""), None);
}

/// Every provider-error class keeps its stable word, and the word parses
/// back.
#[test]
fn provider_error_class_words_round_trip() {
    let pairs = [
        (ProviderErrorClass::RateLimit, "rate-limit"),
        (ProviderErrorClass::Quota, "quota"),
    ];
    for (class, word) in pairs {
        assert_eq!(class.word(), word);
        assert_eq!(ProviderErrorClass::parse(word), Some(class));
    }
    assert_eq!(ProviderErrorClass::parse("rate_limit"), None);
}

/// A defer kind serializes to its stable word and back.
#[test]
fn defer_kind_serde_matches_word() {
    for kind in [
        DeferKind::ModelCap,
        DeferKind::ProviderCooldown,
        DeferKind::BandUnavailable,
        DeferKind::DispatchBusy,
    ] {
        let text = serde_json::to_value(kind).unwrap();
        assert_eq!(text, json!(kind.word()));
        let back: DeferKind = serde_json::from_value(text).unwrap();
        assert_eq!(back, kind);
    }
}

/// A provider-error class serializes to its stable word and back.
#[test]
fn provider_error_class_serde_matches_word() {
    for class in [ProviderErrorClass::RateLimit, ProviderErrorClass::Quota] {
        let text = serde_json::to_value(class).unwrap();
        assert_eq!(text, json!(class.word()));
        let back: ProviderErrorClass = serde_json::from_value(text).unwrap();
        assert_eq!(back, class);
    }
}

/// Cooldown metadata survives a serde round trip with class, provider and
/// absolute until intact.
#[test]
fn cooldown_metadata_round_trips() {
    let cooldown = Cooldown {
        class: ProviderErrorClass::RateLimit,
        provider: "anthropic".to_string(),
        until: 1_800,
    };
    let text = serde_json::to_value(&cooldown).unwrap();
    assert_eq!(
        text,
        json!({"class": "rate-limit", "provider": "anthropic", "until": 1_800})
    );
    let back: Cooldown = serde_json::from_value(text).unwrap();
    assert_eq!(back, cooldown);
}

/// The admission envelope round trips both outcomes, including a deferral
/// with no known retry instant.
#[test]
fn admission_envelope_round_trips() {
    let ready: Admission<&str> = Admission::Ready("P1");
    let back: Admission<String> =
        serde_json::from_value(serde_json::to_value(&ready).unwrap()).unwrap();
    assert_eq!(back, Admission::Ready("P1".to_string()));

    let deferred: Admission<String> = Admission::Deferred(Deferred {
        kind: DeferKind::ProviderCooldown,
        until: Some(1_800),
    });
    let text = serde_json::to_value(&deferred).unwrap();
    assert_eq!(
        text,
        json!({"deferred": {"kind": "provider-cooldown", "until": 1_800}})
    );
    let back: Admission<String> =
        serde_json::from_value(serde_json::to_value(&deferred).unwrap()).unwrap();
    assert_eq!(back, deferred);

    let waiting: Admission<String> = Admission::Deferred(Deferred {
        kind: DeferKind::DispatchBusy,
        until: None,
    });
    let back: Admission<String> =
        serde_json::from_value(serde_json::to_value(&waiting).unwrap()).unwrap();
    assert_eq!(back, waiting);
}

/// Provider keys split before the first slash.
#[test]
fn provider_key_takes_the_prefix_before_the_first_slash() {
    assert_eq!(provider_key("anthropic/claude-x"), "anthropic");
    assert_eq!(provider_key("zai/glm-5.3-flash"), "zai");
    assert_eq!(provider_key("a/b/c"), "a");
}

/// A slug without a slash is its own provider key.
#[test]
fn provider_key_keeps_a_slashless_slug_whole() {
    assert_eq!(provider_key("gpt-5"), "gpt-5");
    assert_eq!(provider_key("o4-mini-high"), "o4-mini-high");
}

/// The collection-note key and the default duration are the standardized
/// constants.
#[test]
fn note_key_and_default_duration_are_stable() {
    assert_eq!(COOLDOWN_NOTE_KEY, "provider_cooldown");
    assert_eq!(DEFAULT_COOLDOWN_SECS, 60);
}
