use super::route::{self, Action, Route};

#[test]
fn parses_aggregate_health() {
    assert_eq!(route::parse("/health"), Some(Route::AggregateHealth));
}

#[test]
fn parses_v1_and_lenient_paths() {
    match route::parse("/codex/v1/responses") {
        Some(Route::Provider { name, action }) => {
            assert_eq!(name, "codex");
            assert_eq!(action, Action::Responses);
        }
        other => panic!("{other:?}"),
    }
    match route::parse("/codex/responses") {
        Some(Route::Provider { action, .. }) => assert_eq!(action, Action::Responses),
        other => panic!("{other:?}"),
    }
    match route::parse("/codex/codex/responses") {
        Some(Route::Provider { action, .. }) => assert_eq!(action, Action::Responses),
        other => panic!("{other:?}"),
    }
    match route::parse("/kimi/v1/chat/completions") {
        Some(Route::Provider { action, .. }) => assert_eq!(action, Action::ChatCompletions),
        other => panic!("{other:?}"),
    }
    match route::parse("/kimi/v1/messages") {
        Some(Route::Provider { action, .. }) => assert_eq!(action, Action::Messages),
        other => panic!("{other:?}"),
    }
}

#[test]
fn strips_provider_for_forward() {
    assert_eq!(
        route::forward_path("/llama/v1/chat/completions"),
        "/v1/chat/completions"
    );
    assert_eq!(route::forward_path("/llama"), "/");
}
