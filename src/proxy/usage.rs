//! Pull token counts out of Responses and Chat Completions payloads.

use serde_json::Value;

/// Token counts harvested from a model response (never from a request body).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cached: Option<u64>,
    pub upstream_id: Option<String>,
}

impl Usage {
    /// Merge `other` on top of `self`, keeping the first id if both set.
    pub fn merge(&mut self, other: Self) {
        self.input = other.input.or(self.input);
        self.output = other.output.or(self.output);
        self.cached = other.cached.or(self.cached);
        if self.upstream_id.is_none() {
            self.upstream_id = other.upstream_id;
        }
    }
}

/// Read usage from a Responses `response` object or a chat completion.
#[must_use]
pub fn from_value(value: &Value) -> Usage {
    if let Some(usage) = from_responses(value) {
        return usage;
    }
    if let Some(usage) = from_chat(value) {
        return usage;
    }
    Usage {
        upstream_id: value.get("id").and_then(Value::as_str).map(str::to_owned),
        ..Usage::default()
    }
}

fn from_responses(value: &Value) -> Option<Usage> {
    let response = if value.get("type").and_then(Value::as_str) == Some("response.completed") {
        value.get("response")?
    } else if value.get("object").and_then(Value::as_str) == Some("response")
        || value.get("output").is_some()
    {
        value
    } else {
        return None;
    };
    let usage = response.get("usage")?;
    Some(Usage {
        input: uint(usage, "input_tokens"),
        output: uint(usage, "output_tokens"),
        cached: usage
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .or_else(|| uint(usage, "cached_tokens")),
        upstream_id: response
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn from_chat(value: &Value) -> Option<Usage> {
    let usage = value.get("usage")?;
    Some(Usage {
        input: uint(usage, "prompt_tokens").or_else(|| uint(usage, "input_tokens")),
        output: uint(usage, "completion_tokens").or_else(|| uint(usage, "output_tokens")),
        cached: usage
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64),
        upstream_id: value.get("id").and_then(Value::as_str).map(str::to_owned),
    })
}

/// Harvest usage from one SSE `data:` JSON object.
#[must_use]
pub fn from_sse_data(data: &str) -> Option<Usage> {
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return None;
    };
    let usage = from_value(&value);
    if usage.input.is_some() || usage.output.is_some() || usage.upstream_id.is_some() {
        Some(usage)
    } else {
        None
    }
}

/// Best-effort model name from a request JSON body.
#[must_use]
pub fn model_from_body(body: &[u8]) -> Option<String> {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return None;
    };
    value
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn uint(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}
