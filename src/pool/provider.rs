//! The provider seam: what the pool needs from a machine host.

use std::collections::BTreeMap;

use crate::error::VivariumError;

/// One machine as the provider reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineRecord {
    pub id: String,
    pub name: String,
    /// Provider state word, e.g. `started`, `stopped`.
    pub state: String,
    pub region: String,
    pub created_at: String,
    pub metadata: BTreeMap<String, String>,
    /// Exit code of the machine's most recent process exit, when known.
    pub exit_code: Option<i64>,
}

/// Everything needed to create and start one slot.
#[derive(Clone)]
pub struct CreateSpec {
    pub name: String,
    pub region: String,
    pub image: String,
    pub vm_size: String,
    pub memory_mb: u32,
    pub metadata: BTreeMap<String, String>,
    /// Environment for the machine. May carry a secret: never print it.
    pub env: BTreeMap<String, String>,
    /// `(guest path, contents)` files written into the machine.
    pub files: Vec<(String, String)>,
    /// Command the machine runs on every boot.
    pub command: Vec<String>,
}

impl std::fmt::Debug for CreateSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateSpec")
            .field("name", &self.name)
            .field("region", &self.region)
            .field("image", &self.image)
            .field("metadata", &self.metadata)
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// A machine host. Implementations must keep secrets out of errors.
pub trait PoolProvider {
    /// All machines in the pool's app, pool members or not.
    ///
    /// # Errors
    /// Returns an error if the provider cannot be reached or answers badly.
    fn list(&self) -> Result<Vec<MachineRecord>, VivariumError>;

    /// Create and start one machine; returns its id without waiting for the
    /// machine to finish booting.
    ///
    /// # Errors
    /// Returns an error if the machine could not be created.
    fn create(&self, spec: &CreateSpec) -> Result<String, VivariumError>;

    /// Destroy one machine by id, whatever its state.
    ///
    /// # Errors
    /// Returns an error if the machine could not be destroyed.
    fn destroy(&self, id: &str) -> Result<(), VivariumError>;
}
