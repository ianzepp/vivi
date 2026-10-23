//! Flow settings for the `[pump.flow]` and `[pump.slice]` tables.
//!
//! Unset `max_live_runs` means no cap on live runs; unset slice `minutes`
//! means [`DEFAULT_SLICE_MINUTES`].

/// Default slice size: aim for 5 to this many minutes per slice.
pub const DEFAULT_SLICE_MINUTES: u32 = 10;

/// Flow settings (the `[pump.flow]` table).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FlowConfig {
    /// Most runs that may be starting, live or restarting at once; unset
    /// means no cap.
    pub max_live_runs: Option<u32>,
}

impl FlowConfig {
    /// Whether every key still has its default (nothing was written).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Slice settings (the `[pump.slice]` table).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SliceConfig {
    /// Upper end of the slice size the slicer aims for, in minutes; unset
    /// means [`DEFAULT_SLICE_MINUTES`].
    pub minutes: Option<u32>,
}

impl SliceConfig {
    /// Whether every key still has its default (nothing was written).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The slice size in minutes: the set value or the default.
    #[must_use]
    pub fn minutes(&self) -> u32 {
        self.minutes.unwrap_or(DEFAULT_SLICE_MINUTES)
    }
}

#[cfg(test)]
#[path = "flow_test.rs"]
mod tests;
