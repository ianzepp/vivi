//! A slot of the `local` provider is a directory under its pool directory:
//! `<root>/<pool>/slot-<n>/` for project-relative roots and
//! `<root>/<mailspace-id>/<pool>/slot-<n>/` for shared roots. Everything about
//! the slot is a file in that directory, and nothing is kept anywhere else:
//!
//! - `marks/<name>`: one small file per mark (mailspace, pool, slot, base,
//!   state, arch)
//! - `created`: the time the slot was made, UTC
//! - `claim`: exists while a task holds the slot; written whole, then hard-linked into place,
//!   so it appears complete or not at all, and of two claimants exactly one wins
//! - `reclaim`: exists while a build-tree reclaim empties the slot; made the same way as the
//!   claim, and both a dispatch claim and another reclaim back off while it is there
//! - `boot.pid`, `boot.start`, `exit_code`: the boot process (see [`super::local_proc`])
//! - `slot.env`, `boot.sh`, ...: the environment and the files delivered with the slot
//! - `repo/`: the checkout. In a worktree slot it is a Git worktree of the
//!   repository named in `worktree_of`.

use std::collections::BTreeMap;
use std::fs::DirBuilder;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rand_core::{OsRng, RngCore};

use super::docker::slot_files;
use super::job::sh_quote;
use super::local_git::Repo;
use super::local_proc as proc;
use super::provider::{CreateSpec, Hold, MachineRecord};
use super::slot::{META_CLAIMED, META_STATE, META_TASK, STATE_READY};
use super::ssh::{one_line, relative};
use super::ssh_state::{is_slot_id, name_of};
use crate::error::VivariumError;

/// What the listing's `region` column shows.
pub const WHERE: &str = "local";
/// Marks the root as made by Vivi, so the last `destroy` can remove it.
const ROOT_MARK: &str = ".vivi-root";
/// The file that records the repository a worktree slot was made from.
pub const WORKTREE_OF_FILE: &str = "worktree_of";
/// The checkout inside a slot.
pub const CHECKOUT: &str = "repo";
/// The file that holds the slot while a build-tree reclaim empties it.
pub const RECLAIM_FILE: &str = "reclaim";
/// Stand-ins for GNU tools a host may lack, put in the slot's `bin`.
pub(crate) const TOOLS: [(&str, &str); 3] = [
    ("timeout", include_str!("local-bin/timeout")),
    ("sha256sum", include_str!("local-bin/sha256sum")),
    ("nproc", include_str!("local-bin/nproc")),
];

fn failed(what: &str, path: &Path, e: &std::io::Error) -> VivariumError {
    VivariumError::Message(format!("{what} {}: {e}", path.display()))
}

fn refuse(why: &str) -> VivariumError {
    VivariumError::Message(format!("refusing to delete: {why}"))
}

/// The root as a path: `~/` is the home directory.
#[must_use]
pub fn expand_root(raw: &str) -> PathBuf {
    match raw.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().map_or_else(|| PathBuf::from(raw), |home| home.join(rest)),
        None => PathBuf::from(raw),
    }
}

fn private_dirs(path: &Path) -> Result<(), VivariumError> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|e| failed("cannot create", path, &e))
}

fn write_file(path: &Path, content: &[u8], mode: u32) -> Result<(), VivariumError> {
    if let Some(parent) = path.parent() {
        private_dirs(parent)?;
    }
    std::fs::write(path, content).map_err(|e| failed("cannot write", path, &e))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|e| failed("cannot protect", path, &e))
}

/// Write a file whole and move it into place.
pub(crate) fn replace_file(path: &Path, content: &[u8]) -> Result<(), VivariumError> {
    let partial = path.with_extension("partial");
    write_file(&partial, content, 0o600)?;
    std::fs::rename(&partial, path).map_err(|e| failed("cannot place", path, &e))
}

fn first_line(path: &Path) -> Option<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return None;
    };
    text.lines().next().map(|line| line.trim().to_string())
}

/// True when `dir` holds Vivi's pool mark.
#[must_use]
pub fn is_known(dir: &Path) -> bool {
    dir.join("marks/vivi_pool").is_file()
}

fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

/// The new slot's facts as the pool lists them.
#[must_use]
pub fn record_of(dir: &Path, id: &str) -> MachineRecord {
    let started = proc::is_alive(dir);
    let mut metadata = BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir(dir.join("marks")) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(value) = first_line(&entry.path()).filter(|v| !v.is_empty())
                && !name.ends_with(".partial")
            {
                metadata.insert(name, value);
            }
        }
    }
    if let Ok(claim) = std::fs::read_to_string(dir.join("claim")) {
        for line in claim.lines() {
            match line.split_once('=') {
                Some(("task", task)) => metadata.insert(META_TASK.to_string(), task.to_string()),
                Some(("claimed", at)) => metadata.insert(META_CLAIMED.to_string(), at.to_string()),
                _ => None,
            };
        }
    }
    let exit_code = if started {
        None
    } else {
        first_line(&dir.join(proc::EXIT_CODE)).and_then(|t| t.parse::<i64>().into_iter().next())
    };
    MachineRecord {
        id: id.to_string(),
        name: name_of(id),
        state: if started { "started" } else { "stopped" }.to_string(),
        region: WHERE.to_string(),
        created_at: first_line(&dir.join("created")).unwrap_or_default(),
        metadata,
        exit_code,
    }
}

/// True for the current three-part shared-root id and the two-part
/// project-local id.
#[must_use]
pub fn slot_id_ok(id: &str) -> bool {
    if is_slot_id(id) {
        return true;
    }
    let parts: Vec<&str> = id.split('/').collect();
    let [pool, slot] = parts.as_slice() else {
        return false;
    };
    !pool.is_empty()
        && !pool.starts_with('-')
        && pool
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && slot
            .strip_prefix("slot-")
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// List only the direct slot directories of one resolved pool site.
///
/// # Errors
/// Returns an error if the pool directory cannot be read.
pub fn list(site: &super::local_site::LocalSite) -> Result<Vec<MachineRecord>, VivariumError> {
    let mut found = Vec::new();
    for slot in subdirs(&site.pool_dir)? {
        let Some(name) = slot.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("slot-") || !is_known(&slot) {
            continue;
        }
        let id = if site.relative {
            format!("{}/{name}", site.pool)
        } else {
            format!("{}/{}/{name}", site.mailspace, site.pool)
        };
        if slot_id_ok(&id) {
            found.push(record_of(&slot, &id));
        }
    }
    Ok(found)
}

fn id_below(root: &Path, dir: &Path) -> String {
    dir.strip_prefix(root)
        .map(|rel| rel.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The plain directories directly inside `dir` (links and dot names are
/// skipped); none when `dir` is not there.
fn subdirs(dir: &Path) -> Result<Vec<PathBuf>, VivariumError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(failed("cannot read", dir, &e)),
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    found.sort();
    Ok(found)
}

fn env_file(spec: &CreateSpec) -> String {
    spec.env
        .iter()
        .map(|(key, value)| format!("{key}={}\n", sh_quote(value)))
        .collect()
}

/// Lay the slot's files out in a fresh directory next to its final place.
fn lay_out(part: &Path, spec: &CreateSpec) -> Result<(), VivariumError> {
    for (key, value) in &spec.metadata {
        one_line(value)?;
        write_file(
            &part.join("marks").join(key),
            format!("{value}\n").as_bytes(),
            0o600,
        )?;
    }
    write_file(
        &part.join("marks/vivi_pool_arch"),
        format!("{}\n", arch()).as_bytes(),
        0o600,
    )?;
    write_file(&part.join("slot.env"), env_file(spec).as_bytes(), 0o600)?;
    for (path, content) in slot_files(spec) {
        write_file(&part.join(relative(&path)?), &content, 0o600)?;
    }
    for (name, text) in TOOLS {
        write_file(&part.join("bin").join(name), text.as_bytes(), 0o700)?;
    }
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    write_file(&part.join("created"), format!("{now}\n").as_bytes(), 0o600)
}

/// Make the slot `id` from `spec`: its files, then, for a worktree slot, its
/// checkout. The slot is not started.
///
/// # Errors
/// Returns an error if the slot exists, a file cannot be written, or Git
/// refuses the worktree (the half-made slot is removed first).
pub fn create(root: &Path, id: &str, spec: &CreateSpec) -> Result<(), VivariumError> {
    let dest = root.join(id);
    if dest.symlink_metadata().is_ok() {
        return Err(VivariumError::Message(format!(
            "slot directory exists: {}",
            dest.display()
        )));
    }
    if !root.exists() {
        private_dirs(root)?;
        write_file(&root.join(ROOT_MARK), b"", 0o600)?;
    }
    let pool_dir = dest.parent().unwrap_or(root);
    private_dirs(pool_dir)?;
    let part = pool_dir.join(format!(
        ".partial-{}.{}",
        id.rsplit('/').next().unwrap_or(""),
        std::process::id()
    ));
    if let Err(e) = lay_out(&part, spec) {
        std::fs::remove_dir_all(&part).map(drop).unwrap_or_default();
        return Err(e);
    }
    std::fs::rename(&part, &dest).map_err(|e| failed("cannot place", &dest, &e))?;
    match spec.env.get("WORKTREE_OF") {
        Some(repo) => add_worktree(&dest, repo, spec.env.get("BASE")),
        None => Ok(()),
    }
}

/// Make the slot's checkout as a detached worktree of `repo` at `base`.
fn add_worktree(dest: &Path, repo: &str, base: Option<&String>) -> Result<(), VivariumError> {
    let repo = Repo::new(Path::new(repo));
    let made = write_file(
        &dest.join(WORKTREE_OF_FILE),
        repo.path().to_string_lossy().as_bytes(),
        0o600,
    )
    .and_then(|()| {
        let base = base.ok_or_else(|| VivariumError::Message("slot spec lacks BASE".into()))?;
        repo.add_worktree(&dest.join(CHECKOUT), base)
    });
    if let Err(e) = made {
        let checkout = dest.join(CHECKOUT);
        let pool_root = dest.parent().unwrap_or(dest);
        std::fs::remove_dir_all(dest).map(drop).unwrap_or_default();
        if checkout.exists() {
            repo.remove_worktree(&checkout)
                .map(drop)
                .unwrap_or_default();
        } else {
            repo.remove_gone_worktree_under(&checkout, pool_root)
                .map(drop)
                .unwrap_or_default();
        }
        return Err(e);
    }
    Ok(())
}

fn nonce() -> String {
    let mut bytes = [0_u8; 12];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Whether the slot may be claimed: seeded, not running, and not held by a
/// build-tree reclaim.
fn free_now(dir: &Path) -> bool {
    first_line(&dir.join("marks").join(META_STATE)).as_deref() == Some(STATE_READY)
        && !dir.join(RECLAIM_FILE).is_file()
        && !proc::is_alive(dir)
}

/// Claim the slot for `task` at `now`. The claim is one file, written whole
/// and then hard-linked to its name: the link fails if the name exists, so of
/// two claimants, in any processes, exactly one gets it.
///
/// # Errors
/// Returns an error if the slot is gone or the files cannot be written.
pub fn claim(dir: &Path, task: &str, now: u64) -> Result<Option<Hold>, VivariumError> {
    if !is_known(dir) {
        return Err(VivariumError::Message(format!(
            "slot {} is gone",
            dir.display()
        )));
    }
    let nonce = nonce();
    let tmp = dir.join(format!("claim.{}.{nonce}.tmp", std::process::id()));
    write_file(
        &tmp,
        format!("task={task}\nclaimed={now}\nnonce={nonce}\n").as_bytes(),
        0o600,
    )?;
    let linked = std::fs::hard_link(&tmp, dir.join("claim"));
    std::fs::remove_file(&tmp).map(drop).unwrap_or_default();
    match linked {
        Ok(()) if free_now(dir) => {
            let _ignored =
                super::clean_rule::write_mark(dir, super::clean_rule::MARK_LAST_TASK, task);
            Ok(Some(Hold { nonce }))
        }
        Ok(()) => {
            std::fs::remove_file(dir.join("claim"))
                .map(drop)
                .unwrap_or_default();
            Ok(None)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
        Err(e) => Err(failed("cannot create the claim in", dir, &e)),
    }
}

/// Drop the claim. Absent is not an error.
///
/// # Errors
/// Returns an error if the file cannot be removed.
pub fn unclaim(dir: &Path) -> Result<(), VivariumError> {
    match std::fs::remove_file(dir.join("claim")) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(failed("cannot remove the claim in", dir, &e))
        }
        _ => Ok(()),
    }
}

/// Hold the slot while a build-tree reclaim empties it. The hold is one file,
/// written whole and then hard-linked to its name, like the claim: of two
/// reclaimers, in any processes, exactly one wins. `Ok(false)` means the slot
/// is already held — by another reclaim or by a dispatch claim — and the
/// caller must not empty it. The holder must call [`release_reclaim`].
///
/// # Errors
/// Returns an error if the slot is gone or the files cannot be written.
pub fn hold_reclaim(dir: &Path) -> Result<bool, VivariumError> {
    if !is_known(dir) {
        return Err(VivariumError::Message(format!(
            "slot {} is gone",
            dir.display()
        )));
    }
    let nonce = nonce();
    let tmp = dir.join(format!("reclaim.{}.{nonce}.tmp", std::process::id()));
    write_file(&tmp, format!("nonce={nonce}\n").as_bytes(), 0o600)?;
    let linked = std::fs::hard_link(&tmp, dir.join(RECLAIM_FILE));
    std::fs::remove_file(&tmp).map(drop).unwrap_or_default();
    match linked {
        Ok(()) if !dir.join("claim").is_file() => Ok(true),
        Ok(()) => {
            std::fs::remove_file(dir.join(RECLAIM_FILE))
                .map(drop)
                .unwrap_or_default();
            Ok(false)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(failed("cannot create the reclaim hold in", dir, &e)),
    }
}

/// Drop the reclaim hold. Absent is not an error.
///
/// # Errors
/// Returns an error if the file cannot be removed.
pub fn release_reclaim(dir: &Path) -> Result<(), VivariumError> {
    match std::fs::remove_file(dir.join(RECLAIM_FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(failed("cannot remove the reclaim hold in", dir, &e))
        }
        _ => Ok(()),
    }
}

/// Write `content` to `path` (below the slot) for the holder of the claim.
///
/// # Errors
/// Returns an error if `hold` is not the claim holder or the file cannot be written.
pub fn put(dir: &Path, hold: &Hold, path: &str, content: &[u8]) -> Result<(), VivariumError> {
    let claim = std::fs::read_to_string(dir.join("claim")).unwrap_or_default();
    let nonce = claim.lines().find_map(|l| l.strip_prefix("nonce="));
    if nonce != Some(hold.nonce.as_str()) {
        return Err(VivariumError::Message(format!(
            "not the claim holder of {}",
            dir.display()
        )));
    }
    replace_file(&dir.join(relative(path)?), content)
}

/// Set the mark `key` of the slot to `value`.
///
/// # Errors
/// Returns an error if the name is not a mark name or the file cannot be written.
pub fn seal(dir: &Path, key: &str, value: &str) -> Result<(), VivariumError> {
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
        return Err(VivariumError::Message(format!("bad mark name: {key}")));
    }
    one_line(value)?;
    replace_file(
        &dir.join("marks").join(key),
        format!("{value}\n").as_bytes(),
    )
}

/// The files of the slot at `paths` (as the pool spells them) that exist.
///
/// # Errors
/// Returns an error if a path is not inside the slot.
pub fn read_files(dir: &Path, paths: &[&str]) -> Result<BTreeMap<String, Vec<u8>>, VivariumError> {
    let mut files = BTreeMap::new();
    for path in paths {
        if let Ok(content) = std::fs::read(dir.join(relative(path)?)) {
            files.insert((*path).to_string(), content);
        }
    }
    Ok(files)
}

/// Put a worktree slot's checkout back on a detached HEAD, when the slot is
/// not running: the branch it last worked on is then free to be checked out
/// or merged elsewhere. The working tree is not touched.
pub fn detach_if_idle(dir: &Path) {
    let checkout = dir.join(CHECKOUT);
    if !dir.join(WORKTREE_OF_FILE).is_file()
        || !checkout.join(".git").exists()
        || proc::is_alive(dir)
    {
        return;
    }
    let done = std::process::Command::new("git")
        .arg("-C")
        .arg(&checkout)
        .args(["checkout", "-q", "--detach"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output();
    match done {
        Ok(out) if !out.status.success() => eprintln!(
            "warning: could not detach {}: {}",
            checkout.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => eprintln!("warning: could not detach {}: {e}", checkout.display()),
        Ok(_) => {}
    }
}

fn mark_is(dir: &Path, name: &str, want: &str) -> bool {
    first_line(&dir.join("marks").join(name)).as_deref() == Some(want)
}

/// The slot directory `id` is safe to delete: it is a slot id; the root is a
/// real directory of its own (not `/`, not the home directory); the slot is a
/// plain directory (not a link) that lies inside the root; and it carries
/// Vivi's own marks naming this pool and this mailspace. `Ok(None)` when
/// there is nothing to delete.
fn checked_slot(root: &Path, id: &str) -> Result<Option<(PathBuf, PathBuf)>, VivariumError> {
    if !slot_id_ok(id) {
        return Err(refuse(&format!("not a slot id: {id}")));
    }
    let Ok(real_root) = root.canonicalize() else {
        return Ok(None);
    };
    let home = dirs::home_dir().and_then(|h| h.canonicalize().map_or(None, Some));
    if real_root == Path::new("/") || home.as_ref() == Some(&real_root) {
        return Err(refuse(&format!(
            "the root is / or the home directory: {}",
            real_root.display()
        )));
    }
    let dir = root.join(id);
    let Ok(meta) = dir.symlink_metadata() else {
        return Ok(None);
    };
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(refuse(&format!("{id} is not a plain directory")));
    }
    let parts: Vec<&str> = id.split('/').collect();
    let pool = parts[parts.len() - 2];
    if !mark_is(&dir, "vivi_pool", pool) {
        return Err(refuse(&format!(
            "{id} carries no Vivi mark for pool {pool}"
        )));
    }
    if parts.len() == 3 && !mark_is(&dir, "vivi_pool_mailspace", parts[0]) {
        let space = parts[0];
        return Err(refuse(&format!(
            "{id} carries no Vivi mark for mailspace {space}"
        )));
    }
    let real = dir
        .canonicalize()
        .map_err(|e| failed("cannot resolve", &dir, &e))?;
    if !real.starts_with(&real_root) || id_below(&real_root, &real) != id {
        return Err(refuse(&format!(
            "{} is outside {}",
            real.display(),
            real_root.display()
        )));
    }
    Ok(Some((real_root, real)))
}

/// Delete the slot `id`, under the guards of [`checked_slot`]: stop what it
/// runs, remove its worktree through Git (which never touches a branch), then
/// the directory. A slot that is not there is already gone.
///
/// # Errors
/// Returns an error if a guard refuses, something cannot be stopped, or Git
/// will not remove the worktree.
pub fn destroy(root: &Path, id: &str) -> Result<(), VivariumError> {
    let Some((real_root, real)) = checked_slot(root, id)? else {
        return Ok(());
    };
    proc::stop(&real)?;
    let pool_root = pool_root(&real_root, id);
    let repo = std::fs::read_to_string(real.join(WORKTREE_OF_FILE))
        .map_or(None, |text| Some(Repo::new(Path::new(text.trim()))))
        .filter(|repo| repo.path().is_dir());
    if let Some(repo) = &repo {
        let checkout = real.join(CHECKOUT);
        if checkout.exists() {
            repo.remove_worktree(&checkout)?;
        } else {
            repo.remove_gone_worktree_under(&checkout, &pool_root)?;
        }
    }
    remove_target_worktrees(&real, &pool_root)?;
    std::fs::remove_dir_all(&real).map_err(|e| failed("cannot delete", &real, &e))?;
    tidy(&real_root, id);
    Ok(())
}

/// Remove only worktrees registered beneath this already validated slot.
fn remove_target_worktrees(slot: &Path, pool_root: &Path) -> Result<(), VivariumError> {
    let targets = slot.join("targets");
    let entries = match std::fs::read_dir(&targets) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let cache = entry?.path();
        if cache.symlink_metadata()?.file_type().is_symlink() {
            continue;
        }
        if let Ok(source) = std::fs::read_to_string(cache.join(WORKTREE_OF_FILE)) {
            let repo = Repo::new(Path::new(source.trim()));
            let checkout = cache.join(CHECKOUT);
            if checkout.exists() {
                repo.remove_worktree(&checkout)?;
            } else {
                repo.remove_gone_worktree_under(&checkout, pool_root)?;
            }
        }
    }
    Ok(())
}

fn pool_root(root: &Path, id: &str) -> PathBuf {
    let parts: Vec<&str> = id.split('/').collect();
    if parts.len() == 2 {
        root.join(parts[0])
    } else {
        root.join(parts[0]).join(parts[1])
    }
}

/// Remove the pool directory, the mailspace directory and, if Vivi made it,
/// the root, as each becomes empty.
fn tidy(root: &Path, id: &str) {
    let parts: Vec<&str> = id.split('/').collect();
    let pool_dir = if parts.len() == 2 {
        root.join(parts[0])
    } else {
        root.join(parts[0]).join(parts[1])
    };
    let dirs = if parts.len() == 2 {
        vec![pool_dir]
    } else {
        vec![pool_dir, root.join(parts[0])]
    };
    for dir in dirs {
        std::fs::remove_dir(&dir).map(drop).unwrap_or_default();
    }
    let only_mark = std::fs::read_dir(root).is_ok_and(|entries| {
        let names: Vec<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names == [ROOT_MARK]
    });
    if only_mark {
        std::fs::remove_file(root.join(ROOT_MARK))
            .map(drop)
            .unwrap_or_default();
        std::fs::remove_dir(root).map(drop).unwrap_or_default();
    }
}

#[cfg(test)]
#[path = "local_slots_test.rs"]
mod tests;

#[cfg(test)]
#[path = "local_shim_test.rs"]
mod shim_tests;
