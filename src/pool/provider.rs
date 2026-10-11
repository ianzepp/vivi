//! The provider seam: what the pool needs from a machine host.

use std::collections::BTreeMap;

use super::doctor::Check;
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

impl MachineRecord {
    /// What the provider's answer for a machine that no longer exists reads
    /// as: state `destroyed`, nothing else known.
    #[must_use]
    pub fn gone(id: &str) -> Self {
        Self {
            id: id.to_string(),
            name: String::new(),
            state: "destroyed".to_string(),
            region: String::new(),
            created_at: String::new(),
            metadata: BTreeMap::new(),
            exit_code: None,
        }
    }
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
    /// Make sure the pool's application exists, creating it in `organization`
    /// when it does not. True when this call created it.
    ///
    /// # Errors
    /// Returns an error if the provider cannot list or create applications.
    fn ensure_app(&self, organization: &str) -> Result<bool, VivariumError>;

    /// The provider's own health checks for `vivi pool doctor`: read-only,
    /// under the provider's own heading, each failure naming its fix. The
    /// harness environment names are the ones the application must hold as
    /// secrets.
    fn doctor(&self, harness_env: &[String]) -> Vec<Check>;

    /// The names (never the values) of the application's secrets.
    ///
    /// # Errors
    /// Returns an error if the provider cannot list secrets.
    fn secret_names(&self) -> Result<Vec<String>, VivariumError>;

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

    /// The shell command that removes one machine by hand, for a report that
    /// cannot remove it itself.
    fn destroy_command(&self, id: &str) -> String;

    /// One machine as the provider reports it right now. A machine that no
    /// longer exists is not an error: it reads as [`MachineRecord::gone`]
    /// (state `destroyed`), so a dispatch whose machine vanished can be closed.
    ///
    /// # Errors
    /// Returns an error if the provider fails.
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
