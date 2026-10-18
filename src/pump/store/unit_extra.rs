//! The unit fields `pump.db` keeps inside the `units.extra_json` column.
//!
//! The quarantine id, the dispatch base and the solo flag are real fields on
//! [`Unit`], but they are rare, so they ride in the same JSON object that
//! holds a unit's unknown keys instead of getting columns (and a schema
//! bump). [`pack`] writes them next to the unknown keys; [`lift`] takes them
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
}
