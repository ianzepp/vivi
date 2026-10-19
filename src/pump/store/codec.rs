//! JSON and numeric codecs shared by the pump store and its table modules.

use crate::error::VivariumError;

pub(super) fn json<T: serde::Serialize>(value: &T) -> Result<String, VivariumError> {
    serde_json::to_string(value).map_err(|err| VivariumError::Message(err.to_string()))
}

pub(super) fn json_or_null<T: serde::Serialize>(value: &T) -> Result<String, VivariumError> {
    json(value)
}

pub(super) fn from_json<T: Default + serde::de::DeserializeOwned>(text: String) -> T {
    serde_json::from_str(&text).unwrap_or_default()
}

pub(super) fn from_opt_json<T: serde::de::DeserializeOwned>(text: Option<String>) -> Option<T> {
    text.and_then(|text| match serde_json::from_str(&text) {
        Ok(value) => Some(value),
        Err(_) => None,
    })
}

pub(super) fn from_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

pub(super) fn from_u32(value: i64) -> u32 {
    u32::try_from(value).unwrap_or(0)
}

pub(super) fn db_err<E: std::fmt::Display>(what: &'static str) -> impl FnOnce(E) -> VivariumError {
    move |err| VivariumError::Other(format!("pump.db {what}: {err}"))
}
