//! The unit fields `pump.db` keeps inside the `units.extra_json` column.
//!
//! The quarantine id, the dispatch base, the solo flag, the task-send
//! exception flag, the transient-retry bookkeeping and the split-planner
//! bookkeeping are real fields on
//! [`Unit`], but they are rare, so they
//! ride in the same JSON object that holds a unit's unknown keys instead of
//! getting columns (and a schema bump). [`pack`] writes them next to the unknown keys; [`lift`] takes them
//! back out, so `Unit::extra` never carries them.

use serde_json::Value;

use crate::error::VivariumError;
use crate::pump::state::Unit;

/// The `extra_json` key of [`Unit::quarantine`].
const QUARANTINE_KEY: &str = "quarantine";
/// The `extra_json` key of [`Unit::repairs`].
const REPAIRS_KEY: &str = "repairs";
/// The `extra_json` key of [`Unit::base`].
const BASE_KEY: &str = "base";
/// The `extra_json` key of [`Unit::solo`].
const SOLO_KEY: &str = "solo";
/// The `extra_json` key of [`Unit::send_reported`].
const SEND_REPORTED_KEY: &str = "send_reported";
/// The `extra_json` key of [`Unit::transient_retries`].
const TRANSIENT_RETRIES_KEY: &str = "transient_retries";
/// The `extra_json` key of [`Unit::retry_not_before`].
const RETRY_NOT_BEFORE_KEY: &str = "retry_not_before";
/// The `extra_json` key of [`Unit::ready_at`].
const READY_AT_KEY: &str = "ready_at";
/// The `extra_json` key of [`Unit::split_fails`].
const SPLIT_FAILS_KEY: &str = "split_fails";
/// The `extra_json` key of [`Unit::split_retry_at`].
const SPLIT_RETRY_AT_KEY: &str = "split_retry_at";
/// The `extra_json` key of [`Unit::split_reasks`].
const SPLIT_REASKS_KEY: &str = "split_reasks";

/// The `extra_json` text for `unit`: its unknown keys plus the set fields.
pub(super) fn pack(unit: &Unit) -> Result<String, VivariumError> {
    let mut extra = unit.extra.clone();
    if let Some(id) = &unit.quarantine {
        extra.insert(QUARANTINE_KEY.to_string(), Value::String(id.clone()));
    }
    if let Some(id) = &unit.repairs {
        extra.insert(REPAIRS_KEY.to_string(), Value::String(id.clone()));
    }
    if let Some(base) = &unit.base {
        extra.insert(BASE_KEY.to_string(), Value::String(base.clone()));
    }
    if unit.solo {
        extra.insert(SOLO_KEY.to_string(), Value::Bool(true));
    }
    if unit.send_reported {
        extra.insert(SEND_REPORTED_KEY.to_string(), Value::Bool(true));
    }
    if unit.transient_retries > 0 {
        extra.insert(
            TRANSIENT_RETRIES_KEY.to_string(),
            Value::from(unit.transient_retries),
        );
    }
    if let Some(secs) = unit.retry_not_before {
        extra.insert(RETRY_NOT_BEFORE_KEY.to_string(), Value::from(secs));
    }
    if let Some(secs) = unit.ready_at {
        extra.insert(READY_AT_KEY.to_string(), Value::from(secs));
    }
    if unit.split_fails > 0 {
        extra.insert(SPLIT_FAILS_KEY.to_string(), Value::from(unit.split_fails));
    }
    if let Some(secs) = unit.split_retry_at {
        extra.insert(SPLIT_RETRY_AT_KEY.to_string(), Value::from(secs));
    }
    if unit.split_reasks > 0 {
        extra.insert(SPLIT_REASKS_KEY.to_string(), Value::from(unit.split_reasks));
    }
    serde_json::to_string(&extra)
        .map_err(|err| VivariumError::Other(format!("pump.db encode unit extra: {err}")))
}

/// Move the keys [`pack`] wrote out of `unit.extra` into their fields.
pub(super) fn lift(unit: &mut Unit) {
    if let Some(Value::String(id)) = unit.extra.remove(QUARANTINE_KEY) {
        unit.quarantine = Some(id);
    }
    if let Some(Value::String(id)) = unit.extra.remove(REPAIRS_KEY) {
        unit.repairs = Some(id);
    }
    if let Some(Value::String(base)) = unit.extra.remove(BASE_KEY) {
        unit.base = Some(base);
    }
    if let Some(Value::Bool(solo)) = unit.extra.remove(SOLO_KEY) {
        unit.solo = solo;
    }
    if let Some(Value::Bool(flag)) = unit.extra.remove(SEND_REPORTED_KEY) {
        unit.send_reported = flag;
    }
    if let Some(retries) = unit
        .extra
        .remove(TRANSIENT_RETRIES_KEY)
        .and_then(|value| value.as_u64())
    {
        unit.transient_retries = u32::try_from(retries).unwrap_or(u32::MAX);
    }
    if let Some(secs) = unit
        .extra
        .remove(RETRY_NOT_BEFORE_KEY)
        .and_then(|value| value.as_u64())
    {
        unit.retry_not_before = Some(secs);
    }
    if let Some(secs) = unit
        .extra
        .remove(READY_AT_KEY)
        .and_then(|value| value.as_u64())
    {
        unit.ready_at = Some(secs);
    }
    if let Some(fails) = unit
        .extra
        .remove(SPLIT_FAILS_KEY)
        .and_then(|value| value.as_u64())
    {
        unit.split_fails = u32::try_from(fails).unwrap_or(u32::MAX);
    }
    if let Some(secs) = unit
        .extra
        .remove(SPLIT_RETRY_AT_KEY)
        .and_then(|value| value.as_u64())
    {
        unit.split_retry_at = Some(secs);
    }
    if let Some(reasks) = unit
        .extra
        .remove(SPLIT_REASKS_KEY)
        .and_then(|value| value.as_u64())
    {
        unit.split_reasks = u32::try_from(reasks).unwrap_or(u32::MAX);
    }
}
