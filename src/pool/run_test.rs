use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use super::*;
use crate::pool::PoolHarness;
use crate::pool::fake_provider::{Fake, pid};
use crate::pool::pools::PoolSet;
use crate::pool::slot::{META_BASE, META_STATE, PoolId, STATE_READY};

fn config(slots: u32) -> PoolConfig {
    toml::from_str(&format!(
        r#"
repo = "o/r"
slots = {slots}
image = "debian:12"
build_dir = "target"
[fly]
app = "a"
[github]
app_id = 1
private_key = "k.pem"
"#
    ))
    .unwrap()
}

fn fast() -> CreateOptions {
    CreateOptions {
        slots: None,
        spacing: Duration::ZERO,
        poll: Duration::ZERO,
        timeout: Duration::from_mins(1),
        confirm_timeout: Duration::from_millis(50),
        heartbeat: Duration::from_hours(1),
    }
}

#[allow(clippy::unnecessary_wraps)]
fn inputs() -> Result<(Secret, String), VivariumError> {
    Ok((Secret::new("tok-secret".into()), "c".repeat(40)))
}

fn existing(name: &str, id: &str, state: &str, pool: &str, index: u32) -> MachineRecord {
    crate::pool::fake_provider::slot(id, name, state, pool, index)
}

#[test]
fn create_makes_missing_slots_and_waits_for_stopped() {
    let fake = Fake::default();
    fake.machines
        .borrow_mut()
        .push(existing("p-slot-2", "old2", "stopped", "p", 2));
    fake.machines.borrow_mut().push(MachineRecord {
        metadata: BTreeMap::new(),
        ..existing("slot-quad-1", "foreign", "stopped", "p", 1)
    });
    let report = create_slots(
        &fake,
        &config(3),
        &pid("p"),
        &fast(),
        &Progress::silent(),
        inputs,
    )
    .unwrap();
    assert_eq!(report.existing.len(), 1);
    let names: Vec<&str> = report.created.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(names, ["ms-p-slot-1", "ms-p-slot-3"]);
    assert!(report.created.iter().all(SlotOutcome::seeded));
    let specs = fake.created.borrow();
    assert_eq!(specs[0].metadata[META_BASE], "c".repeat(40));
    assert_eq!(specs[0].once_env["GH_TOKEN"], "tok-secret");
    assert!(
        !specs[0].env.contains_key("GH_TOKEN"),
        "the secret travels apart"
    );
    assert_eq!(specs[0].tmpfs, "16g");
    assert_eq!(specs[0].build_dir, "target");
    assert!(
        !specs[0].env.contains_key("BUILD_MOUNTED"),
        "this host does not mount"
    );
    assert_eq!(specs[0].command, ["bash", "/work/boot.sh"]);
    assert!(fake.destroyed.borrow().is_empty());
}

#[test]
fn create_with_full_pool_does_nothing_and_skips_token_minting() {
    let fake = Fake::default();
    fake.machines
        .borrow_mut()
        .push(existing("p-slot-1", "o1", "stopped", "p", 1));
    let report = create_slots(
        &fake,
        &config(1),
        &pid("p"),
        &fast(),
        &Progress::silent(),
        || Err(VivariumError::Other("must not mint".into())),
    )
    .unwrap();
    assert!(report.created.is_empty());
    assert!(fake.created.borrow().is_empty());
}

#[test]
fn create_refuses_more_than_the_configured_maximum() {
    let fake = Fake::default();
    let options = CreateOptions {
        slots: Some(4),
        ..fast()
    };
    let err = create_slots(
        &fake,
        &config(3),
        &pid("p"),
        &options,
        &Progress::silent(),
        inputs,
    )
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("exceeds the configured maximum of 3")
    );
    assert!(fake.created.borrow().is_empty());
}

#[test]
fn create_to_a_smaller_target_creates_fewer() {
    let fake = Fake::default();
    let options = CreateOptions {
        slots: Some(1),
        ..fast()
    };
    let report = create_slots(
        &fake,
        &config(4),
        &pid("p"),
        &options,
        &Progress::silent(),
        inputs,
    )
    .unwrap();
    assert_eq!(report.created.len(), 1);
}

#[test]
fn create_reports_timeouts_without_destroying() {
    let fake = Fake {
        never_stop: true,
        ..Fake::default()
    };
    let options = CreateOptions {
        timeout: Duration::ZERO,
        ..fast()
    };
    let report = create_slots(
        &fake,
        &config(1),
        &pid("p"),
        &options,
        &Progress::silent(),
        inputs,
    )
    .unwrap();
    assert!(report.created[0].timed_out);
    assert!(!report.created[0].seeded());
    assert!(fake.destroyed.borrow().is_empty());
}

#[test]
fn nonzero_exit_is_not_a_clean_seed() {
    let outcome = SlotOutcome {
        pool: "p".into(),
        name: "n".into(),
        id: "i".into(),
        state: "stopped".into(),
        exit_code: Some(3),
        seconds: 1,
        timed_out: false,
        error: None,
    };
    assert!(!outcome.seeded());
}

#[test]
fn destroy_touches_only_pool_slots() {
    let fake = Fake::default();
    {
        let mut machines = fake.machines.borrow_mut();
        machines.push(existing("p-slot-1", "m1", "stopped", "p", 1));
        machines.push(existing("p-slot-2", "m2", "started", "p", 2));
        machines.push(MachineRecord {
            metadata: BTreeMap::new(),
            ..existing("slot-quad-1", "q1", "stopped", "p", 1)
        });
    }
    let one = destroy_slots(&fake, &pid("p"), &DestroyTarget::Slot("p-slot-2".into())).unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!(*fake.destroyed.borrow(), ["m2"]);
    let err =
        destroy_slots(&fake, &pid("p"), &DestroyTarget::Slot("slot-quad-1".into())).unwrap_err();
    assert!(err.to_string().contains("no slot"));
    let all = destroy_slots(&fake, &pid("p"), &DestroyTarget::All).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(*fake.destroyed.borrow(), ["m2", "m1"]);
    assert!(fake.machines.borrow().iter().any(|m| m.id == "q1"));
}

#[test]
fn status_lists_pool_slots_from_the_provider() {
    let fake = Fake::default();
    fake.machines
        .borrow_mut()
        .push(existing("p-slot-1", "m1", "stopped", "p", 1));
    let slots = pool_status(&fake, &pid("p")).unwrap();
    assert_eq!(slots[0].state, "free");
}

#[test]
fn a_clean_seed_clears_the_fetch_token_then_marks_the_slot_ready() {
    let fake = Fake::default();
    let report = create_slots(
        &fake,
        &config(1),
        &pid("p"),
        &fast(),
        &Progress::silent(),
        inputs,
    )
    .unwrap();
    assert!(report.created[0].seeded());
    let cleared = fake.env_cleared.borrow();
    assert_eq!(cleared.len(), 1);
    assert_eq!(cleared[0].1, ["GH_TOKEN"]);
    assert_eq!(
        fake.meta("id-ms-p-slot-1", META_STATE).as_deref(),
        Some(STATE_READY)
    );
    assert_eq!(
        fake.calls_to("start"),
        0,
        "clearing the token never starts the slot"
    );
}

#[test]
fn a_new_slot_starts_unmarked_so_status_tells_seeding_from_busy() {
    let fake = Fake {
        never_stop: true,
        ..Fake::default()
    };
    let options = CreateOptions {
        timeout: Duration::ZERO,
        ..fast()
    };
    create_slots(
        &fake,
        &config(1),
        &pid("p"),
        &options,
        &Progress::silent(),
        inputs,
    )
    .unwrap();
    let slots = pool_status(&fake, &pid("p")).unwrap();
    assert_eq!(slots[0].state, "seeding");
    assert!(
        fake.env_cleared.borrow().is_empty(),
        "an unfinished seed keeps its token"
    );
}

#[test]
fn an_interrupted_create_is_finished_by_the_next_one() {
    let fake = Fake::default();
    let mut m = existing("p-slot-1", "m1", "stopped", "p", 1);
    m.metadata.insert(META_STATE.into(), "seeding".into());
    m.exit_code = Some(0);
    fake.add(m);
    let mut failed = existing("p-slot-2", "m2", "stopped", "p", 2);
    failed.metadata.insert(META_STATE.into(), "seeding".into());
    failed.exit_code = Some(3);
    fake.add(failed);
    let report = create_slots(
        &fake,
        &config(2),
        &pid("p"),
        &fast(),
        &Progress::silent(),
        inputs,
    )
    .unwrap();
    assert!(report.created.is_empty());
    assert_eq!(fake.meta("m1", META_STATE).as_deref(), Some(STATE_READY));
    assert_eq!(fake.meta("m2", META_STATE).as_deref(), Some("seeding"));
    assert_eq!(fake.env_cleared.borrow().len(), 1);
}

fn with_env(names: &[&str]) -> PoolConfig {
    let mut config = config(1);
    config.harness = Some(PoolHarness {
        name: "h".into(),
        run: "h".into(),
        models: vec![],
        env: names.iter().map(ToString::to_string).collect(),
        rules: None,
        unknown: BTreeMap::new(),
    });
    config
}

#[test]
fn prepare_home_creates_a_missing_home_once() {
    let fake = Fake::default();
    let config = with_env(&[]);
    assert!(prepare_home(&fake, &config).unwrap().created.is_some());
    assert_eq!(*fake.apps_created.borrow(), ["fake"]);
    assert!(prepare_home(&fake, &config).unwrap().created.is_none());
    assert_eq!(fake.apps_created.borrow().len(), 1);
}

#[test]
fn missing_env_is_the_harness_names_the_host_lacks() {
    let fake = Fake::default();
    fake.secrets.borrow_mut().push("HAVE_KEY".into());
    let config = with_env(&["HAVE_KEY", "NEED_KEY", "OTHER_KEY"]);
    let missing = missing_env(&fake, &config).unwrap();
    assert_eq!(missing, ["NEED_KEY", "OTHER_KEY"]);
    assert_eq!(missing_env_at_dispatch(&fake, &config).unwrap(), missing);
    assert!(missing_env(&fake, &with_env(&[])).unwrap().is_empty());
    assert!(missing_env(&fake, &config_no_harness()).unwrap().is_empty());
}

fn config_no_harness() -> PoolConfig {
    config(1)
}

#[test]
fn the_env_warning_comes_from_the_host_and_only_when_something_is_missing() {
    let fake = Fake::default();
    assert!(env_warning(&fake, &[]).is_none());
    let text = env_warning(&fake, &["A_KEY".to_string(), "B_KEY".to_string()]).unwrap();
    assert_eq!(text, "fake hint: set A_KEY, B_KEY");
}

#[test]
fn create_does_not_return_until_a_fresh_listing_shows_the_slot_free() {
    let fake = Fake::default();
    fake.stale_lists.set(4);
    let options = CreateOptions {
        confirm_timeout: Duration::from_secs(5),
        ..fast()
    };
    let report = create_slots(
        &fake,
        &config(1),
        &pid("p"),
        &options,
        &Progress::silent(),
        inputs,
    )
    .unwrap();
    assert!(report.created[0].seeded(), "{:?}", report.created[0]);
    let listed = fake.list().unwrap();
    assert_eq!(slot::slot_state(&listed[0]), "free");
}

#[test]
fn a_slot_that_never_reads_free_is_reported_not_called_seeded() {
    let fake = Fake::default();
    fake.stale_lists.set(10_000);
    let options = CreateOptions {
        confirm_timeout: Duration::ZERO,
        ..fast()
    };
    let report = create_slots(
        &fake,
        &config(1),
        &pid("p"),
        &options,
        &Progress::silent(),
        inputs,
    )
    .unwrap();
    assert!(!report.created[0].seeded());
    assert!(
        report.created[0]
            .error
            .as_deref()
            .unwrap()
            .contains("still reads")
    );
}

#[test]
fn status_shows_how_long_a_busy_slot_has_run_and_its_budget() {
    use crate::pool::record::{DispatchRecord, OpenDispatch};
    let busy = crate::pool::fake_provider::claimed("m1", "p-slot-1", "started", "p", "aaaa1111");
    let mut slots = slot::pool_slots(&pid("p"), &[busy]);
    let open = OpenDispatch {
        task: "aaaa1111".into(),
        record: DispatchRecord {
            pool: "p".into(),
            slot: "p-slot-1".into(),
            machine: "m1".into(),
            job: "j".into(),
            model: "m".into(),
            from: "main".into(),
            branch: "b".into(),
            start: "s".into(),
            budget: 1800,
            at: 1000,
        },
    };
    annotate_running(&mut slots, &[open], 1095);
    assert_eq!(slots[0].running_secs, Some(95));
    assert_eq!(slots[0].budget_secs, Some(1800));
    let json = serde_json::to_string(&slots[0]).unwrap();
    assert!(json.contains("\"running_secs\":95") && json.contains("\"budget_secs\":1800"));
}

fn capture() -> (Progress, Rc<RefCell<Vec<String>>>) {
    let lines = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&lines);
    let progress = Progress::to(false, move |l| sink.borrow_mut().push(l.to_string()));
    (progress, lines)
}

#[test]
fn create_says_each_phase_and_beats_while_seeds_run() {
    let fake = Fake {
        never_stop: true,
        ..Fake::default()
    };
    let options = CreateOptions {
        timeout: Duration::from_millis(60),
        poll: Duration::from_millis(5),
        heartbeat: Duration::from_millis(10),
        ..fast()
    };
    let (progress, lines) = capture();
    create_slots(&fake, &config(1), &pid("p"), &options, &progress, inputs).unwrap();
    let said = lines.borrow();
    assert_eq!(said[0], "checking the pool's existing slots");
    assert_eq!(said[1], "creating ms-p-slot-1");
    assert_eq!(said[2], "seeding 1 slot(s)");
    let beat = said
        .iter()
        .find(|l| l.starts_with("waiting  "))
        .expect("a heartbeat");
    assert!(beat.starts_with("waiting  seeding ms-p-slot-1 "), "{beat}");
    assert!(beat.ends_with("(gives up at 0s)"), "{beat}");
}

#[test]
fn a_short_create_wait_prints_no_heartbeat() {
    let fake = Fake::default();
    let (progress, lines) = capture();
    create_slots(&fake, &config(1), &pid("p"), &fast(), &progress, inputs).unwrap();
    let said = lines.borrow();
    assert!(said.iter().all(|l| !l.starts_with("waiting")), "{said:?}");
    assert!(
        said.iter().any(|l| l == "marking ms-p-slot-1 ready"),
        "{said:?}"
    );
}

#[test]
fn a_quiet_progress_says_nothing_during_create() {
    let fake = Fake {
        never_stop: true,
        ..Fake::default()
    };
    let options = CreateOptions {
        timeout: Duration::from_millis(30),
        poll: Duration::from_millis(5),
        heartbeat: Duration::from_millis(1),
        ..fast()
    };
    let lines = Rc::new(RefCell::new(Vec::<String>::new()));
    let sink = Rc::clone(&lines);
    let quiet = Progress::to(true, move |l| sink.borrow_mut().push(l.to_string()));
    create_slots(&fake, &config(1), &pid("p"), &options, &quiet, inputs).unwrap();
    assert!(lines.borrow().is_empty());
}

#[test]
fn status_covers_every_pool_of_the_set_each_slot_naming_its_pool() {
    let (a, b) = (Fake::default(), Fake::default());
    a.add(existing("ms-a-slot-1", "ma1", "stopped", "a", 1));
    a.add(existing("ms-a-slot-2", "ma2", "stopped", "a", 2));
    b.add(existing("ms-b-slot-1", "mb1", "stopped", "b", 1));
    let (ia, ib) = (pid("a"), pid("b"));
    let set = PoolSet {
        members: vec![
            crate::pool::pools::Member {
                id: &ia,
                provider: &a,
            },
            crate::pool::pools::Member {
                id: &ib,
                provider: &b,
            },
        ],
        all: true,
    };
    let report = status_of(&set, &[], 1000);
    let seen: Vec<(&str, &str)> = report
        .slots
        .iter()
        .map(|s| (s.pool.as_str(), s.name.as_str()))
        .collect();
    assert_eq!(
        seen,
        [
            ("a", "ms-a-slot-1"),
            ("a", "ms-a-slot-2"),
            ("b", "ms-b-slot-1")
        ]
    );
    assert_eq!(report.pools, [("a".to_string(), 2), ("b".to_string(), 1)]);
    assert!(report.failures.is_empty());
    let one = status_of(&PoolSet::single(&ib, &b), &[], 1000);
    assert_eq!(one.slots.len(), 1);
    assert_eq!(
        a.lists.take(),
        1,
        "pool a was listed once, by the first call only"
    );
}

#[test]
fn a_pool_with_no_slots_still_shows_in_status() {
    let a = Fake::default();
    let ia = pid("a");
    let report = status_of(&PoolSet::single(&ia, &a), &[], 0);
    assert_eq!(report.pools, [("a".to_string(), 0)]);
}

#[test]
fn two_mailspaces_with_one_pool_name_on_one_host_never_see_each_other() {
    let host = Fake::default();
    let mine = PoolId::new("ms", "local");
    let theirs = PoolId::new("other", "local");
    let seed = || Ok((Secret::new("t".into()), "c".repeat(40)));
    create_slots(&host, &config(2), &mine, &fast(), &Progress::silent(), seed).unwrap();
    create_slots(
        &host,
        &config(1),
        &theirs,
        &fast(),
        &Progress::silent(),
        seed,
    )
    .unwrap();
    assert_eq!(pool_status(&host, &mine).unwrap().len(), 2);
    assert_eq!(pool_status(&host, &theirs).unwrap().len(), 1);
    let names: Vec<String> = pool_status(&host, &theirs)
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, ["other-local-slot-1"]);
    // `create` for one mailspace counts only its own slots.
    let again = create_slots(
        &host,
        &config(2),
        &theirs,
        &fast(),
        &Progress::silent(),
        seed,
    )
    .unwrap();
    assert_eq!(again.existing.len(), 1);
    assert_eq!(again.created.len(), 1);
    // Destroying all of one mailspace's pool leaves the other's alone.
    let gone = destroy_slots(&host, &mine, &DestroyTarget::All).unwrap();
    assert_eq!(gone.len(), 2);
    assert_eq!(pool_status(&host, &theirs).unwrap().len(), 2);
    // Another mailspace's slot cannot be named through this pool.
    let err = destroy_slots(
        &host,
        &mine,
        &DestroyTarget::Slot("other-local-slot-1".into()),
    );
    assert!(err.is_err());
    assert_eq!(pool_status(&host, &theirs).unwrap().len(), 2);
}

#[test]
fn the_self_test_pool_of_one_pool_is_not_that_pool() {
    let host = Fake::default();
    let real = pid("local");
    let seed = || Ok((Secret::new("t".into()), "c".repeat(40)));
    create_slots(
        &host,
        &config(1),
        &real.doctor(),
        &fast(),
        &Progress::silent(),
        seed,
    )
    .unwrap();
    assert!(pool_status(&host, &real).unwrap().is_empty());
    assert_eq!(pool_status(&host, &real.doctor()).unwrap().len(), 1);
}
