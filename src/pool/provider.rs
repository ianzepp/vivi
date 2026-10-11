//! The provider seam: what the pool needs from a machine host.

use std::collections::BTreeMap;

use crate::error::VivariumError;

/// A held lease on one machine: the proof of an exclusive claim window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub nonce: String,
}

/// What a command run inside a machine produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i64,
}

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
/// Methods after `destroy` act on one machine by id.
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

    /// One machine as the provider reports it right now.
    ///
    /// # Errors
    /// Returns an error if the machine is unknown or the provider fails.
    fn machine(&self, id: &str) -> Result<MachineRecord, VivariumError>;

    /// Take the machine's lease for `ttl_secs`. `Ok(None)` means another
    /// holder has it: the atomic part of a claim.
    ///
    /// # Errors
    /// Returns an error if the provider fails for any other reason.
    fn lease(&self, id: &str, ttl_secs: u32) -> Result<Option<Lease>, VivariumError>;

    /// Give a lease back.
    ///
    /// # Errors
    /// Returns an error if the provider refuses the release.
    fn release(&self, id: &str, lease: &Lease) -> Result<(), VivariumError>;

    /// Set one metadata key.
    ///
    /// # Errors
    /// Returns an error if the provider refuses the write.
    fn set_metadata(&self, id: &str, key: &str, value: &str) -> Result<(), VivariumError>;

    /// Remove one metadata key; absent is not an error.
    ///
    /// # Errors
    /// Returns an error if the provider refuses the delete.
    fn clear_metadata(&self, id: &str, key: &str) -> Result<(), VivariumError>;

    /// Replace the machine's config file at `path` (a stopped machine stays
    /// stopped), holding `lease`. The file lands on the next boot.
    ///
    /// # Errors
    /// Returns an error if the update is refused.
    fn put_file(
        &self,
        id: &str,
        lease: &Lease,
        path: &str,
        content: &[u8],
    ) -> Result<(), VivariumError>;

    /// In one config update of a stopped machine (which stays stopped): drop
    /// environment names and set one metadata key. One update, because a
    /// config update replaces the machine instance and would overwrite a
    /// metadata write made alongside it.
    ///
    /// # Errors
    /// Returns an error if the update is refused.
    fn seal(
        &self,
        id: &str,
        drop_env: &[&str],
        key: &str,
        value: &str,
    ) -> Result<(), VivariumError>;

    /// Start a stopped machine; returns once the provider accepted it.
    ///
    /// # Errors
    /// Returns an error if the machine cannot be started.
    fn start(&self, id: &str) -> Result<(), VivariumError>;

    /// Stop a machine; returns once the provider accepted it.
    ///
    /// # Errors
    /// Returns an error if the machine cannot be stopped.
    fn stop(&self, id: &str) -> Result<(), VivariumError>;

    /// Run `script` under `bash -c` inside the running machine.
    ///
    /// # Errors
    /// Returns an error if the machine is not running or the call fails.
    fn exec(&self, id: &str, script: &str, timeout_secs: u32) -> Result<ExecOutput, VivariumError>;
}
