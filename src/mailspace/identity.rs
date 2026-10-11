use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use chrono::{DateTime, Utc};

use super::{MAILSPACE_CONFIG, Mailspace, write_config};
use crate::duration::parse_duration_secs;
use crate::error::VivariumError;
use crate::role_schedule::{self, LastSignal, ScheduleReport};
use crate::storage::Storage;

/// Preferred lifecycle values; freeform strings are still accepted.
pub const ROLE_STATUS_ACTIVE: &str = "active";
pub const ROLE_STATUS_PARKED: &str = "parked";
pub const ROLE_STATUS_RETIRED: &str = "retired";

/// Preferred harness value for parent-TUI / spawned child execution.
pub const ROLE_HARNESS_SUBAGENT: &str = "subagent";

/// A mailspace seat: mailbox name plus durable role metadata.
///
/// Stored under `[[identities]]` in `mailspace.toml` for backward compatibility
/// with existing mailspaces. Charter bodies live in `.vivi/charters/<name>.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalIdentity {
    pub name: String,
    /// Former names this role was known by. Rename never rewrites message rows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Process class (`hand`, `head`, `mind`, `operator`, `steward`, or freeform).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Freeform tag slugs (`auditor`, `floater`, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// Lifecycle status. Default `active` for legacy configs missing the field.
    #[serde(default = "default_role_status")]
    pub status: String,
    /// Execution home (`subagent`, `tmux`, `vivi_pty`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// Inference provider / account lane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model id for the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reasoning / thinking effort level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    /// Process id of the live process occupying this seat (self-set by the role).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Host where `pid` lives. Defaults to the local hostname when `pid` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Desired maximum silence between outbound signals (`5m`, `30m`, `2h`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cadence: Option<String>,
}

fn default_role_status() -> String {
    ROLE_STATUS_ACTIVE.to_string()
}

/// Partial update for `role set`. Only `Some` fields are applied.
#[derive(Debug, Clone, Default)]
pub struct RoleUpdate {
    pub kind: Option<Option<String>>,
    pub status: Option<String>,
    pub harness: Option<Option<String>>,
    pub provider: Option<Option<String>>,
    pub model: Option<Option<String>>,
    pub thinking: Option<Option<String>>,
    pub pid: Option<Option<u32>>,
    pub host: Option<Option<String>>,
    pub cadence: Option<Option<String>>,
    pub add_labels: Vec<String>,
    pub clear_labels: Vec<String>,
}

/// JSON/text view of a role, including derived address and charter body.
#[derive(Debug, Clone, Serialize)]
pub struct RoleView {
    pub name: String,
    pub address: String,
    pub aliases: Vec<String>,
    pub kind: Option<String>,
    pub labels: Vec<String>,
    pub status: String,
    pub harness: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub pid: Option<u32>,
    pub host: Option<String>,
    pub cadence: Option<String>,
    pub charter: String,
    pub has_charter: bool,
}

impl LocalIdentity {
    fn new(name: String) -> Self {
        Self {
            name,
            aliases: Vec::new(),
            kind: None,
            labels: Vec::new(),
            status: default_role_status(),
            harness: None,
            provider: None,
            model: None,
            thinking: None,
            pid: None,
            host: None,
            cadence: None,
        }
    }
}

impl Mailspace {
    /// Add a thin identity with default role values (no kind, labels, or harness).
    ///
    /// Errors if the name already exists. To create a full role seat in one
    /// step with metadata, use [`Self::add_role`] with a populated [`RoleUpdate`].
    ///
    /// # Errors
    /// Returns an error if the identity name is invalid, already exists, or
    /// config persistence fails.
    pub fn add_identity(&mut self, identity: &str) -> Result<String, VivariumError> {
        self.add_role(identity, RoleUpdate::default())
    }

    /// Add a role seat: create the identity and apply `update` in one write.
    ///
    /// Errors if the name already exists; update an existing role with
    /// [`Self::set_role`] instead.
    ///
    /// # Errors
    /// Returns an error if the name is invalid, the name already exists, a
    /// field value is invalid, or config persistence fails.
    pub fn add_role(&mut self, name: &str, update: RoleUpdate) -> Result<String, VivariumError> {
        let name = sanitize_identity(name)?;
        if self.find_role_index(&name).is_some() {
            return Err(VivariumError::Message(format!(
                "local role '{name}' already exists; update it with `vivi role set {name} <fields>`"
            )));
        }
        let mut role = LocalIdentity::new(name.clone());
        apply_role_update(&mut role, update)?;
        self.config.identities.push(role);
        self.sort_roles();
        self.persist_config()?;
        Ok(self.address_for(&name))
    }

    /// Update a role's metadata fields from a `RoleUpdate`.
    ///
    /// # Errors
    /// Returns an error if the role name is unknown, field values are invalid,
    /// or config persistence fails.
    pub fn set_role(&mut self, name: &str, update: RoleUpdate) -> Result<RoleView, VivariumError> {
        let name = sanitize_identity(name)?;
        let index = self.require_role_index(&name)?;
        apply_role_update(&mut self.config.identities[index], update)?;
        self.persist_config()?;
        self.role_view(&self.config.identities[index].name.clone())
    }

    /// Rename an identity. The old name is preserved as an alias.
    ///
    /// # Errors
    /// Returns an error if either name is invalid, the role is unknown, the new
    /// name is taken, config persistence fails, or the charter file cannot be
    /// renamed.
    pub fn rename_identity(&mut self, old: &str, new: &str) -> Result<String, VivariumError> {
        let old = sanitize_identity(old)?;
        let new = sanitize_identity(new)?;
        let index = self.require_role_index(&old)?;
        if self.config.identities[index].name == new {
            return Err(VivariumError::Message(format!(
                "role is already named '{new}'"
            )));
        }
        if self.name_taken(&new, Some(index)) {
            return Err(VivariumError::Message(format!(
                "local role '{new}' already exists"
            )));
        }
        let previous_name = self.config.identities[index].name.clone();
        self.config.identities[index].name.clone_from(&new);
        if !self.config.identities[index]
            .aliases
            .iter()
            .any(|a| a == &previous_name)
        {
            self.config.identities[index]
                .aliases
                .push(previous_name.clone());
        }
        self.sort_roles();
        self.persist_config()?;
        rename_charter_file(&self.dir, &previous_name, &new)?;
        Ok(self.address_for(&new))
    }

    /// The address this mailspace mints for a role: `role@<prefix>.local`.
    #[must_use]
    pub fn address_for(&self, identity: &str) -> String {
        format!("{identity}@{}", self.address_domain())
    }

    #[must_use]
    pub fn identity_names(&self, canonical: &str) -> HashSet<String> {
        let mut names = HashSet::new();
        names.insert(canonical.to_string());
        if let Some(identity) = self
            .config
            .identities
            .iter()
            .find(|known| known.name == canonical)
        {
            names.extend(identity.aliases.iter().cloned());
        }
        names
    }

    /// Whether `account` is a role name or alias of this mailspace.
    #[must_use]
    pub(super) fn is_role_account(&self, account: &str) -> bool {
        self.find_role_index(account).is_some()
    }

    /// Whether the role named `name` (or an alias of it) is retired.
    #[must_use]
    pub(crate) fn role_is_retired(&self, name: &str) -> bool {
        self.find_role_by_name_or_alias(name)
            .is_some_and(|role| role.status == ROLE_STATUS_RETIRED)
    }

    /// Canonical identity name owning a (possibly historical) account name.
    /// Messages keep the account they were ingested under, so a renamed
    /// identity still owns its old name's rows.
    pub(super) fn identity_owning_account(&self, account: &str) -> Option<String> {
        self.config.identities.iter().find_map(|identity| {
            self.identity_names(&identity.name)
                .contains(account)
                .then(|| identity.name.clone())
        })
    }

    /// Whether a role is of kind `mind`: the kind that closes and reopens
    /// work it did not author.
    pub(super) fn identity_is_mind(&self, identity: &str) -> bool {
        self.find_role_by_name_or_alias(identity)
            .and_then(|role| role.kind.as_deref())
            .is_some_and(|kind| kind.eq_ignore_ascii_case("mind"))
    }

    /// Resolve a string to a canonical identity name. Accepts `name`, aliases,
    /// `name@local`, `name@<prefix>.local` (this mailspace's own address), and
    /// the legacy `name@<mailspace-name>.local`.
    ///
    /// # Errors
    /// Returns an error if the value is empty, references an external domain, or
    /// does not match any known role or alias.
    pub fn resolve_identity(&self, value: &str) -> Result<String, VivariumError> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(VivariumError::Message(
                "local identity cannot be empty".into(),
            ));
        }
        let identity = if let Some((local, domain)) = trimmed.rsplit_once('@') {
            if self.is_own_domain(domain) {
                local
            } else {
                return Err(VivariumError::Message(format!(
                    "external recipient '{trimmed}' is not allowed for local mailspace delivery"
                )));
            }
        } else {
            trimmed
        };
        let identity = sanitize_identity(identity)?;
        if let Some(known) = self.find_role_by_name_or_alias(&identity) {
            Ok(known.name.clone())
        } else {
            Err(VivariumError::Message(format!(
                "unknown local role '{identity}'; add it with `vivi role add {identity}`"
            )))
        }
    }

    /// Get the full `RoleView` for a role by name or alias.
    ///
    /// # Errors
    /// Returns an error if the role is unknown or the charter file cannot be
    /// read.
    pub fn role_view(&self, name: &str) -> Result<RoleView, VivariumError> {
        let canonical = self.resolve_identity(name)?;
        let role = self
            .find_role_by_name_or_alias(&canonical)
            .ok_or_else(|| VivariumError::Message(format!("unknown local role '{name}'")))?;
        let charter = self.read_charter(&role.name)?;
        Ok(RoleView {
            name: role.name.clone(),
            address: self.address_for(&role.name),
            aliases: role.aliases.clone(),
            kind: role.kind.clone(),
            labels: role.labels.clone(),
            status: role.status.clone(),
            harness: role.harness.clone(),
            provider: role.provider.clone(),
            model: role.model.clone(),
            thinking: role.thinking.clone(),
            pid: role.pid,
            host: role.host.clone(),
            cadence: role.cadence.clone(),
            has_charter: !charter.is_empty(),
            charter,
        })
    }

    /// List all role views.
    ///
    /// # Errors
    /// Returns an error if any role's charter file cannot be read.
    pub fn list_role_views(&self) -> Result<Vec<RoleView>, VivariumError> {
        let mut views = Vec::with_capacity(self.config.identities.len());
        for role in &self.config.identities {
            views.push(self.role_view(&role.name)?);
        }
        Ok(views)
    }

    /// # Errors
    /// Unknown role or storage query failure.
    pub fn schedule_report(&self, name: &str) -> Result<ScheduleReport, VivariumError> {
        self.schedule_report_with(&self.storage()?, name)
    }

    /// # Errors
    /// Returns an error if the role is unknown or storage cannot be queried.
    pub fn schedule_report_with(
        &self,
        storage: &Storage,
        name: &str,
    ) -> Result<ScheduleReport, VivariumError> {
        let canonical = self.resolve_identity(name)?;
        let cadence = self
            .find_role_by_name_or_alias(&canonical)
            .and_then(|role| role.cadence.as_deref());
        if cadence.is_none() {
            return Ok(role_schedule::evaluate(None, None, Utc::now()));
        }
        // Messages sent before the mailspace had an id carry the legacy
        // domain; both forms are this role's signal.
        let mut addresses: Vec<_> = self
            .identity_names(&canonical)
            .into_iter()
            .flat_map(|n| {
                [
                    self.address_for(&n),
                    format!("{n}@{}", self.legacy_domain()),
                ]
            })
            .collect();
        addresses.sort();
        addresses.dedup();
        let signal = storage
            .latest_message_from_addresses(&addresses)?
            .map(|m| LastSignal {
                at: DateTime::parse_from_rfc3339(&m.date).map_or_else(
                    |_| DateTime::from_timestamp(0, 0).unwrap_or_else(Utc::now),
                    |d| d.with_timezone(&Utc),
                ),
                handle: m.handle,
                local_role: m.local_role,
            });
        Ok(role_schedule::evaluate(
            cadence,
            signal.as_ref(),
            Utc::now(),
        ))
    }

    /// Write a charter body for a role.
    ///
    /// # Errors
    /// Returns an error if the role is unknown, the charter directory cannot be
    /// created, or the file cannot be written.
    pub fn set_charter(&mut self, name: &str, body: &str) -> Result<(), VivariumError> {
        let canonical = self.resolve_identity(name)?;
        let path = self.charter_path(&canonical);
        if let Some(parent) = path.parent() {
            crate::store::secure_create_dir_all(parent)?;
        }
        fs::write(&path, body).map_err(|e| {
            VivariumError::Message(format!(
                "failed to write charter for role '{canonical}' at {}: {e}",
                path.display()
            ))
        })?;
        Ok(())
    }

    /// Read the charter body for a role. Returns an empty string if no charter
    /// exists.
    ///
    /// # Errors
    /// Returns an error if the charter file exists but cannot be read.
    pub fn read_charter(&self, name: &str) -> Result<String, VivariumError> {
        let path = self.charter_path(name);
        if !path.exists() {
            return Ok(String::new());
        }
        fs::read_to_string(&path).map_err(|e| {
            VivariumError::Message(format!(
                "failed to read charter for role '{name}' at {}: {e}",
                path.display()
            ))
        })
    }

    fn charter_path(&self, name: &str) -> PathBuf {
        self.dir.join("charters").join(format!("{name}.md"))
    }

    fn persist_config(&self) -> Result<(), VivariumError> {
        write_config(&self.dir.join(MAILSPACE_CONFIG), &self.config)
    }

    fn sort_roles(&mut self) {
        self.config
            .identities
            .sort_by(|left, right| left.name.cmp(&right.name));
    }

    fn find_role_index(&self, name_or_alias: &str) -> Option<usize> {
        self.config.identities.iter().position(|known| {
            known.name == name_or_alias || known.aliases.iter().any(|a| a == name_or_alias)
        })
    }

    fn require_role_index(&self, name_or_alias: &str) -> Result<usize, VivariumError> {
        self.find_role_index(name_or_alias).ok_or_else(|| {
            VivariumError::Message(format!(
                "unknown local role '{name_or_alias}'; add it with `vivi role add {name_or_alias}`"
            ))
        })
    }

    fn find_role_by_name_or_alias(&self, name_or_alias: &str) -> Option<&LocalIdentity> {
        self.find_role_index(name_or_alias)
            .map(|index| &self.config.identities[index])
    }

    fn name_taken(&self, name: &str, except_index: Option<usize>) -> bool {
        self.config.identities.iter().enumerate().any(|(i, known)| {
            if except_index == Some(i) {
                return false;
            }
            known.name == name || known.aliases.iter().any(|a| a == name)
        })
    }
}

fn apply_role_update(role: &mut LocalIdentity, update: RoleUpdate) -> Result<(), VivariumError> {
    if let Some(kind) = update.kind {
        role.kind = match kind {
            Some(value) if !value.trim().is_empty() => {
                Some(sanitize_freeform_field("kind", &value)?)
            }
            _ => None,
        };
    }
    if let Some(status) = update.status {
        role.status = sanitize_freeform_field("status", &status)?;
    }
    if let Some(harness) = update.harness {
        role.harness = optional_field("harness", harness)?;
    }
    if let Some(provider) = update.provider {
        role.provider = optional_field("provider", provider)?;
    }
    if let Some(model) = update.model {
        role.model = optional_field("model", model)?;
    }
    if let Some(thinking) = update.thinking {
        role.thinking = optional_field("thinking", thinking)?;
    }
    if let Some(pid) = update.pid {
        role.pid = optional_pid(pid)?;
    }
    if let Some(host) = update.host {
        role.host = optional_host(host)?;
    }
    if let Some(cadence) = update.cadence {
        role.cadence = optional_cadence(cadence)?;
    }
    for label in update.add_labels {
        let label = sanitize_label(&label)?;
        if !role.labels.iter().any(|known| known == &label) {
            role.labels.push(label);
        }
    }
    for label in update.clear_labels {
        let label = sanitize_label(&label)?;
        role.labels.retain(|known| known != &label);
    }
    Ok(())
}

fn optional_field(field: &str, value: Option<String>) -> Result<Option<String>, VivariumError> {
    match value {
        Some(value) if !value.trim().is_empty() => {
            Ok(Some(sanitize_freeform_field(field, &value)?))
        }
        _ => Ok(None),
    }
}

fn optional_pid(value: Option<u32>) -> Result<Option<u32>, VivariumError> {
    match value {
        Some(pid) if pid > 0 => Ok(Some(pid)),
        Some(pid) => Err(VivariumError::Message(format!(
            "role pid must be a positive integer (got {pid})"
        ))),
        None => Ok(None),
    }
}

fn optional_host(value: Option<String>) -> Result<Option<String>, VivariumError> {
    match value {
        Some(value) if !value.trim().is_empty() => {
            Ok(Some(sanitize_freeform_field("host", &value)?))
        }
        _ => Ok(None),
    }
}

fn optional_cadence(value: Option<String>) -> Result<Option<String>, VivariumError> {
    match value {
        Some(value) if !value.trim().is_empty() => {
            let trimmed = value.trim();
            parse_duration_secs(trimmed)?;
            Ok(Some(trimmed.to_string()))
        }
        _ => Ok(None),
    }
}

fn rename_charter_file(dir: &std::path::Path, old: &str, new: &str) -> Result<(), VivariumError> {
    let old_path = dir.join("charters").join(format!("{old}.md"));
    if !old_path.exists() {
        return Ok(());
    }
    let new_dir = dir.join("charters");
    crate::store::secure_create_dir_all(&new_dir)?;
    let new_path = new_dir.join(format!("{new}.md"));
    fs::rename(&old_path, &new_path).map_err(|e| {
        VivariumError::Message(format!(
            "failed to rename charter {} -> {}: {e}",
            old_path.display(),
            new_path.display()
        ))
    })
}

pub(super) fn sanitize_identity(value: &str) -> Result<String, VivariumError> {
    let value = value.trim().to_ascii_lowercase();
    let valid = !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'));
    if valid {
        Ok(value)
    } else {
        Err(VivariumError::Message(format!(
            "invalid local role '{value}'; use letters, numbers, dot, dash, or underscore"
        )))
    }
}

fn sanitize_label(value: &str) -> Result<String, VivariumError> {
    sanitize_identity(value).map_err(|_| {
        VivariumError::Message(format!(
            "invalid role label '{value}'; use letters, numbers, dot, dash, or underscore"
        ))
    })
}

fn sanitize_freeform_field(field: &str, value: &str) -> Result<String, VivariumError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(VivariumError::Message(format!(
            "role {field} cannot be empty"
        )));
    }
    if value
        .chars()
        .any(|ch| ch == '\n' || ch == '\r' || ch == '\0')
    {
        return Err(VivariumError::Message(format!(
            "role {field} cannot contain control characters"
        )));
    }
    Ok(value.to_string())
}
