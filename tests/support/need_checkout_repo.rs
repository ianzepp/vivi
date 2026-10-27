//! Reusable real-Git and durable-history fixtures for checkout regressions.
//!
//! [`scaffold`] builds, in one temporary directory:
//!
//! - a bare origin reached over `file://`, a shared source repository that
//!   authors every commit, and a real project mailspace whose roles
//!   (planner, admitter, slicer, hand, auditor, merge, CTO) are routed to a
//!   worktree pool and a fetch pool;
//! - a need branch built by two successive genuine merges of separate
//!   implementation branches, so `main` lacks both implementation files while
//!   the need branch holds both through merge ancestry;
//! - with [`Options::non_primary_need_target`], a non-primary need target plus
//!   a decoy primary repository that the work never touches.
//!
//! The seeders write durable run/step/task/payload records consistent with
//! those actual merges: completed merged identities, optional no-defect and
//! replaced (obsolete) slice outcomes, paired successful plan admissions,
//! continuation history, and a distinct legacy archive generation. They never
//! preconstruct job specs or worker checkouts: later suites obtain jobs from
//! the shipped handlers.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use vivi::letter::render;
use vivi::letter::work::{NeedFront, TaskFront};
use vivi::mailspace::{Mailspace, SendRequest};
use vivi::pump::envelope::roles::{RoleKind, parse_for};
use vivi::pump::flow::index::{
    FLOW_VERSION_NUMBER, RunRow, StepRow, StepState, run_state,
};
use vivi::pump::flow::mail::MailspacePort;
use vivi::pump::flow::need_branch;
use vivi::pump::store::flow_links::SliceLink;
use vivi::pump::store::step_payload::StepPayloadRow;
use vivi::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};

/// The first implementation file: absent from main, present on the need branch.
pub const ONE_PATH: &str = "src/one.rs";
/// The known content of the first implementation file.
pub const ONE_BODY: &str = "pub fn one() -> u32 {\n    1\n}\n";
/// The second implementation file: absent from main, present on the need branch.
pub const TWO_PATH: &str = "src/two.rs";
/// The known content of the second implementation file.
pub const TWO_BODY: &str = "pub fn two() -> u32 {\n    one() + 1\n}\n";

/// The branch the two implementation merges build: the need branch.
pub fn impl_branch(id: &str) -> String {
    format!("impl/{id}")
}

/// How a seeded slice that never merged closed instead.
pub enum ClosedOutcome {
    /// The hand found nothing to change.
    NoDefect,
    /// A split replaced the slice, so its work became obsolete.
    Replaced,
}

impl ClosedOutcome {
    /// The verdict word the fanout records.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Self::NoDefect => vivi::pump::flow::fanout::VERDICT_NO_DEFECT,
            Self::Replaced => vivi::pump::flow::def::VERDICT_REPLACED,
        }
    }
}

/// Scaffold choices.
pub struct Options {
    /// Slots of each pool. Slots cost about a second each to create.
    pub slots: usize,
    /// Configure a non-primary need target (`alt`) where the need branch
    /// lives, and point the primary `default` target at a decoy repository.
    pub non_primary_need_target: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            slots: 1,
            non_primary_need_target: false,
        }
    }
}

/// The decoy pair of the non-primary variant: the primary target's bare and
/// its workspace source repository.
pub struct Decoy {
    /// The decoy bare origin (`file://` reachable).
    pub bare: PathBuf,
    /// The decoy workspace source repository.
    pub source: PathBuf,
}

/// The assembled fixture.
pub struct Rig {
    pub _dir: tempfile::TempDir,
    /// The project mailspace root.
    pub project: PathBuf,
    /// The pump home (`<project>/.vivi/pump-home`).
    pub home: PathBuf,
    /// The real bare origin of the need target.
    pub bare: PathBuf,
    /// The shared source repository that authored every commit.
    pub seed: PathBuf,
    /// The pump integration clone of the legacy single-target variant.
    pub repo: Option<PathBuf>,
    /// The decoy pair when [`Options::non_primary_need_target`] was set.
    pub decoy: Option<Decoy>,
    /// The standin worker script both pool harnesses run.
    pub worker_script: PathBuf,
    /// The need handle the branch and the durable records belong to.
    pub need: String,
    /// The main tip the need branch was cut from.
    pub base: String,
    /// The need-branch tip after the first genuine merge.
    pub merge_one: String,
    /// The final need-branch tip after the second genuine merge.
    pub merge_two: String,
    /// The target id the need branch lives on.
    pub target: String,
}

/// The roles the fixture routes, as `(name, kind, pool)`.
const ROLES: &[(&str, &str, &str)] = &[
    ("mind", "mind", "p"),
    ("hand", "hand", "p"),
    ("merge", "merge", "p"),
    ("pump-planner", "planner", "f"),
    ("admitter", "admitter", "f"),
    ("slicer", "slicer", "f"),
    ("auditor", "auditor", "f"),
    ("head-cto", "cto", "f"),
];

/// A minimal standin harness: later suites rewrite the script file in place
/// between the scaffold and the dispatch they drive.
const WORKER_SCRIPT: &str = r#"#!/bin/sh
set -eu
brief="$1"
kind=$(sed -n 's/^kind = "\(.*\)"$/\1/p' "$brief" | head -n 1)
case "$kind" in
  hand)
    printf '%s\n' '+++' 'answer = "built"' 'change = []' 'deviations = []' \
      'remaining = []' '+++' 'fixture hand done'
    ;;
  *)
    printf 'fixture standin: %s\n' "$kind"
    ;;
esac
"#;

/// Build the whole repository/state fixture.
///
/// # Panics
/// Panics when any `vivi` or `git` step fails; the fixture is a precondition
/// of every test that builds it.
#[must_use]
pub fn scaffold(options: &Options) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let project = base.join("project");
    fs::create_dir(&project).unwrap();
    assert_success(&vivi([
        "mailspace",
        "init",
        "--project",
        path_str(&project),
    ]));

    let bare = base.join("origin.git");
    let seed = if options.non_primary_need_target {
        project.join("src")
    } else {
        base.join("seed")
    };
    let decoy = options.non_primary_need_target.then(|| {
        let pair = Decoy {
            bare: base.join("decoy.git"),
            source: project.join("decoy"),
        };
        make_repo(&pair.source);
        git(&pair.source, &["clone", "-q", "--bare", ".", path_str(&pair.bare)]);
        git(&pair.source, &["remote", "add", "origin", &file_url(&pair.bare)]);
        pair
    });

    make_repo(&seed);
    git(&seed, &["clone", "-q", "--bare", ".", path_str(&bare)]);
    git(&seed, &["remote", "add", "origin", &file_url(&bare)]);

    let home = project.join(".vivi/pump-home");
    fs::create_dir_all(&home).unwrap();
    let repo = (!options.non_primary_need_target).then(|| {
        git(&seed, &["push", "-q", &file_url(&bare), "main:next"]);
        let repo = home.join("repo");
        git(&base, &["clone", "-q", &file_url(&bare), path_str(&repo)]);
        git(&repo, &["checkout", "-q", "-B", "next", "origin/next"]);
        repo
    });
    if options.non_primary_need_target {
        let targets = home.join("targets");
        fs::create_dir_all(&targets).unwrap();
        let default_repo = targets.join("default");
        let alt_repo = targets.join("alt");
        git(
            &base,
            &["clone", "-q", &file_url(&decoy.as_ref().unwrap().bare), path_str(&default_repo)],
        );
        git(&base, &["clone", "-q", &file_url(&bare), path_str(&alt_repo)]);
    }

    write_config(&project, &base, &home, options, &bare, &repo, &decoy);
    let script = base.join("worker.sh");
    fs::write(&script, WORKER_SCRIPT).unwrap();
    for pool in ["p", "f"] {
        assert_success(&vivi([
            "pool",
            "create",
            "--pool",
            pool,
            "--project",
            path_str(&project),
        ]));
    }

    let mailspace = Mailspace::discover(Some(&project)).unwrap();
    let need = send_need(&mailspace);
    let (base_tip, merge_one, merge_two) = build_need_branch(&seed, &bare, &need);

    Rig {
        _dir: dir,
        project,
        home,
        bare,
        seed,
        repo,
        decoy,
        worker_script: script,
        need,
        base: base_tip,
        merge_one,
        merge_two,
        target: if options.non_primary_need_target {
            String::from("alt")
        } else {
            String::from("default")
        },
    }
}

/// A plain Git repository with one README commit on `main`.
fn make_repo(dir: &Path) {
    fs::create_dir(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.name", "Checkout Fixture"]);
    git(dir, &["config", "user.email", "fixture@localhost"]);
    fs::write(dir.join("README.md"), "checkout fixture\n").unwrap();
    git(dir, &["add", "README.md"]);
    git(dir, &["commit", "-q", "-m", "fixture"]);
}

/// Build the need branch of `need` in `seed`: two genuine merges of separate
/// implementation branches off main, then publish it to the bare origin.
/// Returns `(base, merge_one, merge_two)`.
fn build_need_branch(seed: &Path, bare: &Path, need: &str) -> (String, String, String) {
    let base = git_output(seed, &["rev-parse", "main"]);
    git(seed, &["checkout", "-q", "-b", "impl/one"]);
    write_impl_file(seed, ONE_PATH, ONE_BODY);
    git(seed, &["add", ONE_PATH]);
    git(seed, &["commit", "-q", "-m", "implementation one"]);
    git(seed, &["checkout", "-q", "-b", "impl/two", &base]);
    write_impl_file(seed, TWO_PATH, TWO_BODY);
    git(seed, &["add", TWO_PATH]);
    git(seed, &["commit", "-q", "-m", "implementation two"]);

    let branch = need_branch::branch(need);
    git(seed, &["checkout", "-q", "-B", &branch, &base]);
    git(
        seed,
        &["merge", "-q", "--no-ff", "--no-edit", "-m", "merge implementation one", "impl/one"],
    );
    let merge_one = git_output(seed, &["rev-parse", "HEAD"]);
    git(
        seed,
        &["merge", "-q", "--no-ff", "--no-edit", "-m", "merge implementation two", "impl/two"],
    );
    let merge_two = git_output(seed, &["rev-parse", "HEAD"]);
    git(
        seed,
        &[
            "push",
            "-q",
            &file_url(bare),
            &format!("refs/heads/{branch}:refs/heads/{branch}"),
        ],
    );
    git(seed, &["checkout", "-q", "main"]);
    (base, merge_one, merge_two)
}

/// Write one implementation file, creating its directory.
fn write_impl_file(seed: &Path, path: &str, body: &str) {
    let full = seed.join(path);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(full, body).unwrap();
}

/// Write the mailspace config: routed roles, the pump tables and both pool
/// shapes (a worktree pool `p` and a fetch pool `f`).
#[allow(clippy::too_many_arguments)]
fn write_config(
    project: &Path,
    base: &Path,
    home: &Path,
    options: &Options,
    bare: &Path,
    repo: &Option<PathBuf>,
    decoy: &Option<Decoy>,
) {
    let config_path = project.join(".vivi/mailspace.toml");
    let old = fs::read_to_string(&config_path).unwrap();
    let mut parsed: toml::Value = toml::from_str(&old).unwrap();
    // `mailspace init` writes a starter [pump]; this fixture supplies its own.
    parsed.as_table_mut().unwrap().remove("pump");
    for (name, kind, pool) in ROLES {
        ensure_role(&mut parsed, name, kind, pool);
    }
    let mut config = toml::to_string(&parsed).unwrap();
    config.push_str(&pump_table(base, home, options, bare, repo, decoy));
    fs::write(config_path, config).unwrap();
}

/// The `[pump]` table and both pools: the legacy single-target shape, or the
/// two-target shape with the decoy primary and the non-primary need target.
fn pump_table(
    base: &Path,
    home: &Path,
    options: &Options,
    bare: &Path,
    repo: &Option<PathBuf>,
    decoy: &Option<Decoy>,
) -> String {
    let mut table = format!(
        "\n[pump]\nhome = {}\ntick = 1\ncommitter_name = \"Checkout Fixture\"\n\
         committer_email = \"fixture@localhost\"\nfrom = \"mind\"\nto = \"hand\"\n\
         reply_from = \"mind\"\n\n[pump.board]\nfrom = []\npull = false\n",
        toml_string(home),
    );
    if options.non_primary_need_target {
        let decoy = decoy.as_ref().unwrap();
        table.push_str(&format!(
            "\n[pump.targets.default]\npath = \"decoy\"\nremote = \"origin\"\n\
             remote_url = {}\nmain = \"main\"\n\n[pump.targets.alt]\npath = \"src\"\n\
             remote = \"origin\"\nremote_url = {}\nmain = \"main\"\n",
            toml_string_quoted(&file_url(&decoy.bare)),
            toml_string_quoted(&file_url(bare)),
        ));
    } else {
        let repo = repo.as_ref().unwrap();
        table.push_str(&format!(
            "repo = {}\nremote = \"origin\"\nremote_url = {}\nbranch = \"next\"\n\
             main = \"main\"\npromote = false\n",
            toml_string(repo),
            toml_string_quoted(&file_url(bare)),
        ));
    }
    let worktree_of = repo
        .clone()
        .unwrap_or_else(|| base.join("project").join("src"));
    table.push_str(&pool_table("p", options.slots, base.join("lanes"), Some(&worktree_of)));
    table.push_str(&pool_table("f", options.slots, base.join("lanes-f"), None));
    table
}

/// One pool: worktree-shaped when `worktree_of` is set, fetch-shaped when not.
fn pool_table(name: &str, slots: usize, root: PathBuf, worktree_of: Option<&Path>) -> String {
    let mut table = format!("\n[pools.{name}]\nprovider = \"local\"\nslots = {slots}\n");
    if let Some(repo) = worktree_of {
        table.push_str(&format!(
            "\n[pools.{name}.local]\nroot = {}\nworktree_of = {}\n",
            toml_string(&root),
            toml_string(repo),
        ));
    } else {
        table.push_str(&format!(
            "\n[pools.{name}.local]\nroot = {}\n",
            toml_string(&root),
        ));
    }
    table.push_str(&format!(
        "\n[pools.{name}.bands]\nP2 = [{{ model = \"standin\", reasoning = \"low\" }}]\n\
         \n[pools.{name}.harness]\nname = \"standin\"\nmodels = [\"standin\"]\n\
         \n[pools.{name}.committer]\nname = \"Checkout Fixture\"\nemail = \"fixture@localhost\"\n",
    ));
    table
}

/// The workspace source repository of the non-primary variant.
fn project_join(base: &Path, tail: &str) -> PathBuf {
    base.join("project").join(tail)
}

/// Send the fixture need over `mailspace` and open its pump record. The
/// record stays open: a restart re-ingests it, which is the continuation the
/// seeders build on.
fn send_need(mailspace: &Mailspace) -> String {
    let need = mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "checkout fixture".into(),
            body: render(
                &NeedFront::new(vec![String::from(
                    "both implementation files exist on the need branch",
                )]),
                "",
            )
            .unwrap(),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle;
    MailspacePort {
        mailspace: Mailspace::discover(Some(&mailspace.root)).unwrap(),
        pump_identity: String::from("mind"),
    }
    .open_pump_record(&need, "")
    .unwrap();
    need
}

/// The durable records of one seeded build run.
pub struct SeededRun {
    /// The task handle of the pending `merge` step.
    pub merge_task: String,
    /// The task handle of the slicer step (the open fanout).
    pub slicer_task: String,
}

/// Seed a live build run whose fanout already completed: sentinel, two paired
/// successful plan admissions, the slicer step, both implementation slices
/// merged through the genuine merges, and the `merge` step still pending.
/// Writes the live need-branch row for `run` over the recorded base.
pub fn seed_build_run(rig: &Rig, run: u32) -> SeededRun {
    let store = open_store(rig);
    let mut port = open_port(rig);
    let run_row = insert_run(&store, rig, run, run_state::STARTING);

    let sentinel = send_flow_step(&mut port, rig, run, None);
    insert_step(&store, &run_row, "sentinel", &sentinel, StepState::Done, None);
    seed_plan_admission(rig, &store, &mut port, run, "plan", "admit1");
    seed_plan_admission(rig, &store, &mut port, run, "plan2", "admit2");
    let slicer_task = seed_slicer_step(rig, &store, &mut port, run);
    let one = seed_merged_slice(
        rig, &store, &mut port, run, &slicer_task, "one", "implementation one",
        "add the first implementation file", ONE_PATH, &rig.merge_one,
    );
    let two = seed_merged_slice(
        rig, &store, &mut port, run, &slicer_task, "two", "implementation two",
        "add the second implementation file", TWO_PATH, &rig.merge_two,
    );
    let merge_task = send_flow_step(&mut port, rig, run, Some("merge"));
    insert_step(&store, &run_row, "merge", &merge_task, StepState::Pending, None);

    port.bind(
        &rig.need,
        &[sentinel, slicer_task.clone(), one, two, merge_task.clone()],
    )
    .unwrap();
    store
        .upsert_need_branch(&NeedBranchRow::for_run(
            &run_row,
            need_branch::branch(&rig.need),
            Some(rig.base.clone()),
            need_branch::LIVE,
            &rig.target,
            Vec::new(),
        ))
        .unwrap();
    store.set_run_state(&rig.need, run, run_state::LIVE).unwrap();
    SeededRun {
        merge_task,
        slicer_task,
    }
}

/// Seed the withdrawn first attempt a live run continues: sentinel and the
/// first plan admission pair are done, nothing sliced. The run keeps no
/// branch row: the live run's row wins.
pub fn seed_continuation(rig: &Rig) {
    let store = open_store(rig);
    let mut port = open_port(rig);
    let run_row = insert_run(&store, rig, 1, run_state::WITHDRAWN);
    let sentinel = send_flow_step(&mut port, rig, 1, None);
    insert_step(&store, &run_row, "sentinel", &sentinel, StepState::Done, None);
    seed_plan_admission(rig, &store, &mut port, 1, "plan", "admit1");
    port.bind(&rig.need, &[sentinel]).unwrap();
}

/// Seed one planner/admitter pair: both steps done with the successful words
/// (`planned`, `admitted`) and their envelope payloads recorded.
pub fn seed_plan_admission(
    rig: &Rig,
    store: &PumpStore,
    port: &mut MailspacePort,
    run: u32,
    plan_node: &str,
    admit_node: &str,
) {
    let run_row = store.run_row(&rig.need, run).unwrap();
    let plan = send_flow_step(port, rig, run, Some(plan_node));
    insert_step(store, &run_row, plan_node, &plan, StepState::Done, Some("planned"));
    record_payload(store, &run_row, plan_node, &plan, RoleKind::Planner, PLANNER_LETTER);
    let admit = send_flow_step(port, rig, run, Some(admit_node));
    insert_step(store, &run_row, admit_node, &admit, StepState::Done, Some("admitted"));
    record_payload(store, &run_row, admit_node, &admit, RoleKind::Admitter, ADMITTER_LETTER);
}

/// Seed the slicer step of `run`: done `sliced`, its envelope payload
/// recorded, and the fanout left open on its task.
pub fn seed_slicer_step(
    rig: &Rig,
    store: &PumpStore,
    port: &mut MailspacePort,
    run: u32,
) -> String {
    let run_row = store.run_row(&rig.need, run).unwrap();
    let task = send_flow_step(port, rig, run, Some("slice"));
    insert_step(store, &run_row, "slice", &task, StepState::Done, Some("sliced"));
    record_payload(
        store,
        &run_row,
        "slice",
        &task,
        RoleKind::Slicer,
        &slicer_letter(&rig.target),
    );
    store.set_open_fanout(&rig.need, run, &task).unwrap();
    task
}

/// Seed one merged implementation slice: the task letter, the done `merged`
/// step row closed over the commit its merge landed, the write scope and the
/// fanout link. Returns the task handle.
pub fn seed_merged_slice(
    rig: &Rig,
    store: &PumpStore,
    port: &mut MailspacePort,
    run: u32,
    slicer_task: &str,
    id: &str,
    title: &str,
    brief: &str,
    path: &str,
    commit: &str,
) -> String {
    let task = send_slice_task(rig, port, run, id, title, brief, path);
    let run_row = store.run_row(&rig.need, run).unwrap();
    insert_step(
        store,
        &run_row,
        &vivi::pump::flow::fanout::slice_node(id),
        &task,
        StepState::Done,
        Some(vivi::pump::flow::fanout::VERDICT_MERGED),
    );
    store.insert_slice_scope(&task, run, &[String::from(path)]).unwrap();
    store
        .link_slice(&SliceLink::for_run(
            &run_row,
            &task,
            Some(String::from(slicer_task)),
            Vec::new(),
        ))
        .unwrap();
    port.close_task(
        &task,
        vivi::pump::flow::fanout::VERDICT_MERGED,
        Some(&need_branch::branch(&rig.need)),
        Some(commit),
    )
    .unwrap();
    task
}

/// Seed a slice that closed without merging: [`ClosedOutcome::NoDefect`] or
/// [`ClosedOutcome::Replaced`] (the obsolete outcome). Its scope and link are
/// still recorded, so the durable set carries the whole fanout.
pub fn seed_unmerged_slice(
    rig: &Rig,
    store: &PumpStore,
    port: &mut MailspacePort,
    run: u32,
    slicer_task: &str,
    id: &str,
    title: &str,
    brief: &str,
    path: &str,
    outcome: &ClosedOutcome,
) -> String {
    let task = send_slice_task(rig, port, run, id, title, brief, path);
    let run_row = store.run_row(&rig.need, run).unwrap();
    insert_step(store, &run_row, &vivi::pump::flow::fanout::slice_node(id), &task, StepState::Done, Some(outcome.word()));
    store.insert_slice_scope(&task, run, &[String::from(path)]).unwrap();
    store
        .link_slice(&SliceLink::for_run(
            &run_row,
            &task,
            Some(String::from(slicer_task)),
            Vec::new(),
        ))
        .unwrap();
    port.close_task(&task, outcome.word(), None, None).unwrap();
    task
}

/// Send one slice task letter with its target and write scope.
fn send_slice_task(
    rig: &Rig,
    port: &MailspacePort,
    run: u32,
    id: &str,
    title: &str,
    brief: &str,
    path: &str,
) -> String {
    let front = TaskFront {
        target: Some(rig.target.clone()),
        ..TaskFront::step(&rig.need, run, &vivi::pump::flow::fanout::slice_node(id))
    };
    let body = render(&front, &format!("{title}\n\n{brief}\n")).unwrap();
    port.send_step(
        "mind",
        "mind",
        &format!("{title} ({} r{run})", rig.need),
        &body,
        &[],
    )
    .unwrap()
}

/// Insert one run row with the fixture's build identity.
fn insert_run(store: &PumpStore, rig: &Rig, run: u32, state: &str) -> RunRow {
    let run_row = RunRow {
        need: rig.need.clone(),
        run,
        flow: String::from("build"),
        flow_version: FLOW_VERSION_NUMBER,
        kind: String::from("build"),
        entry: String::from("rework1"),
        state: String::from(state),
    };
    store.insert_run(&run_row).unwrap();
    run_row
}

/// Insert one step row of `run_row`.
fn insert_step(
    store: &PumpStore,
    run_row: &RunRow,
    node: &str,
    task: &str,
    state: StepState,
    verdict: Option<&str>,
) {
    let mut row = run_row.step(node, task, state);
    row.verdict = verdict.map(str::to_string);
    store.insert_run_step(run_row, &row).unwrap();
}

/// Send one flow step task of `run` (the sentinel when `node` is `None`).
fn send_flow_step(port: &MailspacePort, rig: &Rig, run: u32, node: Option<&str>) -> String {
    let front = match node {
        Some(node) => TaskFront::step(&rig.need, run, node),
        None => TaskFront::sentinel(&rig.need, run),
    };
    let subject = match node {
        Some(node) => format!("{} r{run} {node}", rig.need),
        None => format!("{} r{run} sentinel", rig.need),
    };
    port.send_step("mind", "mind", &subject, &render(&front, "").unwrap(), &[])
        .unwrap()
}

/// Record one step payload in the production shape: the parsed role envelope
/// front and body, under the step's class.
fn record_payload(
    store: &PumpStore,
    run_row: &RunRow,
    node: &str,
    task: &str,
    kind: RoleKind,
    letter: &str,
) {
    let parsed = parse_for(kind, letter).expect("a valid role envelope");
    store
        .record_step_payload(&StepPayloadRow {
            need: run_row.need.clone(),
            run: run_row.run,
            node: String::from(node),
            class: kind.name().to_string(),
            task: String::from(task),
            payload: serde_json::json!({ "front": parsed.front, "body": parsed.body }),
        })
        .unwrap();
}

/// Open the fixture's pump store.
#[must_use]
pub fn open_store(rig: &Rig) -> PumpStore {
    PumpStore::open(&rig.home.join(DB_FILENAME)).unwrap()
}

/// Open a read-only view of any pump database (the live one or an archive).
pub fn open_store_at(path: &Path) -> PumpStore {
    PumpStore::open_read_only(path)
        .unwrap()
        .expect("pump.db")
}

/// Open the fixture's mailspace port over the pump identity.
#[must_use]
pub fn open_port(rig: &Rig) -> MailspacePort {
    MailspacePort {
        mailspace: Mailspace::discover(Some(&rig.project)).unwrap(),
        pump_identity: String::from("mind"),
    }
}

/// Archive the current pump home with the real `vivi pump reset` and return
/// the archive directory: a distinct legacy generation whose pump.db keeps
/// its own durable records beside the fresh home.
pub fn archive_first_generation(rig: &Rig) -> PathBuf {
    let out = vivi(["pump", "reset", "--project", path_str(&rig.project)]);
    assert_success(&out);
    PathBuf::from(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .expect("reset prints the archive path")
            .trim(),
    )
}

/// The planner receipt the plan steps carry.
const PLANNER_LETTER: &str = "+++\nanswer = \"planned\"\n+++\nPlan: two independent \
     implementation slices off main; each adds one file.\n";

/// The admitter receipt the admit steps carry.
const ADMITTER_LETTER: &str = "+++\nanswer = \"admitted\"\n+++\nThe plan is small, \
     scoped and testable; admitted for slicing.\n";

/// The slicer receipt naming both fixture slices with their target and scope.
fn slicer_letter(target: &str) -> String {
    format!(
        "+++\nanswer = \"sliced\"\n\n[[slice]]\nid = \"one\"\ntitle = \"implementation one\"\n\
         brief = \"add the first implementation file\"\ntarget = \"{target}\"\n\
         write_scope = [\"{one}\"]\n\n[[slice]]\nid = \"two\"\ntitle = \"implementation two\"\n\
         brief = \"add the second implementation file\"\ntarget = \"{target}\"\n\
         write_scope = [\"{two}\"]\n+++\nTwo independent implementation slices.\n",
        one = ONE_PATH,
        two = TWO_PATH,
    )
}

/// Insert one identity row (or update it in place) with its routed pool.
fn ensure_role(config: &mut toml::Value, name: &str, kind: &str, pool: &str) {
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
    row.insert("model".to_owned(), toml::Value::String(String::from("P2")));
}

/// A `file://` URL for a local Git path.
#[must_use]
pub fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn git(dir: &Path, args: &[&str]) {
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

/// Run `git` and return its trimmed stdout.
#[must_use]
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

/// Run the `vivi` binary.
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

/// Assert one `vivi` invocation succeeded, naming its output otherwise.
pub fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn path_str(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn toml_string(path: &Path) -> String {
    format!("{:?}", path.to_str().unwrap())
}

fn toml_string_quoted(text: &str) -> String {
    format!("{text:?}")
}
