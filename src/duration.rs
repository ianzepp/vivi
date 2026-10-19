//! Shared human interval parsing (`30s`, `5m`, `1h`, `1d`, bare seconds).

use std::time::Duration;

use crate::error::VivariumError;

/// Parse a positive duration from a human interval string.
///
/// Accepted forms: bare seconds (`30`), or a number with unit `s` / `m` / `h`
/// / `d` (`30s`, `5m`, `1h`, `7d`). Zero and unknown units are rejected.
///
/// # Errors
/// Returns a config error when the value cannot be parsed as a positive interval.
pub fn parse_duration(value: &str) -> Result<Duration, VivariumError> {
    let value = value.trim();
    let Some((number, unit)) = split_duration(value) else {
        return Err(invalid_duration(value));
    };
    let amount = number.parse::<u64>().map_err(|_| invalid_duration(value))?;
    if amount == 0 {
        return Err(VivariumError::Config(
            "duration must be greater than zero".into(),
        ));
    }
    let seconds = match unit {
        "" | "s" => amount,
        "m" => amount.saturating_mul(60),
        "h" => amount.saturating_mul(60 * 60),
        "d" => amount.saturating_mul(60 * 60 * 24),
        _ => {
            return Err(VivariumError::Config(format!(
                "invalid duration unit '{unit}'; use s, m, or h"
            )));
        }
    };
    Ok(Duration::from_secs(seconds))
}

/// Parse a duration and return whole seconds.
///
/// # Errors
/// Same as [`parse_duration`].
pub fn parse_duration_secs(value: &str) -> Result<u64, VivariumError> {
    Ok(parse_duration(value)?.as_secs())
}

fn split_duration(value: &str) -> Option<(&str, &str)> {
    let first_unit = value
        .char_indices()
        .find_map(|(index, ch)| (!ch.is_ascii_digit()).then_some(index))
        .unwrap_or(value.len());
    if first_unit == 0 {
        return None;
    }
    Some((&value[..first_unit], &value[first_unit..]))
}

fn invalid_duration(value: &str) -> VivariumError {
    VivariumError::Config(format!(
        "invalid duration '{value}'; use values like 30s, 5m, or 1h"
    ))
}

#[cfg(test)]
#[path = "duration_test.rs"]
mod tests;
