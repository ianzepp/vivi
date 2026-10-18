//! Path-based routing: `/<provider>/v1/…` and the lenient form without `/v1`.

/// What the front door should do with one request path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Aggregated `/health`.
    AggregateHealth,
    /// A provider-scoped path.
    Provider { name: String, action: Action },
}

/// Provider-local action after the name (and optional `v1`) is stripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Health,
    Models,
    Usage,
    Responses,
    ChatCompletions,
    Messages,
    /// Any other remainder, forwarded as-is after the provider prefix.
    Other(String),
}

/// Parse a request URI path.
#[must_use]
pub fn parse(path: &str) -> Option<Route> {
    let trimmed = path.trim();
    let stripped = trimmed.strip_prefix('/').unwrap_or(trimmed);
    if stripped.is_empty() {
        return None;
    }
    let parts: Vec<&str> = stripped
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        return None;
    }
    if parts.len() == 1 && parts[0] == "health" {
        return Some(Route::AggregateHealth);
    }
    let name = parts[0].to_string();
    let rest = &parts[1..];
    Some(Route::Provider {
        name,
        action: action_from_rest(rest),
    })
}

fn action_from_rest(rest: &[&str]) -> Action {
    let skip_v1 = rest.first().is_some_and(|part| *part == "v1");
    let body = if skip_v1 { &rest[1..] } else { rest };
    match body {
        ["health"] => Action::Health,
        ["models"] => Action::Models,
        ["usage"] => Action::Usage,
        ["responses"] | ["codex", "responses"] => Action::Responses,
        ["chat", "completions"] => Action::ChatCompletions,
        ["messages"] => Action::Messages,
        [] => Action::Other(String::new()),
        _ => Action::Other(format!("/{}", body.join("/"))),
    }
}

/// Path to forward to a child or upstream after stripping `/<provider>`.
#[must_use]
pub fn forward_path(path: &str) -> String {
    let trimmed = path.trim();
    let stripped = trimmed.strip_prefix('/').unwrap_or(trimmed);
    let mut parts = stripped.split('/').filter(|part| !part.is_empty());
    let Some(_) = parts.next() else {
        return "/".into();
    };
    let rest: Vec<&str> = parts.collect();
    if rest.is_empty() {
        "/".into()
    } else {
        format!("/{}", rest.join("/"))
    }
}

impl Action {
    /// Stable label for the access log.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Health => "health",
            Self::Models => "models",
            Self::Usage => "usage",
            Self::Responses => "responses",
            Self::ChatCompletions => "chat/completions",
            Self::Messages => "messages",
            Self::Other(path) => path,
        }
    }
}
