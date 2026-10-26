//! Classifying scope requests: which configured target a request writes in,
//! and whether the paths it asks for may be added there without an operator.
//!
//! Both collectors share this service. A planner `operator` or admitter
//! `escalate` reply names its target in `scope_target`
//! ([`RequestTarget::Named`]); a hand's `blocked-scope` answer carries no
//! target of its own — its target is read from the slice task's front
//! matter through `fanout::target_of`, and the hand request variant holds no
//! target a model could replace ([`RequestTarget::SliceTask`]).
//!
//! Target resolution: the target must be one the pump configures and must
//! agree with the need's explicit `NeedFront.target` and with the need's
//! `need_branches` row target. A disagreement, and any target the pump does
//! not configure, needs an operator. When neither the need nor a branch has
//! selected a repository yet, an explicit known target stands for this
//! request alone: the ruling records it, the shared reader applies it to
//! that target only, and no cross-target branch is created from it.
//!
//! Path classification: paths the request already holds — its original
//! delegation, or an accepted grant — are preserved untouched. Today's deny
//! bounds automatic additions; it never revokes standing permission. The
//! rest of the requested set is then classified whole, against the target's
//! live policy and that target's own resolved integration checkout — never
//! the controller's current checkout. A request is granted whole or not at
//! all: one forbidden member refuses the complete set, and no subset is
//! ever authorized.

use std::path::Path;

use crate::error::VivariumError;
use crate::pump::config::scope::authorize;
use crate::pump::config::{PumpConfig, ScopeDecision, ScopePath};
use crate::pump::flow::fanout::target_of;
use crate::pump::flow::slice_dispatch::SliceMail;

use super::{RulingDecision, ScopeIssue, ScopeRuling, accepted_grants, effective_target_scope};
use super::{original_scope, push_unique};

/// Which target a scope request writes in, and where that target may come
/// from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestTarget {
    /// A planner or admitter request: the `scope_target` its envelope named.
    Named(String),
    /// A hand request: the target of the slice task's own front matter,
    /// read through `fanout::target_of`. The variant carries no target, so
    /// model replacement metadata cannot name one.
    SliceTask,
}

/// One scope request: who asks, for which need, and what it wants to add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeRequest {
    /// The need the request belongs to.
    pub need: String,
    /// The requesting slice's task handle.
    pub task: String,
    /// The run that asked.
    pub run: u32,
    /// Where the request's target comes from.
    pub target: RequestTarget,
    /// The raw paths the request asks for.
    pub paths: Vec<String>,
}

/// The target context a request is classified in: the pump configuration
/// (which targets exist, what each allows and denies, where each target's
/// integration checkout lives) and the need's branch state.
pub struct ScopeContext<'a> {
    /// The pump configuration. Targets come from `target_configs`, and a
    /// target's checkout from `target_repo_dir` — never the controller's
    /// current checkout.
    pub config: &'a PumpConfig,
    /// The project root `target_repo_dir` resolves against.
    pub project: &'a Path,
    /// The target of the need's branch row, when the need has one.
    pub branch_target: Option<&'a str>,
    /// The reply role whose scope-ruling notes count as grants.
    pub reply_from: &'a str,
}

/// One classified scope request: the resolved target, the normalized
/// requested set, and the typed decision a ruling or journal entry records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeClassification {
    /// The resolved configured target; empty when no target resolved and an
    /// operator must name one.
    pub target: String,
    /// The complete requested set, normalized and deduplicated.
    pub paths: Vec<String>,
    /// What the classification decided about: the requested set minus the
    /// paths the need already holds. A grant adds exactly these; a refused
    /// target leaves the whole request to the operator. A grant that adds
    /// nothing has no ruling to record.
    pub added: Vec<String>,
    /// The typed decision.
    pub decision: RulingDecision,
    /// The strongest typed issue when an operator is required; a grant
    /// carries none.
    pub issue: Option<ScopeIssue>,
    /// Why, in one line of plain prose.
    pub reason: String,
}

impl ScopeClassification {
    /// Whether the request may proceed: only a grant does, and it adds
    /// [`ScopeClassification::added`] to the need's effective scope.
    #[must_use]
    pub fn is_grant(&self) -> bool {
        self.decision.is_grant()
    }

    /// The scope-ruling record this classification settles into for the
    /// request it classified. A grant names the paths it adds; an
    /// operator-required decision names the whole requested set. A grant
    /// that adds nothing yields a record with no paths, which
    /// [`super::encode`] refuses: there is nothing to record.
    #[must_use]
    pub fn ruling(&self, need: &str, run: u32, task: &str) -> ScopeRuling {
        let paths = if self.is_grant() {
            &self.added
        } else {
            &self.paths
        };
        ScopeRuling {
            need: need.to_string(),
            target: self.target.clone(),
            run,
            task: task.to_string(),
            paths: paths.clone(),
            reason: self.reason.clone(),
            decision: self.decision,
            issue: self.issue,
        }
    }
}

/// Classify one scope request: resolve its target, keep what the need
/// already holds, and classify the rest of the requested set atomically
/// against the target's live policy and its own checkout.
///
/// # Errors
/// Returns [`VivariumError`] when the need letter or the slice task cannot
/// be read. An unknown or inconsistent target is not an error: it is an
/// operator-required classification.
pub fn classify(
    mail: &dyn SliceMail,
    context: &ScopeContext<'_>,
    request: &ScopeRequest,
) -> Result<ScopeClassification, VivariumError> {
    let paths = normalize(&request.paths);
    let named = request_target(mail, request)?;
    if let Some((issue, reason)) = target_problem(mail, context, request, named.as_deref())? {
        return Ok(ScopeClassification {
            target: named.unwrap_or_default(),
            added: paths.clone(),
            paths,
            decision: RulingDecision::OperatorRequired,
            issue: Some(issue),
            reason,
        });
    }
    let target = named.unwrap_or_default();
    let held = effective_target_scope(mail, &request.need, &target, context.reply_from)?;
    let fresh: Vec<String> = paths
        .iter()
        .filter(|path| !holds(&held, path))
        .cloned()
        .collect();
    let (decision, issue, reason, added) = if fresh.is_empty() {
        (
            RulingDecision::Grant,
            None,
            format!(
                "every requested path is already delegated or granted on {}",
                request.need
            ),
            Vec::new(),
        )
    } else {
        let verdict = authorize(
            &context.config.target_configs(),
            &target,
            context
                .config
                .target_repo_dir(context.project, &target)
                .as_deref(),
            &fresh,
        );
        let (decision, issue, reason) = policy_verdict(&target, verdict);
        (decision, issue, reason, fresh)
    };
    Ok(ScopeClassification {
        target,
        added,
        paths,
        decision,
        issue,
        reason,
    })
}

/// The target a request writes in, after trimming: the `scope_target` a
/// role request named, or the slice task's own front-matter target for a
/// hand request.
///
/// # Errors
/// Returns [`VivariumError`] when the slice task cannot be read.
fn request_target(
    mail: &dyn SliceMail,
    request: &ScopeRequest,
) -> Result<Option<String>, VivariumError> {
    let named = match &request.target {
        RequestTarget::Named(name) => Some(name.clone()),
        RequestTarget::SliceTask => {
            let body = mail.task_body(&request.task)?;
            target_of(&body)
        }
    };
    Ok(named.map(|name| name.trim().to_string()))
}

/// The typed target issue of a request, if it has one: no target at all, a
/// target the pump does not configure, or a target that disagrees with the
/// need's explicit target or its branch row target.
fn target_problem(
    mail: &dyn SliceMail,
    context: &ScopeContext<'_>,
    request: &ScopeRequest,
    named: Option<&str>,
) -> Result<Option<(ScopeIssue, String)>, VivariumError> {
    let Some(target) = named.filter(|target| !target.is_empty()) else {
        let source = match request.target {
            RequestTarget::Named(_) => "the request names no target",
            RequestTarget::SliceTask => "the slice task names no target",
        };
        return Ok(Some((ScopeIssue::UnknownTarget, String::from(source))));
    };
    if !context.config.target_configs().contains_key(target) {
        return Ok(Some((
            ScopeIssue::UnknownTarget,
            format!("target `{target}` is not a configured pump target"),
        )));
    }
    let (_, need_target) = original_scope(mail, &request.need)?;
    if let Some(need_target) = need_target
        && need_target != target
    {
        return Ok(Some((
            ScopeIssue::TargetMismatch,
            format!("request target `{target}` disagrees with the need's target `{need_target}`"),
        )));
    }
    if let Some(branch) = context.branch_target
        && branch != target
    {
        return Ok(Some((
            ScopeIssue::TargetMismatch,
            format!("request target `{target}` disagrees with the need's branch target `{branch}`"),
        )));
    }
    Ok(None)
}

/// The typed ruling a policy decision settles into: an allowed request
/// grants; every other verdict needs an operator and names its issue.
fn policy_verdict(
    target: &str,
    verdict: ScopeDecision,
) -> (RulingDecision, Option<ScopeIssue>, String) {
    match verdict {
        ScopeDecision::Allowed => (
            RulingDecision::Grant,
            None,
            format!("the requested paths sit inside target `{target}` scope policy"),
        ),
        ScopeDecision::Denied => (
            RulingDecision::OperatorRequired,
            Some(ScopeIssue::Denied),
            format!("a requested path touches denied scope of target `{target}`"),
        ),
        ScopeDecision::OutsideAllow => (
            RulingDecision::OperatorRequired,
            Some(ScopeIssue::OutsideAllow),
            format!("a requested path sits outside target `{target}` allow list"),
        ),
        ScopeDecision::InvalidPath => (
            RulingDecision::OperatorRequired,
            Some(ScopeIssue::InvalidPath),
            String::from("a requested path is not a valid literal path"),
        ),
        ScopeDecision::UnverifiablePath => (
            RulingDecision::OperatorRequired,
            Some(ScopeIssue::UnverifiablePath),
            format!("the checkout of target `{target}` cannot verify what a requested path names"),
        ),
    }
}

/// Whether the held effective scope already covers `path`: a held directory
/// entry covers the paths beneath it, a held file entry only itself. A path
/// that does not parse is covered by nothing and is left for the policy to
/// refuse.
fn holds(held: &[String], path: &str) -> bool {
    let Ok(requested) = ScopePath::parse(path) else {
        return false;
    };
    held.iter()
        .any(|entry| ScopePath::parse(entry).is_ok_and(|entry| entry.covers(&requested)))
}

/// The normalized requested set: each path through `ScopePath::parse`, a
/// path that does not parse kept as written so the refusal can name it,
/// deduplicated, first seen first.
fn normalize(paths: &[String]) -> Vec<String> {
    let mut normalized = Vec::new();
    for path in paths {
        let text = ScopePath::parse(path).map_or_else(
            |_| path.trim().to_string(),
            |parsed| parsed.as_str().to_string(),
        );
        push_unique(&mut normalized, &text);
    }
    normalized
}

/// One target's effective scope for a runtime brief: the reader's facts
/// only — the original delegation paths that reach the target, plus the
/// accepted grants recorded for it, delegation first. An unrestricted
/// policy default is not an existing grant and never appears here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetScope {
    /// The configured target the paths live in.
    pub target: String,
    /// The need's original delegation paths that reach this target; empty
    /// when the need names another target.
    pub original: Vec<String>,
    /// The accepted grant paths recorded for this target, oldest first.
    pub granted: Vec<String>,
}

impl TargetScope {
    /// Every path of the group, delegation first then grants, deduplicated.
    #[must_use]
    pub fn paths(&self) -> Vec<String> {
        let mut paths = Vec::new();
        for path in self.original.iter().chain(self.granted.iter()) {
            push_unique(&mut paths, path);
        }
        paths
    }

    /// Whether the target holds no scope at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.original.is_empty() && self.granted.is_empty()
    }
}

/// The effective scope of `need`, grouped per target for runtime briefs.
/// `targets` are the configured target ids to render. This is the shared
/// reader, not the policy: a target with an unrestricted default still
/// shows only the delegation and grants it actually holds, and a target
/// that holds nothing is left out — its absence means no permission there,
/// never an error.
///
/// # Errors
/// Returns [`VivariumError`] when the need letter cannot be read.
pub fn effective_scopes(
    mail: &dyn SliceMail,
    need: &str,
    reply_from: &str,
    targets: &[String],
) -> Result<Vec<TargetScope>, VivariumError> {
    let (original, need_target) = original_scope(mail, need)?;
    let mut groups = Vec::new();
    for target in targets {
        let reaches = !original.is_empty()
            && need_target
                .as_deref()
                .is_none_or(|named| named == target.as_str());
        let (original, granted) = if reaches {
            let mut granted = Vec::new();
            for ruling in accepted_grants(mail, need, Some(target), reply_from)? {
                for path in &ruling.paths {
                    push_unique(&mut granted, path);
                }
            }
            (original.clone(), granted)
        } else {
            (Vec::new(), Vec::new())
        };
        let group = TargetScope {
            target: target.clone(),
            original,
            granted,
        };
        if !group.is_empty() {
            groups.push(group);
        }
    }
    Ok(groups)
}

#[cfg(test)]
#[path = "request_test.rs"]
mod tests;
