//! Ages and dates: the start of a pull window (`--since <age|date>`) and the
//! age `vivi sync prune --older-than` counts back from.

use std::time::Duration;

use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};

use crate::duration::parse_duration;
use crate::error::VivariumError;

/// Parse an age such as `30d`, `12h`, `90m`. Days are accepted here because a
/// letter's age is naturally counted in days; every other form goes through
/// the shared interval parser.
///
/// # Errors
/// Returns a config error for a zero, malformed, or unknown-unit age.
pub fn parse_age(value: &str) -> Result<Duration, VivariumError> {
    let trimmed = value.trim();
    if let Some(days) = trimmed.strip_suffix('d') {
        return match days.parse::<u64>() {
            Ok(days) if days > 0 => Ok(Duration::from_secs(days.saturating_mul(24 * 60 * 60))),
            _ => Err(VivariumError::Config(format!(
                "invalid age '{value}'; use values like 30d, 12h, or 90m"
            ))),
        };
    }
    parse_duration(trimmed)
}

/// Resolve a `--since` value to an RFC 3339 UTC instant (whole seconds).
///
/// Accepted forms: an age counted back from `now` (`30d`, `12h`, `90m`), an
/// RFC 3339 time, or a `YYYY-MM-DD` date (midnight UTC).
///
/// # Errors
/// Returns a config error naming the accepted forms for anything else.
pub fn parse_since(value: &str, now: DateTime<Utc>) -> Result<String, VivariumError> {
    let trimmed = value.trim();
    let invalid = || {
        VivariumError::Config(format!(
            "invalid --since '{value}'; use an age (30d, 12h, 90m), an RFC 3339 time, or a \
             YYYY-MM-DD date"
        ))
    };
    if let Ok(time) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(format(time.with_timezone(&Utc)));
    }
    if let Ok(date) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        let midnight = date.and_hms_opt(0, 0, 0).ok_or_else(invalid)?;
        return Ok(format(midnight.and_utc()));
    }
    let age = parse_age(trimmed).map_err(|_| invalid())?;
    let span = chrono::Duration::from_std(age).map_err(|_| invalid())?;
    let start = now.checked_sub_signed(span).ok_or_else(invalid)?;
    Ok(format(start))
}

fn format(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
#[path = "window_test.rs"]
mod tests;
