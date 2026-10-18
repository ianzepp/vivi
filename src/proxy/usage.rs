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

    fn is_empty(&self) -> bool {
        self.input.is_none() && self.output.is_none() && self.upstream_id.is_none()
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
    if let Some(usage) = from_timings(value) {
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
    } else if value.get("object").and_then(Value::as_str) == Some("response") {
        value
    } else {
        return None;
    };
    usage_object(response.get("usage")?, response.get("id"))
}

fn from_chat(value: &Value) -> Option<Usage> {
    usage_object(value.get("usage")?, value.get("id"))
}

fn from_timings(value: &Value) -> Option<Usage> {
    let timings = value.get("timings")?;
    let input = uint(timings, "prompt_n").or_else(|| uint(value, "tokens_evaluated"));
    let output = uint(timings, "predicted_n").or_else(|| uint(value, "tokens_predicted"));
    if input.is_none() && output.is_none() {
        return None;
    }
    Some(Usage {
        input,
        output,
        cached: None,
        upstream_id: value.get("id").and_then(Value::as_str).map(str::to_owned),
    })
}

fn usage_object(usage: &Value, id: Option<&Value>) -> Option<Usage> {
    if usage.is_null() {
        return None;
    }
    Some(Usage {
        input: uint(usage, "prompt_tokens").or_else(|| uint(usage, "input_tokens")),
        output: uint(usage, "completion_tokens").or_else(|| uint(usage, "output_tokens")),
        cached: usage
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(as_uint)
            .or_else(|| {
                usage
                    .pointer("/input_tokens_details/cached_tokens")
                    .and_then(as_uint)
            })
            .or_else(|| uint(usage, "cached_tokens")),
        upstream_id: id.and_then(Value::as_str).map(str::to_owned),
    })
}

/// Harvest usage from one SSE `data:` JSON object.
#[must_use]
pub fn from_sse_data(data: &str) -> Option<Usage> {
    let Ok(value) = serde_json::from_str::<Value>(data.trim()) else {
        return None;
    };
    let usage = from_value(&value);
    if usage.is_empty() { None } else { Some(usage) }
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
    value.get(key).and_then(as_uint)
}

fn as_uint(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number_as_uint(number),
        Value::String(text) => match text.parse() {
            Ok(value) => Some(value),
            Err(_) => None,
        },
        _ => None,
    }
}

fn number_as_uint(number: &serde_json::Number) -> Option<u64> {
    if let Some(value) = number.as_u64() {
        return Some(value);
    }
    if let Some(value) = number.as_i64() {
        return match u64::try_from(value) {
            Ok(converted) => Some(converted),
            Err(_) => None,
        };
    }
    float_as_uint(number.as_f64()?)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn float_as_uint(value: f64) -> Option<u64> {
    if value.is_finite() && value >= 0.0 {
        Some(value as u64)
    } else {
        None
    }
}
