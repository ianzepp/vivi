//! Companion tests for receipt storage.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use tempfile::{TempDir, tempdir};

use super::{REPLY_DIR, save_receipt};
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobSpec, PoolJob};

/// The fake clock's start.
const T0: u64 = 1_000_000;
/// A receipt with Unicode, blank lines, and a trailing newline, saved
/// verbatim.
const RECEIPT: &str = "worker receipt ✓ — résumé\n第二段落\n\nlast\n";

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    host: Arc<FakeHost>,
}

fn rig() -> Rig {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.set_now(T0);
    Rig {
        _dir: dir,
        ctx,
        host,
    }
}

impl Rig {
    fn replies(&self) -> PathBuf {
        self.ctx.home.join(REPLY_DIR)
    }

    fn artifact(&self, id: &str) -> PathBuf {
        self.replies().join(format!("{id}.txt"))
    }

    fn job(&self, id: &str) -> PoolJob {
        let mut job = PoolJob::from(JobSpec::new(id, "merge", "witness", "subject", "body"));
        job.outcome = Some(JobOutcome {
            state: String::from("finished"),
            detail: String::from("ok"),
            receipt: String::from(RECEIPT),
            at: 7,
        });
        job
    }

    fn job_without_outcome(&self, id: &str) -> PoolJob {
        PoolJob::from(JobSpec::new(id, "merge", "witness", "subject", "body"))
    }

    fn saved_secs(&self, path: &Path) -> u64 {
        let modified = fs::metadata(path).unwrap().modified().unwrap();
        modified.duration_since(UNIX_EPOCH).unwrap().as_secs()
    }

    fn mode(&self, path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// Every entry name in the receipt directory, sorted.
    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.replies())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

#[test]
fn saves_exact_unicode_bytes_with_trailing_suffixes() {
    let rig = rig();
    let saved = save_receipt(&rig.ctx, &rig.job("job.one")).unwrap();
    assert_eq!(saved, Path::new(REPLY_DIR).join("job.one.txt"));
    assert!(!saved.is_absolute());
    assert_eq!(
        fs::read(&rig.ctx.home.join(&saved)).unwrap(),
        RECEIPT.as_bytes()
    );
    // A job ID that already ends in `.txt` keeps it; no suffix is stripped
    // or normalized away.
    let saved = save_receipt(&rig.ctx, &rig.job("report.txt")).unwrap();
    assert_eq!(saved, Path::new(REPLY_DIR).join("report.txt.txt"));
    assert_eq!(
        fs::read(rig.ctx.home.join(&saved)).unwrap(),
        RECEIPT.as_bytes()
    );
}

#[test]
fn missing_outcome_saves_an_empty_receipt() {
    let rig = rig();
    let saved = save_receipt(&rig.ctx, &rig.job_without_outcome("pending")).unwrap();
    let artifact = rig.ctx.home.join(&saved);
    assert_eq!(fs::metadata(&artifact).unwrap().len(), 0);
    assert_eq!(rig.saved_secs(&artifact), T0);
}

#[test]
fn receipts_and_managed_directory_are_private() {
    let rig = rig();
    let saved = save_receipt(&rig.ctx, &rig.job("private")).unwrap();
    assert_eq!(rig.mode(&rig.replies()), 0o700);
    assert_eq!(rig.mode(&rig.ctx.home.join(&saved)), 0o600);
    // An existing directory with looser permissions is tightened.
    fs::create_dir_all(&rig.replies()).unwrap();
    fs::set_permissions(&rig.replies(), fs::Permissions::from_mode(0o755)).unwrap();
    save_receipt(&rig.ctx, &rig.job("private")).unwrap();
    assert_eq!(rig.mode(&rig.replies()), 0o700);
}

#[test]
fn rejects_invalid_job_ids_and_path_traversal() {
    let rig = rig();
    for id in ["", ".", "..", "a/b", "../escape", "has space", "café"] {
        let error = save_receipt(&rig.ctx, &rig.job(id)).unwrap_err();
        assert!(
            error.to_string().contains("invalid pump job ID"),
            "id {id:?}: {error}"
        );
    }
    // Validation happens before anything is created.
    assert!(!rig.replies().exists());
}

#[test]
fn rejects_symlinked_managed_directory() {
    let rig = rig();
    let elsewhere = rig.ctx.home.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    symlink(&elsewhere, rig.replies()).unwrap();
    let error = save_receipt(&rig.ctx, &rig.job("job7")).unwrap_err();
    assert!(error.to_string().contains("symlink"), "{error}");
    // The symlink was never followed: nothing landed in the target.
    assert!(fs::read_dir(&elsewhere).unwrap().next().is_none());
}

#[test]
fn rejects_symlinked_artifact_without_following_it() {
    let rig = rig();
    fs::create_dir_all(&rig.replies()).unwrap();
    let victim = rig.ctx.home.join("victim.txt");
    fs::write(&victim, "keep\n").unwrap();
    symlink(&victim, rig.artifact("job8")).unwrap();
    let error = save_receipt(&rig.ctx, &rig.job("job8")).unwrap_err();
    assert!(error.to_string().contains("symlink"), "{error}");
    assert_eq!(fs::read_to_string(&victim).unwrap(), "keep\n");
    // A dangling artifact link is refused too, and never creates the target.
    symlink(rig.ctx.home.join("missing"), rig.artifact("job9")).unwrap();
    save_receipt(&rig.ctx, &rig.job("job9")).unwrap_err();
    assert!(!rig.ctx.home.join("missing").exists());
    assert_eq!(rig.entries(), vec!["job8.txt", "job9.txt"]);
}

#[test]
fn rejects_obstructed_storage() {
    // The artifact path holds a directory: refuse, leave it intact, and
    // leave no staged temporary behind.
    let blocked_dir = rig();
    fs::create_dir_all(blocked_dir.artifact("job10")).unwrap();
    let error = save_receipt(&blocked_dir.ctx, &blocked_dir.job("job10")).unwrap_err();
    assert!(
        error.to_string().contains("not a regular receipt file"),
        "{error}"
    );
    assert!(blocked_dir.artifact("job10").is_dir());
    assert_eq!(blocked_dir.entries(), vec!["job10.txt"]);
    // The managed directory path holds a regular file: refuse.
    let blocked = rig();
    fs::write(blocked.replies(), b"not a directory").unwrap();
    let error = save_receipt(&blocked.ctx, &blocked.job("job10")).unwrap_err();
    assert!(
        error.to_string().contains("not a regular receipt file"),
        "{error}"
    );
    assert_eq!(fs::read(blocked.replies()).unwrap(), b"not a directory");
}

#[test]
fn reobservation_preserves_bytes_and_retention_timestamp() {
    let rig = rig();
    let saved = save_receipt(&rig.ctx, &rig.job("job11")).unwrap();
    let artifact = rig.ctx.home.join(&saved);
    assert_eq!(rig.saved_secs(&artifact), T0);
    rig.host.advance(500);
    let again = save_receipt(&rig.ctx, &rig.job("job11")).unwrap();
    assert_eq!(again, saved);
    assert_eq!(fs::read(&artifact).unwrap(), RECEIPT.as_bytes());
    assert_eq!(rig.saved_secs(&artifact), T0);
    assert_eq!(rig.entries(), vec!["job11.txt"]);
}

#[test]
fn failure_publishes_no_partial_destination() {
    // An unwritable pump home fails the save before anything is staged.
    let sealed = rig();
    fs::set_permissions(&sealed.ctx.home, fs::Permissions::from_mode(0o500)).unwrap();
    let error = save_receipt(&sealed.ctx, &sealed.job("job12")).unwrap_err();
    fs::set_permissions(&sealed.ctx.home, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(error.to_string().contains("job-replies"), "{error}");
    assert!(!sealed.replies().exists());
    // An obstructed destination fails the publish: no partial file, no
    // temporary left behind.
    let blocked = rig();
    fs::create_dir_all(&blocked.replies()).unwrap();
    fs::create_dir_all(blocked.artifact("job12")).unwrap();
    save_receipt(&blocked.ctx, &blocked.job("job12")).unwrap_err();
    assert_eq!(blocked.entries(), vec!["job12.txt"]);
    // A symlinked destination fails without writing through the link.
    let linked = rig();
    fs::create_dir_all(&linked.replies()).unwrap();
    let victim = linked.ctx.home.join("victim.txt");
    fs::write(&victim, "keep\n").unwrap();
    symlink(&victim, linked.artifact("job12")).unwrap();
    save_receipt(&linked.ctx, &linked.job("job12")).unwrap_err();
    assert_eq!(fs::read_to_string(&victim).unwrap(), "keep\n");
    assert_eq!(linked.entries(), vec!["job12.txt"]);
}
