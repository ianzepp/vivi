//! The merge-main and sync handlers: what the jobs carry and when they refuse.

use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::{MergeMainStart, SyncStart};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::JobSpec;
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};
use crate::routing::{RoleRouting, RoutedRole};

const NEED: &str = "n1";
const MAIN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BASE: &str = "0123456789abcdef0123456789abcdef01234567";
const OLD_BASE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    host: Arc<FakeHost>,
    store: PumpStore,
    mail: FakeMail,
}

fn with_merge_route(ctx: Ctx) -> Ctx {
    let mut routing: RoleRouting = (*ctx.routing).clone();
    routing.roles.push(RoutedRole {
        name: String::from("merge"),
        kind: Some(String::from("merge")),
        status: String::from("active"),
        pool: Some(String::from("test-planner-pool")),
        read_only: Some(false),
        model: None,
        models: Vec::new(),
    });
    ctx.with_routing(routing)
}

fn rig(base: Option<&str>) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    host.reply(
        "ls-remote origin main",
        0,
        &format!("{MAIN}\trefs/heads/main\n"),
    );
    if let Some(base) = base {
        store.upsert_need_branch(&NeedBranchRow {
            need: NEED.into(),
            branch: format!("vivi/need/{NEED}"),
            base: Some(base.into()),
            status: "live".into(),
            run: 1,
            target: "default".into(),
            priors: Vec::new(),
        })?;
    }
    Ok(Rig {
        _dir: dir,
        ctx: with_merge_route(ctx),
        host,
        store,
        mail: FakeMail::new("mind"),
    })
}

fn run_row() -> RunRow {
    RunRow {
        need: NEED.into(),
        run: 1,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    }
}

fn step(node: &str) -> StepRow {
    StepRow {
        run: 1,
        node: node.into(),
        task: format!("t-{node}"),
        state: StepState::Pending,
        verdict: None,
    }
}

fn start(rig: &Rig, handler: &dyn StepHandler, node: &str) -> Result<JobSpec, VivariumError> {
    let def = load(FlowKind::Build)?;
    let run = run_row();
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &run,
        def: &def,
    };
    match handler.start(&cx, &step(node))? {
        StartAction::Job(spec) => Ok(*spec),
        other => Err(VivariumError::Message(format!("not a job: {other:?}"))),
    }
}

#[test]
fn merge_main_merges_the_need_tip_into_the_main_tip() {
    let rig = rig(Some(OLD_BASE)).unwrap();
    rig.host.reply(
        "ls-remote origin refs/heads/vivi/need/n1",
        0,
        &format!("{BASE}\trefs/heads/vivi/need/n1\n"),
    );
    let spec = start(&rig, &MergeMainStart, "merge").unwrap();
    assert_eq!(spec.id, format!("merge-main-t-merge-{}-a1", &MAIN[..12]));
    assert_eq!(spec.role, "merge");
    assert_eq!(spec.target.as_deref(), Some("default"));
    assert_eq!(spec.from_ref.as_deref(), Some(MAIN));
    assert_eq!(spec.with, vec![BASE.to_owned()]);
    assert_eq!(spec.publish_contains.as_deref(), Some(BASE));
    assert_eq!(spec.task.as_deref(), Some("t-merge"));
    assert!(rig.host.commands().iter().any(|c| c.contains("ls-remote")));
}

#[test]
fn merge_main_answers_failed_when_the_need_tip_is_the_run_base() {
    let rig = rig(Some(BASE)).unwrap();
    let def = load(FlowKind::Build).unwrap();
    let run = run_row();
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &run,
        def: &def,
    };
    let action = MergeMainStart.start(&cx, &step("merge")).unwrap();
    let StartAction::Answer { word, payload } = action else {
        panic!("not an answer: {action:?}");
    };
    assert_eq!(word, "failed");
    let reason = payload["reason"].as_str().unwrap();
    assert_eq!(reason.matches(BASE).count(), 2, "{reason}");
    assert!(reason.contains("nothing to merge"), "{reason}");
}

#[test]
fn merge_main_refuses_a_short_need_tip() {
    let rig = rig(Some("abc123")).unwrap();
    let err = start(&rig, &MergeMainStart, "merge").unwrap_err();
    assert!(matches!(err, VivariumError::Parse(_)), "{err}");
}

#[test]
fn merge_main_needs_a_need_branch_row() {
    let rig = rig(None).unwrap();
    assert!(start(&rig, &MergeMainStart, "merge").is_err());
    assert!(start(&rig, &SyncStart, "sync1").is_err());
}

#[test]
fn sync_asks_the_merge_role_to_merge_main_into_the_need_branch() {
    let rig = rig(Some(BASE)).unwrap();
    let spec = start(&rig, &SyncStart, "sync1").unwrap();
    assert_eq!(spec.kind, "sync");
    assert_eq!(spec.role, "merge");
    assert_eq!(spec.body.lines().next(), Some("pump-job-kind: sync"));
    assert!(spec.body.contains(MAIN) && spec.body.contains(BASE));
    assert_eq!(spec.target.as_deref(), Some("default"));
    assert_eq!(spec.from_ref.as_deref(), Some(BASE));
    assert_eq!(spec.with, vec![MAIN.to_owned()]);
    assert_eq!(spec.publish_ref.as_deref(), Some("refs/heads/vivi/need/n1"));
    assert_eq!(spec.publish_contains.as_deref(), Some(MAIN));
    let branch = spec.branch.unwrap();
    let tail = branch.strip_prefix("pump/merge-main/").unwrap();
    assert!(!tail.contains('/'), "{branch}");
    assert!(tail.ends_with("-sync"), "{branch}");
}
