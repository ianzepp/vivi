//! Append-only audit trail for server-key MCP calls (wave 3 D2): one
//! JSON object per line in `DATA_DIR/audit.jsonl` — one line per write
//! tool call and per key/grant change. Reads are not logged. The file is
//! 0600 on create and appends with a single write; no fsync. There is no
//! read API yet.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::server::auth::rfc3339;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuditOutcome {
    Ok,
    Refused,
    Error,
}

/// One audit line, in wire order: `at`, `key_id`, `principal`,
/// `project_id`, `tool`, `outcome`, `summary`. `key_id` and `project_id`
/// are written as `null`, never omitted, as the contract pins; `principal`
/// is absent when there is none.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditRecord {
    pub at: String,
    #[serde(default)]
    pub key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    pub tool: String,
    pub outcome: AuditOutcome,
    pub summary: String,
}

/// The write half of [`AuditRecord`]; `at` is stamped by
/// [`AuditLog::record`].
#[derive(Debug, Clone)]
pub struct AuditEvent {
    pub key_id: Option<String>,
    pub principal: Option<String>,
    pub project_id: Option<String>,
    pub tool: String,
    pub outcome: AuditOutcome,
    pub summary: String,
}

/// The audit log at `DATA_DIR/audit.jsonl`. Clone-safe; appends are
/// serialized behind one lock.
#[derive(Clone)]
pub struct AuditLog {
    lock: Arc<Mutex<()>>,
    path: PathBuf,
}

impl AuditLog {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            lock: Arc::new(Mutex::new(())),
            path: data_dir.join("audit.jsonl"),
        }
    }

    /// Append one record as one line. Fails without writing when the
    /// record cannot be encoded or the file cannot be opened.
    pub fn record(&self, event: AuditEvent) -> Result<(), String> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let record = AuditRecord {
            at: rfc3339(chrono::Utc::now()),
            key_id: event.key_id,
            principal: event.principal,
            project_id: event.project_id,
            tool: event.tool,
            outcome: event.outcome,
            summary: event.summary,
        };
        let mut line = serde_json::to_string(&record)
            .map_err(|e| format!("failed to encode audit record: {e}"))?;
        line.push('\n');
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to prepare audit dir: {e}"))?;
        }
        append_private(&self.path, line.as_bytes())
            .map_err(|e| format!("failed to append audit record: {e}"))
    }
}

#[cfg(unix)]
fn append_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}

#[cfg(not(unix))]
fn append_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(bytes)
}
