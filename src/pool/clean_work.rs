//! Safe work-lane recycle: worktrees, target caches, and scratch branches.

use std::path::{Path, PathBuf};

use super::clean::{ActionKind, CleanAction, CleanSkip};
use super::clean_rule::{MARK_LAST_BRANCH, MARK_LAST_TASK, empty_dir, read_mark, write_mark};
use super::clean_slot::slot_is_free;
use super::local_git::Repo;
use super::local_slots::{CHECKOUT, WORKTREE_OF_FILE};
use crate::error::VivariumError;

/// Why a worktree must not be deleted.
#[must_use]
pub fn worktree_blockers(path: &Path, merge_lane: &str, build_dir: Option<&str>) -> Vec<String> {
    let checkout = Repo::new(path);
    let mut out = Vec::new();
    match checkout.dirty_paths() {
        Ok(paths) => out.extend(source_dirt(&paths, build_dir)),
        Err(e) => out.push(format!("cannot read status: {e}")),
    }
    let Some(head) = checkout.commit_of("HEAD") else {
        return out;
    };
    if checkout.commit_of(merge_lane).is_none() {
        out.push(format!("merge lane {merge_lane} is not a commit"));
        return out;
    }
    match checkout.commits_ahead(&head, merge_lane) {
        Ok(ahead) if !ahead.is_empty() => {
            out.push(format!("{} commit(s) not in {merge_lane}", ahead.len()))
        }
        Err(e) => out.push(format!("cannot compare to {merge_lane}: {e}")),
        Ok(_) => {}
    }
    out
}

fn source_dirt(paths: &[String], build_dir: Option<&str>) -> Vec<String> {
    let dirt: Vec<&String> = paths
        .iter()
        .filter(|p| !is_build_path(p, build_dir))
        .collect();
    if dirt.is_empty() {
        return Vec::new();
    }
    let shown: Vec<&str> = dirt.iter().take(5).map(|p| p.as_str()).collect();
    vec![format!("uncommitted changes: {}", shown.join(", "))]
}

fn is_build_path(path: &str, build_dir: Option<&str>) -> bool {
    let first = path.split('/').next().unwrap_or(path);
    first == "target" || build_dir.is_some_and(|b| first == b)
}

/// Recycle a free slot's worktrees when `merge_lane` contains their work.
///
/// # Errors
/// Returns an error only when a Git invocation itself fails unexpectedly;
/// safety refusals become skips.
pub fn recycle_lane(
    pool: &str,
    slot: &str,
    dir: &Path,
    merge_lane: &str,
    build_dir: Option<&str>,
    reset_base: &str,
    dry_run: bool,
) -> Result<(Vec<CleanAction>, Vec<CleanSkip>), VivariumError> {
    if slot_is_free(dir) {
        return recycle_free_lane(pool, slot, dir, merge_lane, build_dir, reset_base, dry_run);
    }
    Ok((
        Vec::new(),
        vec![skip(
            pool,
            slot,
            ActionKind::Worktree,
            "slot is mid-dispatch",
        )],
    ))
}

fn recycle_free_lane(
    pool: &str,
    slot: &str,
    dir: &Path,
    merge_lane: &str,
    build_dir: Option<&str>,
    reset_base: &str,
    dry_run: bool,
) -> Result<(Vec<CleanAction>, Vec<CleanSkip>), VivariumError> {
    let mut actions = Vec::new();
    let mut skips = Vec::new();
    let origin_sha = match fetch_reset(dir, reset_base) {
        Ok(sha) => sha,
        Err(e) => {
            skips.push(skip(pool, slot, ActionKind::Reset, &format!("WARN {e}")));
            return Ok((actions, skips));
        }
    };
    drop_targets(
        pool,
        slot,
        dir,
        merge_lane,
        build_dir,
        reset_base,
        dry_run,
        &mut actions,
        &mut skips,
    );
    recycle_checkout(
        pool,
        slot,
        dir,
        merge_lane,
        build_dir,
        reset_base,
        &origin_sha,
        dry_run,
        &mut actions,
        &mut skips,
    )?;
    drop_scratch_branch(
        pool,
        slot,
        dir,
        merge_lane,
        dry_run,
        &mut actions,
        &mut skips,
    );
    if !dry_run && recycled(&actions) {
        forget_lane(dir);
    }
    Ok((actions, skips))
}

fn recycle_checkout(
    pool: &str,
    slot: &str,
    dir: &Path,
    merge_lane: &str,
    build_dir: Option<&str>,
    reset_base: &str,
    origin_sha: &str,
    dry_run: bool,
    actions: &mut Vec<CleanAction>,
    skips: &mut Vec<CleanSkip>,
) -> Result<(), VivariumError> {
    let repo = dir.join(CHECKOUT);
    if repo.is_dir() {
        recycle_one(
            pool,
            slot,
            dir,
            &repo,
            merge_lane,
            build_dir,
            ActionKind::Worktree,
            reset_base,
            origin_sha,
            dry_run,
            actions,
            skips,
        )?;
    }
    Ok(())
}

fn recycled(actions: &[CleanAction]) -> bool {
    actions.iter().any(|a| {
        matches!(
            a.kind,
            ActionKind::Worktree | ActionKind::Target | ActionKind::Branch
        )
    })
}

fn forget_lane(dir: &Path) {
    let _ignored = write_mark(dir, MARK_LAST_TASK, "");
    let _ignored = write_mark(dir, MARK_LAST_BRANCH, "");
}

fn drop_targets(
    pool: &str,
    slot: &str,
    dir: &Path,
    merge_lane: &str,
    build_dir: Option<&str>,
    reset_base: &str,
    dry_run: bool,
    actions: &mut Vec<CleanAction>,
    skips: &mut Vec<CleanSkip>,
) {
    let targets = dir.join("targets");
    let Ok(entries) = std::fs::read_dir(&targets) else {
        return;
    };
    for entry in entries.flatten() {
        let cache = entry.path();
        let checkout = cache.join(CHECKOUT);
        if !checkout.is_dir() {
            continue;
        }
        let _ignored = recycle_one(
            pool,
            slot,
            dir,
            &checkout,
            merge_lane,
            build_dir,
            ActionKind::Target,
            reset_base,
            "",
            dry_run,
            actions,
            skips,
        );
        if !dry_run && !checkout.exists() {
            let _ignored = std::fs::remove_dir_all(&cache);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn recycle_one(
    pool: &str,
    slot: &str,
    dir: &Path,
    checkout: &Path,
    merge_lane: &str,
    build_dir: Option<&str>,
    kind: ActionKind,
    reset_base: &str,
    origin_sha: &str,
    dry_run: bool,
    actions: &mut Vec<CleanAction>,
    skips: &mut Vec<CleanSkip>,
) -> Result<(), VivariumError> {
    let blockers = worktree_blockers(checkout, merge_lane, build_dir);
    if !blockers.is_empty() {
        skips.push(skip(pool, slot, kind, &blockers.join("; ")));
        return Ok(());
    }
    let bytes = super::clean_rule::dir_bytes(checkout);
    if dry_run {
        actions.push(act(pool, slot, kind, bytes, checkout, "safe to remove"));
        return Ok(());
    }
    remove_then_restore(
        pool, slot, dir, checkout, kind, bytes, build_dir, reset_base, origin_sha, actions, skips,
    )
}

#[allow(clippy::too_many_arguments)]
fn remove_then_restore(
    pool: &str,
    slot: &str,
    dir: &Path,
    checkout: &Path,
    kind: ActionKind,
    bytes: u64,
    build_dir: Option<&str>,
    reset_base: &str,
    origin_sha: &str,
    actions: &mut Vec<CleanAction>,
    skips: &mut Vec<CleanSkip>,
) -> Result<(), VivariumError> {
    for path in super::clean_rule::build_paths(checkout, build_dir) {
        let _ignored = empty_dir(&path);
    }
    let source = source_of(dir, checkout);
    let Some(repo) = source else {
        skips.push(skip(pool, slot, kind, "no worktree source"));
        return Ok(());
    };
    match repo.remove_worktree_clean(checkout) {
        Ok(()) => {
            actions.push(act(pool, slot, kind, bytes, checkout, "removed"));
            if kind == ActionKind::Worktree {
                restore_at_origin(
                    dir, &repo, reset_base, origin_sha, pool, slot, actions, skips,
                );
            }
        }
        Err(e) => skips.push(skip(pool, slot, kind, &e.to_string())),
    }
    Ok(())
}

fn restore_at_origin(
    dir: &Path,
    repo: &Repo,
    reset_base: &str,
    origin_sha: &str,
    pool: &str,
    slot: &str,
    actions: &mut Vec<CleanAction>,
    skips: &mut Vec<CleanSkip>,
) {
    match super::reset::add_at_origin(dir, repo, reset_base, origin_sha) {
        Ok(out) => actions.push(super::reset::reset_action(pool, slot, &out)),
        Err(e) => skips.push(skip(
            pool,
            slot,
            ActionKind::Reset,
            &format!("WARN removed but reset failed: {e}"),
        )),
    }
}

fn fetch_reset(dir: &Path, reset_base: &str) -> Result<String, VivariumError> {
    let checkout = dir.join(CHECKOUT);
    let repo = source_of(dir, &checkout)
        .ok_or_else(|| VivariumError::Message("no worktree source".into()))?;
    super::reset::fetch_origin_base(&repo, reset_base)
}

fn source_of(slot: &Path, checkout: &Path) -> Option<Repo> {
    let file = if checkout.ends_with(CHECKOUT) {
        checkout.parent()?.join(WORKTREE_OF_FILE)
    } else {
        slot.join(WORKTREE_OF_FILE)
    };
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(_) => match std::fs::read_to_string(slot.join(WORKTREE_OF_FILE)) {
            Ok(text) => text,
            Err(_) => return None,
        },
    };
    let path = PathBuf::from(text.trim());
    path.is_dir().then(|| Repo::new(&path))
}

fn drop_scratch_branch(
    pool: &str,
    slot: &str,
    dir: &Path,
    merge_lane: &str,
    dry_run: bool,
    actions: &mut Vec<CleanAction>,
    skips: &mut Vec<CleanSkip>,
) {
    let Some(branch) = read_mark(dir, MARK_LAST_BRANCH) else {
        return;
    };
    if !branch.starts_with(crate::pump::need::unit_namespace_prefix()) {
        return;
    }
    let Some(repo) = source_of(dir, &dir.join(CHECKOUT)) else {
        return;
    };
    if !repo.is_ancestor(&format!("refs/heads/{branch}"), merge_lane) {
        skips.push(skip(
            pool,
            slot,
            ActionKind::Branch,
            &format!("{branch} is not in {merge_lane}"),
        ));
        return;
    }
    if dry_run {
        actions.push(act(
            pool,
            slot,
            ActionKind::Branch,
            0,
            Path::new(&branch),
            "local scratch",
        ));
        return;
    }
    match repo.delete_merged_branch(&branch) {
        Ok(()) => actions.push(act(
            pool,
            slot,
            ActionKind::Branch,
            0,
            Path::new(&branch),
            "deleted",
        )),
        Err(e) => skips.push(skip(pool, slot, ActionKind::Branch, &e.to_string())),
    }
}

/// Sweep source-repo worktrees that point at `root` and whose slot is gone
/// or whose unit is done (`done` is true for that path).
///
/// # Errors
/// Returns an error if Git cannot list worktrees.
pub fn sweep_orphans(
    pool: &str,
    source: &Path,
    root: &Path,
    merge_lane: Option<&str>,
    done: impl Fn(&Path) -> bool,
    dry_run: bool,
) -> Result<(Vec<CleanAction>, Vec<CleanSkip>), VivariumError> {
    let repo = Repo::new(source);
    let root = super::local_git::real_path(root);
    let source = super::local_git::real_path(source);
    let mut actions = Vec::new();
    let mut skips = Vec::new();
    for tree in repo.worktrees()? {
        let resolved = super::local_git::real_path(&tree.path);
        if tree.bare || resolved == source || !resolved.starts_with(&root) {
            continue;
        }
        sweep_one(
            pool,
            &repo,
            &tree.path,
            &root,
            merge_lane,
            done(&tree.path),
            dry_run,
            &mut actions,
            &mut skips,
        );
    }
    Ok((actions, skips))
}

#[allow(clippy::too_many_arguments)]
fn sweep_one(
    pool: &str,
    repo: &Repo,
    path: &Path,
    pool_root: &Path,
    merge_lane: Option<&str>,
    done: bool,
    dry_run: bool,
    actions: &mut Vec<CleanAction>,
    skips: &mut Vec<CleanSkip>,
) {
    if !path.exists() {
        if dry_run {
            actions.push(act(
                pool,
                "-",
                ActionKind::Orphan,
                0,
                path,
                "prunable entry",
            ));
        } else {
            match repo.remove_gone_worktree_under(path, pool_root) {
                Ok(()) => actions.push(act(
                    pool,
                    "-",
                    ActionKind::Orphan,
                    0,
                    path,
                    "removed stale entry",
                )),
                Err(error) => skips.push(skip(pool, "-", ActionKind::Orphan, &error.to_string())),
            }
        }
        return;
    }
    if !done {
        return;
    }
    let lane = merge_lane.unwrap_or("");
    if lane.is_empty() {
        skips.push(skip(pool, "-", ActionKind::Orphan, "no merge-lane ref"));
        return;
    }
    let blockers = worktree_blockers(path, lane, Some("target"));
    if !blockers.is_empty() {
        skips.push(skip(pool, "-", ActionKind::Orphan, &blockers.join("; ")));
        return;
    }
    if dry_run {
        actions.push(act(pool, "-", ActionKind::Orphan, 0, path, "done unit"));
        return;
    }
    match repo.remove_worktree_clean(path) {
        Ok(()) => actions.push(act(pool, "-", ActionKind::Orphan, 0, path, "removed")),
        Err(e) => skips.push(skip(pool, "-", ActionKind::Orphan, &e.to_string())),
    }
}

fn act(
    pool: &str,
    slot: &str,
    kind: ActionKind,
    bytes: u64,
    path: &Path,
    note: &str,
) -> CleanAction {
    CleanAction {
        pool: pool.to_string(),
        slot: slot.to_string(),
        kind,
        bytes,
        path: path.display().to_string(),
        note: note.to_string(),
    }
}

fn skip(pool: &str, slot: &str, kind: ActionKind, reason: &str) -> CleanSkip {
    CleanSkip {
        pool: pool.to_string(),
        slot: slot.to_string(),
        kind,
        reason: reason.to_string(),
    }
}

#[cfg(test)]
#[path = "clean_work_test.rs"]
mod tests;
