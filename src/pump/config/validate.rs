//! Pump config checks: what is invalid, and what is only missing.
//!
//! A finding is one of two kinds. An *error* is a value that is wrong: a bad
//! number, a path that cannot be read, a name that points at nothing. A
//! *missing* item is something the pump needs and the config does not yet
//! say: no committer, no rung command, no pool for the dispatch role. A
//! project in the second state is "not ready", not broken: filling the items
//! in makes it ready with no other step.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::{MergeConfig, PumpConfig, PumpTarget, RUNG_NAMES, RungsConfig, known};
use crate::VivariumError;

/// One thing the pump needs and the config does not yet say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing {
    /// A short stable name, e.g. `pump.committer_name` or `hand pool`.
    pub name: String,
    /// What is missing and how to supply it.
    pub detail: String,
}

/// Everything a check found, split by kind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Findings {
    /// Wrong values; the pump must not start until they are fixed.
    pub errors: Vec<String>,
    /// Absent items; the pump holds (not ready) until they are supplied.
    pub missing: Vec<Missing>,
}

impl Findings {
    fn invalid(&mut self, message: impl Into<String>) {
        self.errors.push(message.into());
    }

    fn absent(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.missing.push(Missing {
            name: name.into(),
            detail: detail.into(),
        });
    }

    /// Whether nothing is wrong and nothing is missing.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.errors.is_empty() && self.missing.is_empty()
    }
}

impl PumpConfig {
    /// Check the config against itself and the known pool names, strictly:
    /// both wrong and missing items fail.
    ///
    /// `pools` holds the names of the mailspace's `[pools.<name>]` tables.
    ///
    /// `project` is the base a relative `hand_rules` path resolves against.
    /// Empty `[pump.dispatch] lanes` is reported: pass [`Self::validate_for`]
    /// when a role assigns a pool so capacity can come from that pool. The
    /// pump itself uses [`Self::findings_for`], which keeps the two kinds
    /// apart.
    ///
    /// # Errors
    /// [`VivariumError::Config`] naming every violated key when a rule fails.
    pub fn validate(&self, project: &Path, pools: &[String]) -> Result<(), VivariumError> {
        self.validate_for(project, pools, false)
    }

    /// Like [`Self::validate`], but empty `[pump.dispatch] lanes` is allowed
    /// when the hand role has a pool; capacity comes from its pool's slots and
    /// optional caps, while kind-pool routing and `default_kind` are optional.
    ///
    /// # Errors
    /// [`VivariumError::Config`] naming every violated key when a rule fails.
    pub fn validate_for(
        &self,
        project: &Path,
        pools: &[String],
        hand_role_pool: bool,
    ) -> Result<(), VivariumError> {
        let found = self.findings_for(project, pools, hand_role_pool);
        let mut all = found.errors;
        all.extend(found.missing.into_iter().map(|item| item.detail));
        if all.is_empty() {
            Ok(())
        } else {
            Err(VivariumError::Config(all.join("; ")))
        }
    }

    /// Check the config and report wrong values apart from missing items.
    /// `hand_role_pool` is whether the hand role has a pool, which lets
    /// `[pump.dispatch] lanes` stay empty.
    #[must_use]
    pub fn findings_for(&self, project: &Path, pools: &[String], hand_role_pool: bool) -> Findings {
        let mut found = Findings::default();
        if self.committer_name.trim().is_empty() {
            found.absent(
                "pump.committer_name",
                "[pump] committer_name must not be empty",
            );
        }
        if self.committer_email.trim().is_empty() {
            found.absent(
                "pump.committer_email",
                "[pump] committer_email must not be empty",
            );
        }
        if let Some(path) = self.hand_rules_path(project)
            && let Err(err) = std::fs::read_to_string(&path)
        {
            found.invalid(format!(
                "[pump] hand_rules `{}` cannot be read: {err}",
                path.display()
            ));
        }
        if self.tick == 0 {
            found.invalid("[pump] tick must be >= 1");
        }
        self.validate_board(&mut found);
        self.validate_plan(&mut found);
        self.validate_workspace(&mut found);
        self.validate_dispatch(pools, hand_role_pool, &mut found);
        validate_rungs("[pump.rungs]", &self.rungs, pools, &mut found);
        if self.targets.is_empty() {
            validate_rung_pools("[pump.rungs]", &self.effective_rungs(None), &mut found);
        }
        for (id, target) in &self.targets {
            let scope = format!("[pump.targets.{id}.rungs]");
            validate_rungs(&scope, &target.rungs, pools, &mut found);
            validate_rung_pools(&scope, &self.effective_rungs(Some(id)), &mut found);
        }
        if self.targets.is_empty() {
            validate_merge(&self.merge, &mut found);
        } else {
            let mut target_paths = BTreeSet::new();
            for (id, target) in self.target_configs() {
                let normalized: PathBuf = Path::new(&target.path).components().collect();
                if !target_paths.insert(normalized) {
                    found.invalid(format!(
                        "[pump.targets.{id}] path duplicates another configured repository target"
                    ));
                }
                validate_target(&id, &target, &mut found);
            }
        }
        found
    }

    /// Every `[[pump.plan.proof_rules]]` entry names a scope and a proof
    /// pattern that compiles, so a typo fails here and not at the first plan.
    fn validate_plan(&self, found: &mut Findings) {
        for (index, rule) in self.plan.proof_rules.iter().enumerate() {
            let at = index + 1;
            if rule.scope.trim().is_empty() {
                found.invalid(format!(
                    "[[pump.plan.proof_rules]] entry {at}: scope must not be empty"
                ));
            }
            if rule.proof.trim().is_empty() {
                found.invalid(format!(
                    "[[pump.plan.proof_rules]] entry {at}: proof must not be empty"
                ));
            } else if let Err(error) = rule.pattern() {
                found.invalid(format!(
                    "[[pump.plan.proof_rules]] entry {at}: proof `{}` is not a valid regex: {error}",
                    rule.proof
                ));
            }
        }
    }

    /// The retired `[pump.board]` intake keys are errors, not silently
    /// ignored: a config that still sets them expects an intake that is gone.
    fn validate_board(&self, found: &mut Findings) {
        if self.board.retired_mark.is_some() {
            found.invalid(
                "[pump.board] mark is retired: a need is planned when it has an open pump \
                 record; send one with `vivi pump send --need <handle>` and remove `mark`",
            );
        }
        if self.board.retired_since.is_some() {
            found.invalid(
                "[pump.board] since is retired: intake is explicit, so no date cutoff \
                 applies; send a pump record with `vivi pump send --need <handle>` and \
                 remove `since`",
            );
        }
    }

    fn validate_dispatch(&self, pools: &[String], hand_role_pool: bool, found: &mut Findings) {
        if self.dispatch.lanes.is_empty() && !hand_role_pool {
            found.absent(
                "hand pool",
                "[pump.dispatch] lanes must not be empty (or give the hand role a pool: \
                 `vivi role pool hand <pool>`)",
            );
        }
        for lane in &self.dispatch.lanes {
            if lane.cap == 0 {
                found.invalid(format!(
                    "[pump.dispatch] lanes: lane `{}` needs cap >= 1",
                    lane.model
                ));
            }
            if lane.model.is_empty() {
                found.invalid("[pump.dispatch] lanes: lane model must not be empty");
            }
        }
        if self.dispatch.max_attempts == 0 {
            found.invalid("[pump.dispatch] max_attempts must be >= 1");
        }
        if self.dispatch.budget == 0 {
            found.invalid("[pump.dispatch] budget must be >= 1");
        }
        if self.dispatch.per_tick == 0 {
            found.invalid("[pump.dispatch] per_tick must be >= 1");
        }
        if self.dispatch.parallel == 0 {
            found.invalid("[pump.dispatch] parallel must be >= 1");
        }
        self.validate_kinds(pools, hand_role_pool, found);
    }

    fn validate_kinds(&self, pools: &[String], hand_role_pool: bool, found: &mut Findings) {
        for (name, kind) in &self.dispatch.kinds {
            if !hand_role_pool {
                if kind.pools.is_empty() {
                    found.absent(
                        format!("pump.dispatch.kinds.{name}.pools"),
                        format!("[pump.dispatch.kinds.{name}] pools must not be empty"),
                    );
                }
                for pool in &kind.pools {
                    if !known(pools, pool) {
                        found.invalid(format!(
                            "[pump.dispatch.kinds.{name}] pools names unknown pool `{pool}`"
                        ));
                    }
                }
            }
            if kind.max == Some(0) {
                found.invalid(format!("[pump.dispatch.kinds.{name}] max must be >= 1"));
            }
        }
        if !hand_role_pool && !self.dispatch.kinds.is_empty() {
            let default = &self.dispatch.default_kind;
            if default.is_empty() {
                found.absent(
                    "pump.dispatch.default_kind",
                    "[pump.dispatch] default_kind must be set when kinds is non-empty",
                );
            } else if !self.dispatch.kinds.contains_key(default) {
                found.invalid(format!(
                    "[pump.dispatch] default_kind names undefined kind `{default}`"
                ));
            }
        }
    }

    /// `[pump.workspace] lane_gib`, when set, must be a plausible lane disk.
    fn validate_workspace(&self, found: &mut Findings) {
        if let Some(gib) = self.workspace.lane_gib
            && !(1..=1024).contains(&gib)
        {
            found.invalid(format!(
                "[pump.workspace] lane_gib must be between 1 and 1024, got {gib}"
            ));
        }
    }
}

/// The legacy `[pump.merge]` settings that are wrong when present.
fn validate_merge(merge: &MergeConfig, found: &mut Findings) {
    if merge.batch == 0 {
        found.invalid("[pump.merge] batch must be >= 1");
    }
    if merge.lanes == 0 {
        found.invalid("[pump.merge] lanes must be >= 1");
    }
    if merge.need_parallel == 0 {
        found.invalid("[pump.merge] need_parallel must be >= 1");
    }
    if merge
        .repair_budget
        .is_some_and(|budget| budget == 0 || budget < merge.proof_timeout)
    {
        found.invalid("[pump.merge] repair_budget must be >= proof_timeout and >= 1");
    }
    for entry in &merge.generated {
        if entry.paths.is_empty() {
            found.invalid("[[pump.merge.generated]] paths must not be empty");
        }
        if entry.regenerate.trim().is_empty() {
            found.invalid("[[pump.merge.generated]] regenerate must not be empty");
        }
    }
    validate_checks_for("[pump.merge.checks_for]", &merge.checks_for, found);
}

fn validate_target(id: &str, target: &PumpTarget, found: &mut Findings) {
    let prefix = format!("[pump.targets.{id}]");
    if !valid_target_id(id) {
        found.invalid(format!(
            "{prefix} id must use lowercase letters, digits and hyphens"
        ));
    }
    let path = Path::new(&target.path);
    if target.path.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| part == std::path::Component::ParentDir)
    {
        found.invalid(format!(
            "{prefix} path must be a workspace-relative path without `..`"
        ));
    }
    if target.github_repo.as_deref().is_some_and(str::is_empty) {
        found.invalid(format!("{prefix} github_repo must not be empty when set"));
    }
    let merge = target.merge.as_ref();
    validate_target_merge(id, merge, found);
}

fn validate_target_merge(id: &str, merge: Option<&MergeConfig>, found: &mut Findings) {
    let prefix = format!("[pump.targets.{id}]");
    if merge.is_some_and(|merge| merge.batch == 0) {
        found.invalid(format!("{prefix}.merge batch must be >= 1"));
    }
    if merge.is_some_and(|merge| merge.lanes == 0) {
        found.invalid(format!("{prefix}.merge lanes must be >= 1"));
    }
    if merge.is_some_and(|merge| merge.need_parallel == 0) {
        found.invalid(format!("{prefix}.merge need_parallel must be >= 1"));
    }
    if merge.is_some_and(|merge| {
        merge
            .repair_budget
            .is_some_and(|budget| budget == 0 || budget < merge.proof_timeout)
    }) {
        found.invalid(format!(
            "{prefix}.merge repair_budget must be >= proof_timeout and >= 1"
        ));
    }
    for entry in merge.map(|merge| merge.generated.as_slice()).unwrap_or(&[]) {
        if entry.paths.is_empty() {
            found.invalid(format!(
                "[pump.targets.{id}.merge] generated paths must not be empty"
            ));
        }
        if entry.regenerate.trim().is_empty() {
            found.invalid(format!(
                "[pump.targets.{id}.merge] generated regenerate must not be empty"
            ));
        }
    }
    let scope = format!("[pump.targets.{id}.merge.checks_for]");
    let empty = BTreeMap::new();
    validate_checks_for(
        &scope,
        merge.map(|merge| &merge.checks_for).unwrap_or(&empty),
        found,
    );
}

fn validate_rungs(scope: &str, rungs: &RungsConfig, pools: &[String], found: &mut Findings) {
    if rungs.timeout == Some(0) {
        found.invalid(format!("{scope} timeout must be >= 1"));
    }
    for name in RUNG_NAMES {
        if rungs.command(name).is_some_and(|cmd| cmd.trim().is_empty()) {
            found.invalid(format!("{scope} {name} must not be empty when set"));
        }
    }
    for name in &rungs.pools {
        if !known(pools, name) {
            found.invalid(format!("{scope} pools names unknown pool `{name}`"));
        }
    }
}

/// A set rung command with no effective pool never dispatches: the rung job's
/// role is unrouted, so it would stay pending forever.
fn validate_rung_pools(scope: &str, effective: &RungsConfig, found: &mut Findings) {
    let set = RUNG_NAMES
        .iter()
        .any(|name| effective.command(name).is_some());
    if set && effective.pools.is_empty() {
        found.invalid(format!(
            "{scope} sets a rung command but its effective pools are empty; rung jobs cannot dispatch"
        ));
    }
}

fn validate_checks_for(scope: &str, table: &BTreeMap<String, Vec<String>>, found: &mut Findings) {
    for (glob, cmds) in table {
        if cmds.is_empty() || cmds.iter().any(|cmd| cmd.trim().is_empty()) {
            found.invalid(format!("{scope} '{glob}' must have non-empty commands"));
        }
    }
}

fn valid_target_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}
