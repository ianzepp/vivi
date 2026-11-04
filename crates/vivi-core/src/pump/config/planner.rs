//! Split-planner settings for the `[pump.planner]` table.

/// Planner settings (the `[pump.planner]` table).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
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
}

impl Default for PlannerConfig {
    fn default() -> Self {
        Self {
            run: String::new(),
            charter: String::new(),
            split_hard_cap: 5400,
            planners_max: 3,
        }
    }
}
