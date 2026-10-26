//! Main-rung repair filing through the real binary: one tick that finds a red
//! main rung files the repair need with a `vivi need send` subprocess and
//! opens its pump record through the tick's own mailspace, in the same tick.

use std::collections::BTreeSet;
use std::fs;
use std::process::{Command, Output};
use std::sync::Arc;

use vivi::letter::read::{need_of, record_of};
use vivi::letter::records::RecordKind;
use vivi::mailspace::{Mailspace, PumpState};
use vivi::pump::config::{BoardConfig, FlowConfig, PumpConfig, RungsConfig};
use vivi::pump::flow::tick::tick;
use vivi::pump::host::{Ctx, RealHost};
use vivi::pump::jobs::{JobOutcome, JobStatus, PoolJob, rung};
use vivi::pump::state::State;
use vivi::pump::store::{DB_FILENAME, PumpStore};
use vivi::routing::{RoleRouting, RoutedPool, RoutedRole};

const TARGET: &str = "default";
const TIP: &str = "0123456789abcdef0123456789abcdef01234567";
/// Main-state key `main-started:default`: written so the scheduler stays
/// inside its interval and touches no git during the tick.
const STARTED_KEY: &str = "main-started:default";
/// Main-state key `main-repair-need:default`: the handle red.rs records
/// right after the send parses.
const FILED_KEY: &str = "main-repair-need:default";
/// Main-state key `main-repair-sending:default`: written before the send,
/// cleared once the need's record is open.
const SENDING_KEY: &str = "main-repair-sending:default";
/// Main-state key `main-failing:default`: the JSON failing-id set red.rs
/// stores after filing.
const FAILING_KEY: &str = "main-failing:default";
/// The one failing id the seeded receipt parses to.
const FAILING_ID: &str = "mir_emit_harness::emit::a";
/// The receipt a red unittest run leaves: one failing test, agent exit 101.
const RECEIPT: &str = "     Running unittests src/lib.rs (target/debug/deps/mir_emit_harness-1a2b3c4d5e)\n\
                       test emit::a ... FAILED\n\
                       agent exit 101 in 9s\n";

#[test]
fn main_red_files_and_opens_a_repair_need_in_one_tick_through_real_cli() {
    let project = temp_mailspace();
    // One unrelated valid build need with its open pump record: intake runs
    // before the main rung, enumerates and reads this record (warming the
    // tick's mailspace index), and must not start a run under the cap of 0.
    let build_need = send_need(&project, "mind", "unrelated build work");
    assert_success(&vivi([
        "pump",
        "send",
        "--need",
        &build_need,
        "--project",
        root(&project),
    ]));

    let ctx = real_ctx(&project);
    let needs_before = open_need_handles(&project);

    // The main rung started recently: the scheduler must not enqueue or touch
    // git; the only main-rung work this tick does is apply the seeded job.
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME)).unwrap();
    store
        .set_main_state(STARTED_KEY, &ctx.now().to_string())
        .unwrap();
    drop(store);

    let mut state = State::default();
    let seeded = seed_red_main_rung_job(&ctx, &mut state);

    tick(&ctx, &mut state).unwrap();

    // First-tick proof, read back through the real mailspace the subprocess
    // wrote to.
    let mailspace = Mailspace::discover(Some(project.path())).unwrap();
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME)).unwrap();
    let filed = store
        .main_state(FILED_KEY)
        .unwrap()
        .unwrap_or_else(|| panic!("no {FILED_KEY} main state after the tick"));

    let needs_after = mailspace
        .list_kind(Some("operator"), "needs", "need")
        .unwrap();
    let new_needs: Vec<_> = needs_after
        .iter()
        .filter(|message| !needs_before.contains(&message.handle))
        .collect();
    assert_eq!(
        new_needs.len(),
        1,
        "exactly one new need after the tick; before {needs_before:?}, after {:?}",
        needs_after
            .iter()
            .map(|message| (message.handle.clone(), message.subject.clone()))
            .collect::<Vec<_>>()
    );
    let repair = new_needs[0];
    assert!(
        repair
            .subject
            .starts_with("pump: main rung red on default at "),
        "the new need is the repair need: {}",
        repair.subject
    );
    assert_eq!(repair.handle, filed, "the filed handle is the new need");

    // The stored handle resolves to the repair need's exact message ID.
    let storage = mailspace.storage().unwrap();
    let message_id = storage.resolve_message_token(&filed).unwrap();
    assert_eq!(message_id, repair.message_id, "exact message id");

    // Exactly one open pump record names the repair need, and it is readable.
    let open = mailspace.pump_list(None, Some(PumpState::Open)).unwrap();
    let naming: Vec<_> = open.iter().filter(|record| record.need == filed).collect();
    assert_eq!(
        naming.len(),
        1,
        "exactly one open pump record names {filed}; open: {:?}",
        open.iter()
            .map(|record| (record.handle.clone(), record.need.clone()))
            .collect::<Vec<_>>()
    );
    let record_handle = naming[0].handle.clone();
    let (record, body) = mailspace.pump_show(&record_handle).unwrap();
    assert_eq!(record.need, filed);
    assert!(
        body.contains(TARGET),
        "the open record names the target {TARGET}: {body}"
    );

    // The need letter itself, independent of the filing marker: target and
    // done-when in its front matter, the `repair` mark folded on the root.
    let thread = mailspace
        .thread(&repair.handle, false, usize::MAX, usize::MAX)
        .unwrap();
    let root = thread
        .iter()
        .find(|message| message.handle == repair.handle)
        .unwrap_or_else(|| panic!("no root message for {filed} in its thread"));
    let letter = need_of(&repair.handle, &root.body).unwrap();
    assert_eq!(
        letter.front.target.as_deref(),
        Some(TARGET),
        "the repair need targets {TARGET}"
    );
    assert!(
        letter
            .front
            .done_when
            .iter()
            .any(|clause| clause == "the main rung on default passes"),
        "done_when names the rung passing: {:?}",
        letter.front.done_when
    );
    assert!(
        root.marks.iter().any(|mark| mark == "repair"),
        "the repair need carries the repair mark: {:?}",
        root.marks
    );

    // The record letter: a repair run on this need, at the seeded tip.
    let record_letter = record_of(&record_handle, &body).unwrap();
    assert_eq!(record_letter.front.need, filed);
    assert_eq!(record_letter.front.kind, Some(RecordKind::Repair));
    assert_eq!(record_letter.front.target.as_deref(), Some(TARGET));
    assert_eq!(record_letter.front.tested.as_deref(), Some(TIP));

    // The filing completed inside this tick: the sending marker is cleared.
    assert!(
        store.main_state(SENDING_KEY).unwrap().is_none(),
        "{SENDING_KEY} must be cleared once the record is open"
    );

    // The exact failing-id set of the seeded receipt, compared as a set.
    let failing_json = store
        .main_state(FAILING_KEY)
        .unwrap()
        .unwrap_or_else(|| panic!("no {FAILING_KEY} main state after the tick"));
    let failing: BTreeSet<String> = serde_json::from_str(&failing_json)
        .unwrap_or_else(|e| panic!("bad {FAILING_KEY} state {failing_json:?}: {e}"));
    assert_eq!(failing, BTreeSet::from([FAILING_ID.to_owned()]));

    // The tick applied the seeded job, so a repeat tick cannot re-report it.
    assert!(
        state.jobs[&seeded].applied_at.is_some(),
        "the terminal main rung job must be applied by the filing tick"
    );

    // The filing tick is clean: no main-rung run error was raised.
    assert!(
        main_rung_run_errors(&store).is_empty(),
        "no flow-run-error may name the main-rung unit"
    );

    // The cap of 0 held: the unrelated build need never started a run.
    assert!(
        store.runs_of(&build_need).unwrap().is_empty(),
        "no run may start for {build_need} under max_live_runs = 0"
    );

    // Second tick, same context and state: the job is applied and the record
    // open, so the tick must not file a second need or open a second record.
    tick(&ctx, &mut state).unwrap();

    let mailspace = Mailspace::discover(Some(project.path())).unwrap();
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME)).unwrap();
    assert_eq!(
        store.main_state(FILED_KEY).unwrap().as_deref(),
        Some(filed.as_str()),
        "the filed handle survives the second tick"
    );

    let needs_last = mailspace
        .list_kind(Some("operator"), "needs", "need")
        .unwrap();
    let new_last: Vec<_> = needs_last
        .iter()
        .filter(|message| !needs_before.contains(&message.handle))
        .collect();
    assert_eq!(
        new_last.len(),
        1,
        "still exactly one repair need after the second tick; needs: {:?}",
        needs_last
            .iter()
            .map(|message| message.handle.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(new_last[0].handle, filed);

    let open_last = mailspace.pump_list(None, Some(PumpState::Open)).unwrap();
    let naming_last: Vec<_> = open_last
        .iter()
        .filter(|record| record.need == filed)
        .collect();
    assert_eq!(
        naming_last.len(),
        1,
        "still exactly one open record names {filed}; open: {:?}",
        open_last
            .iter()
            .map(|record| (record.handle.clone(), record.need.clone()))
            .collect::<Vec<_>>()
    );
    assert_eq!(naming_last[0].handle, record_handle);

    // No second record in the need's full pump history either.
    let history = mailspace.pump_history(&filed).unwrap();
    assert_eq!(
        history.len(),
        1,
        "no second record in the history of {filed}: {:?}",
        history
            .iter()
            .map(|record| record.handle.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(history[0].handle, record_handle);

    // The second tick stays as clean as the first.
    assert!(
        store.main_state(SENDING_KEY).unwrap().is_none(),
        "{SENDING_KEY} must stay cleared"
    );
    assert!(
        main_rung_run_errors(&store).is_empty(),
        "no flow-run-error may name the main-rung unit after the second tick"
    );
}

/// The `flow-run-error` exception rows naming the main-rung unit: the
/// identifiers only, never the diagnostic text.
fn main_rung_run_errors(store: &PumpStore) -> Vec<String> {
    store
        .exceptions()
        .unwrap()
        .into_iter()
        .filter(|(_at, kind, unit, _text)| kind == "flow-run-error" && unit == "main-rung")
        .map(|(_at, kind, unit, _text)| format!("{kind}/{unit}"))
        .collect()
}

/// The need handles the operator's needs board holds before the tick.
fn open_need_handles(project: &tempfile::TempDir) -> BTreeSet<String> {
    let mailspace = Mailspace::discover(Some(project.path())).unwrap();
    mailspace
        .list_kind(Some("operator"), "needs", "need")
        .unwrap()
        .into_iter()
        .map(|message| message.handle)
        .collect()
}

/// Seed one terminal, failing main rung job for `TARGET` at `TIP` into
/// `state.jobs`: the finished job red.rs applies. Returns the job id.
fn seed_red_main_rung_job(ctx: &Ctx, state: &mut State) -> String {
    let mut spec = rung::spec(ctx, TARGET, "main", TIP, "main-step", 1)
        .unwrap_or_else(|| panic!("target {TARGET} has a main rung"));
    spec.payload["main_rung"] = serde_json::json!(TARGET);
    let id = spec.id.clone();
    let mut job = PoolJob::from(spec);
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: String::new(),
        receipt: RECEIPT.to_owned(),
        at: 1,
    });
    state.jobs.insert(id.clone(), job);
    id
}

fn real_ctx(project: &tempfile::TempDir) -> Ctx {
    let home = project.path().join(".vivi/pump");
    fs::create_dir_all(home.join("plans")).unwrap();
    let cfg = PumpConfig {
        board: BoardConfig {
            pull: false,
            ..BoardConfig::default()
        },
        rungs: RungsConfig {
            main: Some(String::from("cargo test")),
            ..RungsConfig::default()
        },
        flow: FlowConfig {
            max_live_runs: Some(0),
            ..FlowConfig::default()
        },
        ..PumpConfig::default()
    };
    let mailspace = Mailspace::discover(Some(project.path())).unwrap();
    Ctx::new(
        cfg,
        Arc::new(RealHost::new(&home)),
        project.path().to_path_buf(),
        env!("CARGO_BIN_EXE_vivi").to_string(),
    )
    .with_mailspace(mailspace.mailspace_id().to_string())
    .with_routing(planner_routing())
}

fn planner_routing() -> RoleRouting {
    RoleRouting {
        roles: vec![RoutedRole {
            name: "pump-planner".into(),
            kind: Some("planner".into()),
            status: "active".into(),
            pool: Some("here".into()),
            read_only: None,
            model: None,
            models: Vec::new(),
        }],
        pools: std::collections::BTreeMap::from([(
            "here".into(),
            RoutedPool {
                slots: 1,
                provider: "local".into(),
                fallback: None,
                caps: std::collections::BTreeMap::new(),
                models: Vec::new(),
                has_bands: false,
            },
        )]),
    }
}

fn temp_mailspace() -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    assert_success(&vivi(["mailspace", "init", "--project", root(&project)]));
    // `mailspace init` creates the operator identity; mind is the pump
    // identity (`[pump] reply_from` default) and must exist as a role.
    assert_success(&vivi([
        "role",
        "add",
        "mind",
        "--kind",
        "mind",
        "--project",
        root(&project),
    ]));
    project
}

fn send_need(project: &tempfile::TempDir, to: &str, subject: &str) -> String {
    let out = vivi([
        "need",
        "send",
        "--from",
        "operator",
        "--to",
        to,
        "--subject",
        subject,
        "--done-when",
        "it is planned.",
        "--body",
        "it is planned.",
        "--project",
        root(project),
    ]);
    assert_success(&out);
    stdout(&out)
        .lines()
        .find_map(|line| {
            line.strip_prefix(&format!("created {to} "))
                .map(str::to_owned)
        })
        .unwrap_or_else(|| panic!("no handle in {}", stdout(&out)))
}

fn root(project: &tempfile::TempDir) -> &str {
    project.path().to_str().unwrap()
}

fn vivi<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(output),
        stderr(output)
    );
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
