//! Dispatch admission: the dispatch catalog and pure candidate resolution.
//!
//! [`DispatchCatalog`] is the complete dispatch view of one
//! [`MailspaceConfig`] snapshot: every `[pools.<name>]` table in full
//! (bands, harness models, caps) and every local role's model facts (band or
//! slug, thinking, read-only). Where [`crate::routing::RoutedPool`] keeps
//! only the slice the router needs, the catalog keeps everything model
//! resolution reads, so a candidate resolved here refuses exactly where
//! `vivi pool dispatch` would refuse it.
//!
//! [`resolve_candidate`] is pure: for one job's overrides and its actual
//! task letter it delegates model precedence to
//! [`crate::pool::role_dispatch::resolve`] — the same resolver the dispatch
//! command runs — and returns the candidate pool's actual model, its
//! model-provider key, and the resolved execution mode. Each candidate pool
//! resolves independently: no later band entry is chosen and no fallback
//! route is added; ordering candidates stays with the caller. Ctx builders
//! and integration tests construct the catalog with
//! [`DispatchCatalog::from_mailspace`].

use std::collections::BTreeMap;

use crate::error::VivariumError;
use crate::mailspace::{LocalIdentity, MailspaceConfig};
use crate::pool::config::PoolConfig;
use crate::pool::role_dispatch::{self, RoleFacts};

/// The complete dispatch view of one mailspace config snapshot.
#[derive(Debug, Clone, Default)]
pub struct DispatchCatalog {
    pools: BTreeMap<String, PoolConfig>,
    roles: BTreeMap<String, RoleFacts>,
}

impl DispatchCatalog {
    /// Build the catalog from one mailspace config snapshot: every pool
    /// table complete, every identity's model facts. Charters are not part
    /// of a config snapshot and stay blank; admission reads only a role's
    /// band or slug, kind, thinking effort and read-only setting.
    #[must_use]
    pub fn from_mailspace(config: &MailspaceConfig) -> Self {
        Self {
            pools: config.pools.clone(),
            roles: config.identities.iter().map(role_facts_of).collect(),
        }
    }

    /// The complete config of pool `name`, when the snapshot has it.
    #[must_use]
    pub fn pool(&self, name: &str) -> Option<&PoolConfig> {
        self.pools.get(name)
    }

    /// The model facts of the role named `name`, matched like the pump's
    /// role routing: the exact name first, then case-insensitive.
    #[must_use]
    pub fn role(&self, name: &str) -> Option<&RoleFacts> {
        if let Some(facts) = self.roles.get(name) {
            return Some(facts);
        }
        self.roles
            .values()
            .find(|facts| facts.name.eq_ignore_ascii_case(name))
    }
}

/// The catalog's model facts of one config identity, keyed by its name.
fn role_facts_of(role: &LocalIdentity) -> (String, RoleFacts) {
    (
        role.name.clone(),
        RoleFacts {
            name: role.name.clone(),
            kind: role.kind.clone(),
            band: role.model.clone(),
            thinking: role.thinking.clone(),
            charter: String::new(),
            read_only: role.read_only,
        },
    )
}

/// What one job contributes to a candidate resolution: its overrides and its
/// actual task letter.
#[derive(Debug, Clone, Copy)]
pub struct CandidateJob<'a> {
    /// Role the job names, when one is set (`PoolJob.role`). A name the
    /// snapshot does not know is no role at all, never another role.
    pub role: Option<&'a str>,
    /// Model override, when the job carries one (`PoolJob.model`). Always a
    /// model slug, never a band.
    pub model: Option<&'a str>,
    /// Whether the job asks to publish nothing (`PoolJob.read_only`).
    pub read_only: bool,
    /// Whether the job runs a shell command instead of a model
    /// (`PoolJob.command.is_some()`).
    pub command: bool,
    /// The actual task letter: front matter plus prose, exactly as sent.
    pub letter: &'a str,
}

/// One candidate pool's resolved execution of a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCandidate {
    /// The pool the resolution ran against.
    pub pool: String,
    /// Model slug the pool harness runs; `None` for a command job.
    pub model: Option<String>,
    /// Model-provider key of the model; `None` for a command job, which has
    /// no provider bucket.
    pub provider: Option<String>,
    /// Reasoning effort, when a band or a role slug chose the model.
    pub reasoning: Option<String>,
    /// Band the job runs at, when one was given.
    pub band: Option<u8>,
    /// Whether the job publishes nothing.
    pub read_only: bool,
}

/// Resolve one job against one candidate pool, purely: no I/O, no CLI, and
/// no effect on any other candidate. Model precedence is
/// [`crate::pool::role_dispatch::resolve`]'s — the job's model override,
/// then the task letter's `band` key, then the role's slug, then the role's
/// band through the pool's own bands table — with the role taken exactly as
/// the dispatch command takes it: the named role when the catalog knows it,
/// else the shipped Auditor when the letter names a built-in auditor job,
/// else no role. A command job resolves to no model, no reasoning and no
/// provider bucket, with the read-only request passed through.
///
/// # Errors
/// Returns an error when the pool is not in the catalog, and whatever
/// [`crate::pool::role_dispatch::resolve`] refuses: a bad band string, no
/// model anywhere, a band the pool does not serve, or a model the pool
/// harness does not offer.
pub fn resolve_candidate(
    catalog: &DispatchCatalog,
    job: &CandidateJob,
    pool: &str,
) -> Result<ResolvedCandidate, VivariumError> {
    let config = catalog.pool(pool).ok_or_else(|| unknown_pool(pool))?;
    if job.command {
        return Ok(ResolvedCandidate {
            pool: pool.to_string(),
            model: None,
            provider: None,
            reasoning: None,
            band: None,
            read_only: job.read_only,
        });
    }
    let role = candidate_role(catalog, job.role, job.letter);
    let resolved = role_dispatch::resolve(
        config,
        pool,
        &role_dispatch::Input {
            model_flag: job.model,
            read_only_flag: job.read_only,
            role: role.as_ref(),
            body: job.letter,
        },
    )?;
    Ok(ResolvedCandidate {
        pool: pool.to_string(),
        provider: Some(model_provider_key(&resolved.model).to_string()),
        model: Some(resolved.model),
        reasoning: resolved.reasoning,
        band: resolved.band,
        read_only: resolved.read_only,
    })
}

/// The role a candidate dispatch runs as, with the dispatch command's
/// precedence: the explicitly named role when the catalog knows it; else the
/// shipped Auditor when the task letter names a built-in auditor job; else
/// no role — a name the catalog does not know is no role at all, never
/// another role instead.
fn candidate_role(
    catalog: &DispatchCatalog,
    named: Option<&str>,
    letter: &str,
) -> Option<RoleFacts> {
    let named = named.map(str::trim).filter(|name| !name.is_empty());
    if let Some(facts) = named.and_then(|name| catalog.role(name)) {
        return Some(facts.clone());
    }
    builtin_auditor_letter(letter).then(RoleFacts::builtin_auditor)
}

/// Whether the task letter's front matter names a built-in auditor job. A
/// letter with no parsable front matter carries no built-in role.
fn builtin_auditor_letter(letter: &str) -> bool {
    match crate::letter::read::task_of("", letter) {
        Ok(envelope) => envelope
            .front
            .job
            .is_some_and(|job| job.builtin_role.as_deref() == Some("auditor")),
        Err(_) => false,
    }
}

/// The model-provider key of a model slug: the namespace before the first
/// `/`. An unqualified slug is its own key, within its pool. The key comes
/// from the model alone: a pool's machine host (`local`, `fly`, …) is never
/// a model-provider key.
#[must_use]
pub fn model_provider_key(model: &str) -> &str {
    model.split('/').next().unwrap_or(model)
}

fn unknown_pool(pool: &str) -> VivariumError {
    VivariumError::Config(format!("pool {pool} is not in the dispatch catalog"))
}

#[cfg(test)]
#[path = "admission_test.rs"]
mod tests;
