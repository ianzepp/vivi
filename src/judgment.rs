//! Judgment provider: typed System One questions over assembled state.
//!
//! Vivi asks narrow yes/no (Noul) questions about a settled item's receipt;
//! the provider answers with probabilities. Providers are constructed from the
//! mailspace's `[judgment]` config only, authenticate exclusively via
//! `key_cmd` (never an envvar, never an inline key), and are consulted only
//! by the apply path — read paths have no provider parameter at all.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use vivi_error::VivariumError;

/// Judgment provider settings (the `[judgment]` table in `mailspace.toml`).
///
/// There is deliberately no inline key field: authentication is only via
/// `key_cmd`, mirroring the accounts `password_cmd` mechanism, so the secret
/// never lives in a config file or the process environment.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Judgment {
    /// Provider vendor; only `typesafe` is supported. `None` disables.
    pub provider: Option<String>,
    /// Endpoint URL override (vendor default when absent).
    pub endpoint: Option<String>,
    /// Model name (vendor default when absent).
    pub model: Option<String>,
    /// Request timeout in milliseconds (default 4000).
    pub timeout_ms: Option<u64>,
    /// Shell command whose stdout is the API key. Required when provider
    /// is set.
    pub key_cmd: Option<String>,
}

/// Default System One endpoint.
const DEFAULT_ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
/// Default model.
const DEFAULT_MODEL: &str = "jev-latest";
/// Default request timeout.
const DEFAULT_TIMEOUT_MS: u64 = 4000;

/// One typed question. V1 uses Noul (yes/no probability) only.
#[derive(Debug, Clone, Serialize)]
pub struct JudgmentQuestion {
    pub id: String,
    /// Question kind sent to the provider (`noul`).
    pub kind: &'static str,
    pub instructions: String,
    pub criteria_true: Option<String>,
    pub criteria_false: Option<String>,
}

impl JudgmentQuestion {
    /// Build a Noul question with symmetric criteria.
    #[must_use]
    pub fn noul(id: &str, instructions: &str, criteria_true: &str, criteria_false: &str) -> Self {
        Self {
            id: id.to_string(),
            kind: "noul",
            instructions: instructions.to_string(),
            criteria_true: Some(criteria_true.to_string()),
            criteria_false: Some(criteria_false.to_string()),
        }
    }
}

/// One provider answer (Noul probability, 0 = no .. 1 = yes).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct JudgmentAnswer {
    pub id: String,
    pub noul: f64,
}

/// A judgment provider: asks typed questions over state.
pub trait JudgmentProvider {
    fn name(&self) -> &'static str;
    fn model(&self) -> &str;

    /// Ask `questions` over `state`.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] classified as auth, timeout, http, or
    /// key_cmd failure. Error text never contains the API key.
    fn ask(
        &self,
        state: &Value,
        questions: &[JudgmentQuestion],
    ) -> Result<Vec<JudgmentAnswer>, VivariumError>;
}

/// TypeSafe System One provider (Jev).
#[derive(Debug, Clone)]
pub struct TypesafeProvider {
    endpoint: String,
    model: String,
    timeout: Duration,
    key: String,
}

impl TypesafeProvider {
    /// Build from the mailspace's `[judgment]` config. Returns `None` when the
    /// mailspace declares none, or declares no provider (feature off).
    ///
    /// # Errors
    /// Returns a [`VivariumError`] when the provider is set but the config
    /// is incomplete, the vendor is unknown, or `key_cmd` fails.
    pub fn from_config(config: Option<&Judgment>) -> Result<Option<Self>, VivariumError> {
        let Some(config) = config.filter(|entry| entry.provider.is_some()) else {
            return Ok(None);
        };
        let key = run_key_cmd(resolve_key_cmd(config)?)?;
        Ok(Some(Self {
            endpoint: config
                .endpoint
                .clone()
                .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string()),
            model: config
                .model
                .clone()
                .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            timeout: Duration::from_millis(config.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS)),
            key,
        }))
    }
}

/// Validate a `[judgment]` table and return the `key_cmd` to run.
fn resolve_key_cmd(config: &Judgment) -> Result<&str, VivariumError> {
    match config.provider.as_deref() {
        Some("typesafe") => {}
        Some(other) => {
            return Err(VivariumError::Config(format!(
                "judgment.provider '{other}' is not supported; only 'typesafe'"
            )));
        }
        None => {
            return Err(VivariumError::Config(
                "judgment.provider is set but no provider was resolved".into(),
            ));
        }
    }
    config.key_cmd.as_deref().ok_or_else(|| {
        VivariumError::Config(
            "judgment.provider is set but judgment.key_cmd is missing in mailspace.toml".into(),
        )
    })
}

impl JudgmentProvider for TypesafeProvider {
    fn name(&self) -> &'static str {
        "typesafe"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn ask(
        &self,
        state: &Value,
        questions: &[JudgmentQuestion],
    ) -> Result<Vec<JudgmentAnswer>, VivariumError> {
        let mut request_questions = serde_json::Map::new();
        for question in questions {
            let mut body = serde_json::Map::new();
            body.insert("type".into(), Value::String(question.kind.to_string()));
            body.insert(
                "instructions".into(),
                Value::String(question.instructions.clone()),
            );
            if let (Some(t), Some(f)) = (&question.criteria_true, &question.criteria_false) {
                body.insert(
                    "criteria".into(),
                    serde_json::json!({ "true": t, "false": f }),
                );
            }
            request_questions.insert(question.id.clone(), Value::Object(body));
        }
        let payload = serde_json::json!({
            "state": state,
            "model": self.model,
            "questions": request_questions,
        });
        let response = self.post(&payload)?;
        parse_answers(&response, questions)
    }
}

impl TypesafeProvider {
    fn post(&self, payload: &Value) -> Result<Value, VivariumError> {
        let endpoint = self.endpoint.clone();
        let key = self.key.clone();
        let timeout = self.timeout;
        let payload = payload.clone();
        let handle = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| VivariumError::Other(format!("failed to build runtime: {e}")))?;
            runtime.block_on(async move {
                let client = reqwest::Client::builder()
                    .timeout(timeout)
                    .build()
                    .map_err(|e| VivariumError::Other(format!("failed to build client: {e}")))?;
                let response = client
                    .post(&endpoint)
                    .bearer_auth(&key)
                    .json(&payload)
                    .send()
                    .await
                    .map_err(|e| {
                        if e.is_timeout() {
                            VivariumError::Message(format!(
                                "judgment provider timed out after {}ms",
                                timeout.as_millis()
                            ))
                        } else {
                            VivariumError::Message(format!("judgment provider unreachable: {e}"))
                        }
                    })?;
                let status = response.status();
                if !status.is_success() {
                    return Err(match status.as_u16() {
                        401 | 403 => {
                            VivariumError::Message("judgment provider rejected credentials".into())
                        }
                        code => {
                            VivariumError::Message(format!("judgment provider error (HTTP {code})"))
                        }
                    });
                }
                response
                    .json::<Value>()
                    .await
                    .map_err(|e| VivariumError::Message(format!("invalid provider response: {e}")))
            })
        });
        handle
            .join()
            .map_err(|_| VivariumError::Message("judgment provider thread panicked".into()))?
    }
}

/// Execute `key_cmd` via `sh -c`, mirroring the accounts `password_cmd`
/// mechanism. Failures report stderr only — never the secret.
fn run_key_cmd(command: &str) -> Result<String, VivariumError> {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .map_err(|e| VivariumError::Config(format!("judgment key_cmd failed to start: {e}")))?;
    if !output.status.success() {
        return Err(VivariumError::Config(format!(
            "judgment key_cmd failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let key = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if key.is_empty() {
        return Err(VivariumError::Config(
            "judgment key_cmd produced an empty key".into(),
        ));
    }
    Ok(key)
}

fn parse_answers(
    response: &Value,
    questions: &[JudgmentQuestion],
) -> Result<Vec<JudgmentAnswer>, VivariumError> {
    let answers = response
        .get("answers")
        .and_then(Value::as_object)
        .ok_or_else(|| VivariumError::Message("provider response has no answers object".into()))?;
    let mut out = Vec::with_capacity(questions.len());
    for question in questions {
        let answer = answers
            .get(&question.id)
            .and_then(|a| a.get("noul"))
            .and_then(Value::as_f64)
            .ok_or_else(|| {
                VivariumError::Message(format!(
                    "provider response missing noul answer for '{}'",
                    question.id
                ))
            })?;
        out.push(JudgmentAnswer {
            id: question.id.clone(),
            noul: answer,
        });
    }
    Ok(out)
}

#[cfg(test)]
#[path = "judgment_test.rs"]
mod tests;
