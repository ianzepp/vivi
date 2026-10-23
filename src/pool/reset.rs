//! One entry point for resetting a slot onto a freshly fetched origin branch.
//!
//! Default base is the configured `[pump] main` (else `main`). The caller
//! may pass another origin branch (for example a prior work branch). Fetch
//! always runs first; a missing origin ref fails the reset and leaves the
//! slot free and clean — there is no silent fallback.

use std::path::Path;

use super::clean::{ActionKind, CleanAction, CleanSkip};
use super::clean_rule::write_mark;
use super::clean_slot::slot_is_free;
use super::config::PoolConfig;
use super::local_git::Repo;
use super::local_slots::{CHECKOUT, WORKTREE_OF_FILE};
use super::provider::{ExecOutput, PoolProvider};
use super::slot::META_BASE;
use crate::error::VivariumError;
use crate::mailspace::Mailspace;
use crate::pump::state::Unit;

/// `marks/base_ref`: origin branch the slot was last reset onto.
pub const MARK_BASE_REF: &str = "base_ref";
/// `marks/base_sha`: resolved commit of that reset.
pub const MARK_BASE_SHA: &str = "base_sha";

/// When the base is chosen: recycle always defaults to main.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetKind {
    Recycle,
    Dispatch,
}

/// The origin branch a reset will fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChosenBase {
    pub name: String,
    pub why: &'static str,
}

/// What a successful reset pointed the slot at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetOutcome {
    pub base_ref: String,
    pub sha: String,
    pub why: &'static str,
}

/// `[pump] main`, or `main` when pump is unset.
#[must_use]
pub fn default_main(space: &Mailspace) -> String {
    space
        .config
        .pump
        .as_ref()
        .map(|p| p.main.as_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("main")
        .to_string()
}

/// Pick the origin branch for this reset. `requested` wins when set.
#[must_use]
pub fn choose_base(
    space: &Mailspace,
    requested: Option<&str>,
    unit: Option<&Unit>,
    kind: ResetKind,
) -> ChosenBase {
    if let Some(name) = requested.map(strip_origin).filter(|n| !n.is_empty()) {
        return ChosenBase {
            name,
            why: "requested",
        };
    }
    if kind == ResetKind::Dispatch
        && let Some(unit) = unit
        && let Some(chosen) = dispatch_override(unit)
    {
        return chosen;
    }
    ChosenBase {
        name: default_main(space),
        why: "default-main",
    }
}

fn dispatch_override(unit: &Unit) -> Option<ChosenBase> {
    if let Some(name) = unit
        .base
        .as_deref()
        .map(strip_origin)
        .filter(|n| !n.is_empty())
    {
        return Some(ChosenBase {
            name,
            why: "unit.base",
        });
    }
    if let Some(name) = prior_attempt_branch(unit) {
        return Some(ChosenBase {
            name,
            why: "prior-attempt",
        });
    }
    None
}

fn prior_attempt_branch(unit: &Unit) -> Option<String> {
    unit.history
        .iter()
        .rev()
        .find_map(|a| a.branch.clone())
        .filter(|b| !b.is_empty())
}

fn strip_origin(name: &str) -> String {
    name.strip_prefix("refs/remotes/origin/")
        .or_else(|| name.strip_prefix("refs/heads/"))
        .or_else(|| name.strip_prefix("origin/"))
        .unwrap_or(name)
        .to_string()
}

/// Fetch `origin/<base>` and return its tip. Missing origin or branch is an
/// error; nothing is checked out.
///
/// # Errors
/// Returns an error when origin is absent or the branch is not on origin.
pub fn fetch_origin_base(repo: &Repo, base: &str) -> Result<String, VivariumError> {
    if repo.remote_url("origin").is_none() {
        return Err(VivariumError::Message("no origin remote".into()));
    }
    repo.fetch_remote_branch("origin", base)
        .map_err(|e| VivariumError::Message(format!("origin/{base} is missing: {e}")))?;
    repo.commit_of(&format!("refs/remotes/origin/{base}"))
        .or_else(|| repo.commit_of("FETCH_HEAD"))
        .ok_or_else(|| VivariumError::Message(format!("origin/{base} is not a commit")))
}

/// Fetch `origin/<base>` and point the slot checkout at that commit.
///
/// # Errors
/// Returns an error when the fetch or checkout fails. The slot is left as it
/// was when the fetch fails.
pub fn reset_local(
    dir: &Path,
    base: &str,
    why: &'static str,
    dry_run: bool,
) -> Result<ResetOutcome, VivariumError> {
    let checkout = dir.join(CHECKOUT);
    let repo = origin_repo(dir, &checkout)
        .ok_or_else(|| VivariumError::Message("no git checkout to reset".into()))?;
    let sha = fetch_origin_base(&repo, base)?;
    if dry_run {
        return Ok(ResetOutcome {
            base_ref: base.to_string(),
            sha,
            why,
        });
    }
    place_checkout(&repo, &checkout, &sha)?;
    record_base_marks(dir, base, &sha)?;
    Ok(ResetOutcome {
        base_ref: base.to_string(),
        sha,
        why,
    })
}

/// Fetch then add a detached worktree at `dir/repo` (after a recycle remove).
///
/// # Errors
/// Returns an error when Git refuses the add.
pub fn add_at_origin(
    dir: &Path,
    repo: &Repo,
    base: &str,
    sha: &str,
) -> Result<ResetOutcome, VivariumError> {
    repo.add_worktree(&dir.join(CHECKOUT), sha)?;
    record_base_marks(dir, base, sha)?;
    Ok(ResetOutcome {
        base_ref: base.to_string(),
        sha: sha.to_string(),
        why: "recycle",
    })
}

fn place_checkout(repo: &Repo, checkout: &Path, sha: &str) -> Result<(), VivariumError> {
    if checkout.join(".git").exists() {
        return Repo::new(checkout).checkout_detach(sha);
    }
    repo.add_worktree(checkout, sha)
}

/// Write `base_ref`, `base_sha`, and `vivi_pool_base`.
///
/// # Errors
/// Returns an error if a mark cannot be written.
pub fn record_base_marks(dir: &Path, base: &str, sha: &str) -> Result<(), VivariumError> {
    write_mark(dir, MARK_BASE_REF, base)?;
    write_mark(dir, MARK_BASE_SHA, sha)?;
    write_mark(dir, META_BASE, sha)
}

fn origin_repo(dir: &Path, checkout: &Path) -> Option<Repo> {
    let file = dir.join(WORKTREE_OF_FILE);
    if let Ok(text) = std::fs::read_to_string(&file) {
        let path = std::path::PathBuf::from(text.trim());
        if path.is_dir() {
            return Some(Repo::new(&path));
        }
    }
    checkout.is_dir().then(|| Repo::new(checkout))
}

/// Reset a claimed or free slot: local fetch+checkout, else remote exec.
///
/// # Errors
/// Returns an error when the origin branch is missing or the checkout fails.
pub fn reset_slot(
    config: &PoolConfig,
    provider: &dyn PoolProvider,
    machine_id: &str,
    base: &str,
    why: &'static str,
) -> Result<ResetOutcome, VivariumError> {
    if config.provider == "local" {
        provider.machine(machine_id)?;
        let root = provider
            .local_root()
            .ok_or_else(|| VivariumError::Message("local provider has no resolved root".into()))?;
        let dir = root.join(machine_id);
        if let Some(source) = config.local.worktree_path() {
            super::local::refuse_foreign_worktree(machine_id, &dir, &source)?;
        }
        if !slot_is_free(&dir) && !dir.join("claim").is_file() {
            return Err(VivariumError::Message(
                "slot is not free or claimed for this reset".into(),
            ));
        }
        return reset_local(&dir, base, why, false);
    }
    reset_remote(provider, machine_id, base, why)
}

fn reset_remote(
    provider: &dyn PoolProvider,
    id: &str,
    base: &str,
    why: &'static str,
) -> Result<ResetOutcome, VivariumError> {
    let started = provider.machine(id)?.state == "started";
    if !started {
        provider.start(id)?;
    }
    let script = remote_script(base);
    let execution = provider.exec(id, &script, 90);
    if started {
        return parse_remote(execution?, base, why);
    }
    let stopped = provider.stop(id);
    finish_remote_reset(execution, stopped, base, why)
}

fn finish_remote_reset(
    execution: Result<ExecOutput, VivariumError>,
    stopped: Result<(), VivariumError>,
    base: &str,
    why: &'static str,
) -> Result<ResetOutcome, VivariumError> {
    let reset = execution.and_then(|out| parse_remote(out, base, why));
    match (reset, stopped) {
        (Ok(out), Ok(())) => Ok(out),
        (Err(reset_error), Ok(())) => Err(reset_error),
        (Ok(_), Err(stop_error)) => Err(stop_error),
        (Err(reset_error), Err(stop_error)) => Err(VivariumError::Message(format!(
            "reset failed ({reset_error}); also failed to stop machine ({stop_error})"
        ))),
    }
}

/// Shell that fetch-checks-out `origin/$BASE` inside a remote slot.
#[must_use]
pub fn remote_script(base: &str) -> String {
    format!(
        r#"BASE={base}
RD="${{WORK:-/work}}/repo"
if [ ! -d "$RD/.git" ] && [ ! -e "$RD/.git" ]; then
  echo "reset: no checkout" >&2; exit 2
fi
git -C "$RD" fetch -q origin "$BASE" || {{ echo "reset: origin/$BASE missing" >&2; exit 3; }}
git -C "$RD" checkout -q --detach FETCH_HEAD || {{ echo "reset: checkout failed" >&2; exit 4; }}
SHA=$(git -C "$RD" rev-parse HEAD)
for M in "${{WORK:-/work}}/marks" "${{WORK:-/work}}/vivi-marks"; do
  mkdir -p "$M"
  printf '%s\n' "$BASE" > "$M/base_ref"
  printf '%s\n' "$SHA" > "$M/base_sha"
  printf '%s\n' "$SHA" > "$M/vivi_pool_base"
done
printf '%s\n' "$SHA"
"#,
        base = shell_single(base)
    )
}

fn shell_single(value: &str) -> String {
    format!("'{}'", value.replace('\'', r#"'\''"#))
}

fn parse_remote(
    out: ExecOutput,
    base: &str,
    why: &'static str,
) -> Result<ResetOutcome, VivariumError> {
    if out.exit_code != 0 {
        let detail = out.stderr.lines().last().unwrap_or("reset failed");
        return Err(VivariumError::Message(format!(
            "WARN reset origin/{base}: {detail}"
        )));
    }
    let sha = out.stdout.lines().last().unwrap_or("").trim().to_string();
    if sha.len() < 7 {
        return Err(VivariumError::Message(format!(
            "WARN reset origin/{base}: empty sha"
        )));
    }
    Ok(ResetOutcome {
        base_ref: base.to_string(),
        sha,
        why,
    })
}

/// Report helpers for clean / reset porcelain.
#[must_use]
pub fn reset_action(pool: &str, slot: &str, out: &ResetOutcome) -> CleanAction {
    CleanAction {
        pool: pool.to_string(),
        slot: slot.to_string(),
        kind: ActionKind::Reset,
        bytes: 0,
        path: format!("origin/{}", out.base_ref),
        note: format!("{} {}", out.sha, out.why),
    }
}

#[must_use]
pub fn reset_skip(pool: &str, slot: &str, reason: &str) -> CleanSkip {
    CleanSkip {
        pool: pool.to_string(),
        slot: slot.to_string(),
        kind: ActionKind::Reset,
        reason: format!("WARN {reason}"),
    }
}

#[cfg(test)]
#[path = "reset_test.rs"]
mod tests;
