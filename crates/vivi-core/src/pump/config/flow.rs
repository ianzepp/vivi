//! Flow settings for the `[pump.flow]` and `[pump.slice]` tables.
//!
//! Unset `max_live_runs` means no cap on live runs; unset slice `minutes`
//! means [`DEFAULT_SLICE_MINUTES`]; an unset `[pump.flow.stages]` switch
//! means the stage runs.

/// Default slice size: aim for 5 to this many minutes per slice.
pub const DEFAULT_SLICE_MINUTES: u32 = 10;

/// Flow settings (the `[pump.flow]` table).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FlowConfig {
    /// Most non-repair runs that may be starting, live or restarting at once;
    /// unset means no cap. Repair-kind runs neither count toward the cap nor
    /// wait for it.
    pub max_live_runs: Option<u32>,
    /// Build-flow stages that can be switched off.
    #[serde(skip_serializing_if = "StagesConfig::is_empty")]
    pub stages: StagesConfig,
}

impl FlowConfig {
    /// Whether every key still has its default (nothing was written).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Build-flow stage switches (the `[pump.flow.stages]` table). A stage that is
/// switched off starts no job: its step answers at once by rule, with a
/// payload that records the skip. Unset means on.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StagesConfig {
    /// The admitter steps; off admits the plan as written.
    pub admit: Option<bool>,
    /// The audit steps; off answers `skipped`, which the join reads as no findings.
    pub audit: Option<bool>,
    /// The judge steps; off answers `block`, so a red need rung goes to rework.
    pub judge: Option<bool>,
    /// The need rung steps; off answers `skipped`, which routes like `pass`.
    pub need_rung: Option<bool>,
}

impl StagesConfig {
    /// Whether every key still has its default (nothing was written).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Whether the stage named `stage` runs: the set value, else on. An
    /// unknown name runs.
    #[must_use]
    pub fn runs(&self, stage: &str) -> bool {
        let flag = match stage {
            "admit" => self.admit,
            "audit" => self.audit,
            "judge" => self.judge,
            "need_rung" => self.need_rung,
            _ => None,
        };
        flag.unwrap_or(true)
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
