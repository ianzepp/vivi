#![allow(dead_code)]
//! Isolated real-Git checkout regression fixture for need-branch checkout
//! work. A rig is a real mailspace whose `[pump]` carries two explicit
//! targets — the `default` workspace repository and a `side` repository with
//! a different path and a different main (`trunk`) — plus a local pool in
//! one of two source modes: fetch-backed slots (a pool remote the boot
//! fetches) or worktree-backed slots (`worktree_of`). Each target has a
//! temporary bare origin and a pre-made integration checkout under the pump
//! home, so a pump child starts without cloning.
//!
//! The seeding cuts a real need branch in the side repository at the trunk
//! tip, grows each marker file on its own slice side branch, and integrates
//! both through real no-fast-forward merge commits (the production merge
//! message), recording the original base and both merge commits. Durable
//! run/slice evidence lands in `pump.db` with the need branch row pointing
//! at the non-primary target. The worker stays isolated: neither marker and
//! no local need-branch ref before any dispatch.
//!
//! The stand-in harness only reads: it records HEAD, branch and marker
//! evidence for its brief and replies with a valid envelope. It never
//! checks out the desired ref and never substitutes a SHA.
//!
//! Include from a test target with
//! `#[path = "support/pump_need_checkout.rs"] mod pump_need_checkout;`.
//! Patterns (not imports) come from `tests/pump_job_survival_cli.rs`.

use std::fs::{self, OpenOptions};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use vivi::letter::render;
use vivi::letter::work::{NeedFront, TaskFront};
use vivi::mailspace::{Mailspace, SendRequest};
use vivi::pump::flow::index::{FLOW_VERSION_NUMBER, RunRow, StepState, run_state};
use vivi::pump::flow::mail::{MailWriter, MailspacePort};
use vivi::pump::flow::need_branch;
use vivi::pump::jobs;
use vivi::pump::state::State;
use vivi::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};

/// The primary workspace target id.
pub const PRIMARY_TARGET: &str = "default";
/// The non-primary need target: another repository with another main.
pub const SIDE_TARGET: &str = "side";
/// Main of the side target, deliberately not the primary's `main`.
pub const SIDE_MAIN: &str = "trunk";
/// First marker file: it exists only on its slice side branch and the need
/// branch, never on the side main.
pub const MARKER_A: &str = "checkout-marker-a.txt";
/// Second marker file, likewise.
pub const MARKER_B: &str = "checkout-marker-b.txt";
/// The message the production slice merge uses; the fixture merges with the
/// exact same no-fast-forward shape.
pub const MERGE_MESSAGE: &str = "Merge one slice onto the need branch";
/// The run every rig seeds.
pub const RUN: u32 = 1;
/// The node of the seeded slice step row.
pub const SLICE_NODE: &str = "slice:s1";

/// Stand-in harness: records HEAD, branch and marker evidence for its brief
/// under `$VIVI_TEST_MARKERS/<id>.evidence`, then answers with a valid
/// envelope. It only reads the checkout it was started in.
pub const TEST_HARNESS: &str = r#"#!/bin/sh
set -eu
brief="$1"
id=$(sed -n 's/^id = "\(.*\)"$/\1/p' "$brief" | head -n 1)
kind=$(sed -n 's/^kind = "\(.*\)"$/\1/p' "$brief" | head -n 1)
head=$(git rev-parse HEAD 2>/dev/null || printf 'no-git')
branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || printf 'no-git')
marker() { [ -f "$1" ] && printf present || printf absent; }
{
  printf 'id=%s\n' "$id"
  printf 'kind=%s\n' "$kind"
  printf 'head=%s\n' "$head"
  printf 'branch=%s\n' "$branch"
  printf 'marker_a=%s\n' "$(marker checkout-marker-a.txt)"
  printf 'marker_b=%s\n' "$(marker checkout-marker-b.txt)"
} > "$VIVI_TEST_MARKERS/$id.evidence"
printf '%s\n' '+++' 'answer = "built"' 'change = []' 'deviations = []' 'remaining = []' '+++' 'checkout fixture hand survived'
printf 'done\n' > "$VIVI_TEST_MARKERS/$id.done"
"#;

/// How the pool's slots take source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalMode {
    /// Fetch-backed: the pool names a remote its boot fetches; no worktree.
    Fetch,
    /// Worktree-backed: slots are Git worktrees of the primary integration
    /// checkout.
    Worktree,
}

/// One scaffolded rig.
pub struct Rig {
    pub dir: tempfile::TempDir,
    pub project: PathBuf,
    pub home: PathBuf,
    pub marker_dir: PathBuf,
    pub child_log: PathBuf,
    /// Tip of the primary integration checkout at scaffold time.
    pub tip: String,
    /// Primary bare origin.
    pub bare: PathBuf,
    /// Primary seed checkout.
    pub seed: PathBuf,
    /// Non-primary bare origin.
    pub side_bare: PathBuf,
    /// Non-primary seed checkout: holds trunk, the slice side branches and
    /// the seeded need branch.
    pub side_seed: PathBuf,
    pub mode: LocalMode,
}

impl Rig {
    /// The integration checkout of one target under the pump home. With
    /// explicit targets every target nests under `targets/<id>/repo`.
    #[must_use]
    pub fn target_repo(&self, id: &str) -> PathBuf {
        self.home.join("targets").join(id).join("repo")
    }
}

/// What the seeding did, with every recorded SHA.
pub struct SeededCheckout {
    pub need: String,
    /// The side main tip the need branch was cut from; carries no marker.
    pub base: String,
    /// First no-fast-forward merge commit: integrates the first slice branch.
    pub merge_a: String,
    /// Second no-fast-forward merge commit: integrates the second slice
    /// branch onto the first merge.
    pub merge_b: String,
    /// The need branch tip after both merges (also on the bare origin).
    pub need_tip: String,
    /// Tip of the first slice side branch (adds the first marker).
    pub slice_a_tip: String,
    /// Tip of the second slice side branch (adds the second marker).
    pub slice_b_tip: String,
    pub sentinel_task: String,
    pub slice_task: String,
}

/// Scaffold the whole rig for one local pool mode: real mailspace, two
/// targets with bare origins and integration checkouts, the local pool, and
/// the stand-in harness script.
pub fn scaffold(mode: LocalMode) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let project = base.join("project");
    fs::create_dir(&project).unwrap();
    // The workspace itself is a Git repository: the default target's path
    // is the workspace root.
    git(&project, &["init", "-q", "-b", "main"]);
    git(&project, &["config", "user.name", "Checkout Fixture"]);
    git(&project, &["config", "user.email", "checkout@localhost"]);
    fs::write(project.join("README.md"), "checkout fixture workspace\n").unwrap();
    git(&project, &["add", "README.md"]);
    git(&project, &["commit", "-q", "-m", "fixture workspace"]);
    assert_success(&vivi(["mailspace", "init", "--project", path(&project)]));

    let bare = base.join("origin.git");
    let seed = base.join("seed");
    fs::create_dir(&seed).unwrap();
    git(&seed, &["init", "-q", "-b", "main"]);
    git(&seed, &["config", "user.name", "Checkout Fixture"]);
    git(&seed, &["config", "user.email", "checkout@localhost"]);
    fs::write(seed.join("README.md"), "primary fixture\n").unwrap();
    git(&seed, &["add", "README.md"]);
    git(&seed, &["commit", "-q", "-m", "primary fixture"]);
    git(&seed, &["clone", "-q", "--bare", ".", path(&bare)]);
    git(&seed, &["push", "-q", path(&bare), "main"]);

    // The non-primary target: its own repository nested in the workspace,
    // its own bare origin, and a main with another name.
    let side_bare = base.join("side-origin.git");
    let side_seed = project.join("side");
    fs::create_dir(&side_seed).unwrap();
    git(&side_seed, &["init", "-q", "-b", SIDE_MAIN]);
    git(&side_seed, &["config", "user.name", "Checkout Fixture"]);
    git(&side_seed, &["config", "user.email", "checkout@localhost"]);
    fs::write(side_seed.join("SIDE.md"), "side target\n").unwrap();
    git(&side_seed, &["add", "SIDE.md"]);
    git(&side_seed, &["commit", "-q", "-m", "side fixture"]);
    git(
        &side_seed,
        &["clone", "-q", "--bare", ".", path(&side_bare)],
    );
    git(&side_seed, &["push", "-q", path(&side_bare), SIDE_MAIN]);

    let home = project.join(".vivi/pump-home");
    fs::create_dir_all(&home).unwrap();
    for (id, url, branch) in [
        (PRIMARY_TARGET, path(&bare), "main"),
        (SIDE_TARGET, path(&side_bare), SIDE_MAIN),
    ] {
        let repo = home.join("targets").join(id).join("repo");
        fs::create_dir_all(&repo.parent().unwrap()).unwrap();
        git(&home, &["clone", "-q", "-b", branch, url, path(&repo)]);
    }
    let repo = rig_primary_repo(&home);
    let tip = git_output(&repo, &["rev-parse", "HEAD"]);
    let markers = base.join("markers");
    fs::create_dir(&markers).unwrap();
    let child_log = base.join("pump-child.log");
    let script = base.join("worker.sh");
    fs::write(&script, TEST_HARNESS).unwrap();

    write_config(mode, &project, &home, &bare, &script);
    let created = vivi(["pool", "create", "--pool", "p", "--project", path(&project)]);
    assert_success(&created);
    Rig {
        dir,
        project,
        home,
        marker_dir: markers,
        child_log,
        tip,
        bare,
        seed,
        side_bare,
        side_seed,
        mode,
    }
}

fn rig_primary_repo(home: &Path) -> PathBuf {
    home.join("targets").join(PRIMARY_TARGET).join("repo")
}

/// Replace the starter `[pump]` table with the two-target fixture config.
fn write_config(mode: LocalMode, project: &Path, home: &Path, bare: &Path, script: &Path) {
    let config_path = project.join(".vivi/mailspace.toml");
    let old = fs::read_to_string(&config_path).unwrap();
    let mut parsed: toml::Value = toml::from_str(&old).unwrap();
    // `mailspace init` writes a starter [pump]; this fixture supplies its own.
    parsed.as_table_mut().unwrap().remove("pump");
    let pool_remote = match mode {
        LocalMode::Fetch => format!("remote_url = {}\n", toml_string(bare)),
        LocalMode::Worktree => String::new(),
    };
    let worktree = match mode {
        LocalMode::Fetch => String::new(),
        LocalMode::Worktree => format!(
            "worktree_of = {}\n",
            toml_string(&home.join("targets").join(PRIMARY_TARGET).join("repo"))
        ),
    };
    let additions = format!(
        "\n[pump]\nhome = {}\nremote = \"origin\"\nremote_url = {}\nmain = \"main\"\ntick = 1\ncommitter_name = \"Checkout Fixture\"\ncommitter_email = \"checkout@localhost\"\nfrom = \"mind\"\nto = \"hand\"\nreply_from = \"mind\"\n\n[pump.board]\nfrom = []\npull = false\n\n[pump.targets.default]\npath = \".\"\nmain = \"main\"\n\n[pump.targets.side]\npath = \"side\"\nmain = \"trunk\"\n\n[pools.p]\nprovider = \"local\"\nslots = 2\n{}\n[pools.p.bands]\nP2 = [{{ model = \"standin\", reasoning = \"low\" }}]\n\n[pools.p.local]\nroot = {}\n{}\n[pools.p.harness]\nname = \"standin\"\nrun = \"bash '{}' {{brief}}\"\nmodels = [\"standin\"]\nenv = [\"VIVI_TEST_MARKERS\", \"VIVI_TEST_TIP\"]\n\n[pools.p.committer]\nname = \"Checkout Fixture\"\nemail = \"checkout@localhost\"\n",
        toml_string(&project.join(".vivi/pump-home")),
        toml_string(bare),
        pool_remote,
        toml_string(&project.parent().unwrap().join("lanes")),
        worktree,
        script.display(),
    );
    for (role, kind) in [
        ("mind", "mind"),
        ("hand", "hand"),
        ("pump-planner", "planner"),
        ("merge", "merge"),
        // Pump readiness requires an active auditor role on a pool.
        ("auditor", "auditor"),
        ("digest", "digest"),
    ] {
        ensure_role(&mut parsed, role, kind, "p", "P2");
    }
    let mut config = toml::to_string(&parsed).unwrap();
    config.push_str(&additions);
    fs::write(config_path, config).unwrap();
}

fn ensure_role(config: &mut toml::Value, name: &str, kind: &str, pool: &str, model: &str) {
    let identities = config
        .get_mut("identities")
        .and_then(toml::Value::as_array_mut)
        .expect("mailspace init omitted identities array");
    let index = identities
        .iter()
        .position(|identity| identity.get("name").and_then(toml::Value::as_str) == Some(name));
    let index = if let Some(index) = index {
        index
    } else {
        identities.push(toml::Value::Table(toml::map::Map::new()));
        identities.len() - 1
    };
    let row = identities[index]
        .as_table_mut()
        .expect("identity row is not a table");
    row.insert("name".to_owned(), toml::Value::String(name.to_owned()));
    row.insert("kind".to_owned(), toml::Value::String(kind.to_owned()));
    row.insert("pool".to_owned(), toml::Value::String(pool.to_owned()));
    row.insert("model".to_owned(), toml::Value::String(model.to_owned()));
}

/// Send a real need, cut and merge its need branch in the side repository,
/// and seed the durable run/slice evidence that matches the Git history.
pub fn seed(rig: &Rig) -> SeededCheckout {
    let mailspace = Mailspace::discover(Some(&rig.project)).unwrap();
    let need = mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "checkout regression".into(),
            body: render(&NeedFront::new(vec!["the markers land".into()]), "").unwrap(),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle;
    let mut port = MailspacePort {
        mailspace,
        pump_identity: String::from("mind"),
    };
    MailWriter::open_record(&mut port, &need, "").unwrap();

    let branch = need_branch::branch(&need);
    let base = git_output(&rig.side_seed, &["rev-parse", SIDE_MAIN]);
    // First slice side branch: its own marker, cut at the base.
    git(
        &rig.side_seed,
        &["checkout", "-q", "-B", "slice-side-a", &base],
    );
    fs::write(rig.side_seed.join(MARKER_A), "first slice marker\n").unwrap();
    git(&rig.side_seed, &["add", MARKER_A]);
    git(&rig.side_seed, &["commit", "-q", "-m", "seed marker a"]);
    let slice_a_tip = git_output(&rig.side_seed, &["rev-parse", "HEAD"]);
    // The need branch starts at the base and takes the first slice through a
    // real no-fast-forward merge, exactly as the production runner merges.
    git(&rig.side_seed, &["checkout", "-q", "-B", &branch, &base]);
    git(
        &rig.side_seed,
        &[
            "merge",
            "-q",
            "--no-ff",
            "--no-edit",
            "-m",
            MERGE_MESSAGE,
            &slice_a_tip,
        ],
    );
    let merge_a = git_output(&rig.side_seed, &["rev-parse", "HEAD"]);
    // Second slice side branch, also cut at the base, integrated the same way.
    git(
        &rig.side_seed,
        &["checkout", "-q", "-B", "slice-side-b", &base],
    );
    fs::write(rig.side_seed.join(MARKER_B), "second slice marker\n").unwrap();
    git(&rig.side_seed, &["add", MARKER_B]);
    git(&rig.side_seed, &["commit", "-q", "-m", "seed marker b"]);
    let slice_b_tip = git_output(&rig.side_seed, &["rev-parse", "HEAD"]);
    git(&rig.side_seed, &["checkout", "-q", &branch]);
    git(
        &rig.side_seed,
        &[
            "merge",
            "-q",
            "--no-ff",
            "--no-edit",
            "-m",
            MERGE_MESSAGE,
            &slice_b_tip,
        ],
    );
    let merge_b = git_output(&rig.side_seed, &["rev-parse", "HEAD"]);
    let need_tip = git_output(&rig.side_seed, &["rev-parse", "HEAD"]);
    git(
        &rig.side_seed,
        &[
            "push",
            "-q",
            path(&rig.side_bare),
            &format!("HEAD:refs/heads/{branch}"),
        ],
    );

    let store = PumpStore::open(&rig.home.join(DB_FILENAME)).unwrap();
    let run_row = RunRow {
        need: need.clone(),
        run: RUN,
        flow: String::from("build"),
        flow_version: FLOW_VERSION_NUMBER,
        kind: String::from("build"),
        entry: String::from("rework1"),
        state: String::from(run_state::STARTING),
    };
    store.insert_run(&run_row).unwrap();
    let sentinel = port
        .send_step(
            "mind",
            "mind",
            &format!("{need} r{RUN} sentinel"),
            &render(&TaskFront::sentinel(&need, RUN), "").unwrap(),
            &[],
        )
        .unwrap();
    store
        .insert_run_step(
            &run_row,
            &run_row.step("sentinel", &sentinel, StepState::Running),
        )
        .unwrap();
    let slice_task = port
        .send_step(
            "mind",
            "mind",
            &format!("{need} r{RUN} slice"),
            &render(&TaskFront::step(&need, RUN, "s1"), "").unwrap(),
            &[],
        )
        .unwrap();
    store
        .insert_run_step(
            &run_row,
            &run_row.step(SLICE_NODE, &slice_task, StepState::Pending),
        )
        .unwrap();
    port.bind(&need, &[sentinel.clone(), slice_task.clone()])
        .unwrap();
    store.set_run_state(&need, RUN, run_state::LIVE).unwrap();
    store
        .upsert_need_branch(&NeedBranchRow {
            need: need.clone(),
            branch: branch.clone(),
            base: Some(base.clone()),
            status: String::from(need_branch::LIVE),
            run: RUN,
            target: String::from(SIDE_TARGET),
            priors: Vec::new(),
        })
        .unwrap();
    SeededCheckout {
        need,
        base,
        merge_a,
        merge_b,
        need_tip,
        slice_a_tip,
        slice_b_tip,
        sentinel_task: sentinel,
        slice_task,
    }
}

/// Whether the file exists on one Git reference (`git cat-file -e`).
pub fn file_on_ref(dir: &Path, reference: &str, file: &str) -> bool {
    git_succeeds(dir, &["cat-file", "-e", &format!("{reference}:{file}")])
}

/// Whether `ancestor` is ancestry of `descendant`.
pub fn is_ancestor(dir: &Path, ancestor: &str, descendant: &str) -> bool {
    git_succeeds(dir, &["merge-base", "--is-ancestor", ancestor, descendant])
}

/// The merge base of two revisions, requiring one to exist.
pub fn merge_base(dir: &Path, one: &str, two: &str) -> String {
    git_output(dir, &["merge-base", one, two])
}

/// The parents of one commit, in order (empty for a root commit).
pub fn parents(dir: &Path, commit: &str) -> Vec<String> {
    let line = git_output(dir, &["rev-list", "--parents", "-n", "1", commit]);
    line.split_whitespace().skip(1).map(str::to_owned).collect()
}

/// The tip of one reference on a bare origin, or `None` when absent.
pub fn remote_tip(dir: &Path, bare: &Path, reference: &str) -> Option<String> {
    let listed = git_output(dir, &["ls-remote", path(bare), reference]);
    listed.split_whitespace().next().map(str::to_owned)
}

/// What a dispatched stand-in recorded for one brief, keyed by job id.
pub struct Evidence {
    pub id: String,
    pub kind: String,
    pub head: String,
    pub branch: String,
    pub marker_a: String,
    pub marker_b: String,
}

/// Read one harness evidence file, or `None` before the job ran.
#[must_use]
pub fn evidence(rig: &Rig, id: &str) -> Option<Evidence> {
    let text = fs::read_to_string(rig.marker_dir.join(format!("{id}.evidence"))).ok()?;
    let get = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(&format!("{key}=")).map(str::to_owned))
    };
    Some(Evidence {
        id: get("id")?,
        kind: get("kind")?,
        head: get("head")?,
        branch: get("branch")?,
        marker_a: get("marker_a")?,
        marker_b: get("marker_b")?,
    })
}

/// The persisted pump state, read-only.
pub fn load_state(rig: &Rig) -> State {
    open_store(rig).load().unwrap()
}

/// Persist `st` through the real store, as the pump would.
pub fn persist_state(rig: &Rig, st: &State) {
    PumpStore::open(&rig.home.join(DB_FILENAME))
        .unwrap()
        .save(st)
        .unwrap();
}

/// The rig's pump store, opened read-only.
pub fn open_store(rig: &Rig) -> PumpStore {
    PumpStore::open_read_only(&rig.home.join(DB_FILENAME))
        .unwrap()
        .expect("pump.db")
}

/// The collected receipt text of one durable job, or `None` before it
/// finished.
#[must_use]
pub fn receipt_of(job: &jobs::PoolJob) -> Option<&str> {
    job.outcome.as_ref().map(|outcome| outcome.receipt.as_str())
}

/// A running `vivi pump run` child. Dropping it releases any stand-in
/// waiting on the marker directory and kills what is left.
pub struct PumpChild {
    child: Child,
    release_path: PathBuf,
}

impl Deref for PumpChild {
    type Target = Child;

    fn deref(&self) -> &Self::Target {
        &self.child
    }
}

impl DerefMut for PumpChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

impl Drop for PumpChild {
    fn drop(&mut self) {
        let _ = fs::write(&self.release_path, "release\n");
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Start the real pump binary over the rig, with the harness environment.
pub fn pump_child(rig: &Rig) -> PumpChild {
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&rig.child_log)
        .unwrap();
    let stderr = stdout.try_clone().unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args([
            "pump",
            "run",
            "--service-child",
            "--project",
            path(&rig.project),
        ])
        .env("VIVI_TEST_MARKERS", &rig.marker_dir)
        .env("VIVI_TEST_TIP", &rig.tip)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .unwrap();
    PumpChild {
        child,
        release_path: rig.marker_dir.join("release-workers"),
    }
}

/// Stop the pump through the real CLI and wait, bounded, for the child.
pub fn stop_pump(rig: &Rig, child: &mut PumpChild) {
    let stopped = vivi(["pump", "stop", "--project", path(&rig.project)]);
    assert_success(&stopped);
    assert!(wait_until(Duration::from_secs(10), || child
        .try_wait()
        .unwrap()
        .is_some()));
}

/// Poll `condition` until it holds or `timeout` has passed; the final result
/// is the condition's, so failures can assert on the last state.
pub fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    condition()
}

/// Run one Git command, requiring success.
pub fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Run one Git command, requiring success, and take its trimmed stdout.
pub fn git_output(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// Run one Git command and report whether it succeeded.
pub fn git_succeeds(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Run the real binary and take its output.
pub fn vivi<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .output()
        .unwrap()
}

/// Require a successful `vivi` invocation.
pub fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn toml_string(path: &Path) -> String {
    format!("{:?}", path.to_str().unwrap())
}
