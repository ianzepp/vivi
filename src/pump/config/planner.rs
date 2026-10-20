//! Split-planner settings for the `[pump.planner]` table.

/// Planner settings (the `[pump.planner]` table).
#[derive(Debug, Clone, serde::Serialize)]
pub struct PlannerConfig {
    /// Retired local command setting; durable planner jobs use the pump-planner role.
    pub run: String,
    /// Retired local charter path; durable planner jobs use the role charter.
    pub charter: String,
    /// Seconds a receipt-less (alive or unreachable) split planner may run
    /// before the unit is stuck; a planner that exits is retried instead.
    pub split_hard_cap: u64,
    /// Concurrent split planners; further split requests stagger their launch.
    pub planners_max: u32,
    /// The legacy `split_timeout` key was read; validate warns and migrate
    /// rewrites it.
    #[serde(skip)]
    pub split_timeout_present: bool,
}

impl Default for PlannerConfig {
    fn default() -> Self {
        Self {
            run: String::new(),
            charter: String::new(),
            split_hard_cap: 5400,
            planners_max: 3,
            split_timeout_present: false,
        }
    }
}

impl<'de> serde::Deserialize<'de> for PlannerConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Default, serde::Deserialize)]
        #[serde(default)]
        struct Raw {
            run: String,
            charter: String,
            split_hard_cap: Option<u64>,
            planners_max: Option<u32>,
            #[serde(rename = "split_timeout")]
            split_timeout: Option<u64>,
        }
        let raw = <Raw as serde::Deserialize>::deserialize(deserializer)?;
        Ok(Self {
            run: raw.run,
            charter: raw.charter,
            split_hard_cap: raw.split_hard_cap.or(raw.split_timeout).unwrap_or(5400),
            planners_max: raw.planners_max.unwrap_or(3),
            split_timeout_present: raw.split_timeout.is_some(),
        })
    }
}
