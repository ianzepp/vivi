use std::fs;
use std::sync::Arc;

use tempfile::tempdir;

use super::{enqueue_merged, refresh_pending};
use crate::mailspace::Mailspace;
use crate::pump::config::PumpConfig;
use crate::pump::host::test_ctx;
use crate::pump::jobs::JobStatus;
use crate::pump::state::{State, Status, Unit};
use crate::routing;

fn project_with_pool(provider: &str, pool: &str) -> (tempfile::TempDir, Mailspace) {
    let dir = tempdir().expect("temporary mailspace");
    fs::create_dir_all(dir.path().join(".vivi")).unwrap();
    fs::write(
        dir.path().join(".vivi/mailspace.toml"),
        format!(
            "name = \"cleanup-test\"\n\n[[identities]]\nname = \"hand\"\npool = \"{pool}\"\n\n[pools.{pool}]\nprovider = \"{provider}\"\nslots = 1\n[pump]\nbranch = \"next\"\n"
        ),
    )
    .unwrap();
    let space = Mailspace::discover(Some(dir.path())).unwrap();
    (dir, space)
}

fn merged_unit() -> Unit {
    Unit {
        need: String::from("n1"),
        target: String::from("default"),
        status: Status::Merged,
        task: Some(String::from("task-1")),
        branch: Some(String::from("unit/u1-a1")),
        pool: Some(String::from("here")),
        ..Unit::default()
    }
}

#[test]
fn merged_unit_enqueues_a_local_snapshot_without_cleaning_inline() {
    let (dir, space) = project_with_pool("local", "here");
    let (mut ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    ctx.mailspace = space.mailspace_id().to_string();
    ctx.routing = Arc::new(routing::from_mailspace(&space.config));
    let mut state = State::default();
    state.units.insert(
        String::from("u2"),
        Unit {
            target: String::from("default"),
            status: Status::Merging,
            ..Unit::default()
        },
    );
    let unit = merged_unit();

    enqueue_merged(&ctx, &mut state, "u1", &unit);

    let job = &state.jobs["reclaim-default-u1"];
    assert_eq!(job.status, JobStatus::Pending);
    assert!(job.local_only);
    assert_eq!(job.pool_candidates, [String::from("here")]);
    assert_eq!(job.payload["request"]["mailspace_id"], space.mailspace_id());
    assert_eq!(job.payload["request"]["pools"], serde_json::json!(["here"]));
    assert_eq!(
        job.payload["request"]["merging_targets"],
        serde_json::json!(["default"])
    );
    assert!(
        job.body
            .contains("--after-merged-request reclaim-request.json")
    );
    assert!(
        host.commands().is_empty(),
        "cleanup must run in the pool job"
    );
}

#[test]
fn reclaim_without_a_local_executor_stays_pending_and_can_recover() {
    let (dir, space) = project_with_pool("fly", "remote");
    let (mut ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    ctx.mailspace = space.mailspace_id().to_string();
    ctx.routing = Arc::new(routing::from_mailspace(&space.config));
    let mut state = State::default();
    let mut unit = merged_unit();
    unit.pool = Some(String::from("remote"));

    enqueue_merged(&ctx, &mut state, "u1", &unit);
    refresh_pending(&ctx, &mut state);

    let job = &state.jobs["reclaim-default-u1"];
    assert_eq!(job.status, JobStatus::Pending);
    assert!(job.local_only);
    assert!(job.pool_candidates.is_empty());
    assert!(job.payload["unavailable_reported"].as_bool().unwrap());
    assert_eq!(
        host.logs()
            .iter()
            .filter(|line| line.contains("EXCEPTION reclaim-unavailable"))
            .count(),
        1
    );

    let config_path = dir.path().join(".vivi/mailspace.toml");
    let config = fs::read_to_string(&config_path)
        .unwrap()
        .replace("provider = \"fly\"", "provider = \"local\"");
    fs::write(config_path, config).unwrap();
    let updated = Mailspace::discover(Some(dir.path())).unwrap();
    ctx.routing = Arc::new(routing::from_mailspace(&updated.config));
    refresh_pending(&ctx, &mut state);

    let job = &state.jobs["reclaim-default-u1"];
    assert_eq!(job.pool_candidates, [String::from("remote")]);
    assert!(!job.payload["unavailable_reported"].as_bool().unwrap());
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("RESOLVED reclaim-unavailable"))
    );
}
