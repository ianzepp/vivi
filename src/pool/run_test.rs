use std::collections::BTreeMap;

use super::*;
use crate::pool::PoolHarness;
use crate::pool::fake_provider::Fake;
use crate::pool::slot::{META_BASE, META_STATE, STATE_READY};

fn config(slots: u32) -> PoolConfig {
    toml::from_str(&format!(
        r#"
app = "a"
repo = "o/r"
slots = {slots}
image = "debian:12"
build_dir = "target"
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
    let report = create_slots(&fake, &config(3), "p", &fast(), inputs).unwrap();
    assert_eq!(report.existing.len(), 1);
    let names: Vec<&str> = report.created.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(names, ["p-slot-1", "p-slot-3"]);
    assert!(report.created.iter().all(SlotOutcome::seeded));
    let specs = fake.created.borrow();
    assert_eq!(specs[0].metadata[META_BASE], "c".repeat(40));
    assert_eq!(specs[0].env["GH_TOKEN"], "tok-secret");
    assert_eq!(specs[0].command, ["bash", "/work/boot.sh"]);
    assert!(fake.destroyed.borrow().is_empty());
}

#[test]
fn create_with_full_pool_does_nothing_and_skips_token_minting() {
    let fake = Fake::default();
    fake.machines
        .borrow_mut()
        .push(existing("p-slot-1", "o1", "stopped", "p", 1));
    let report = create_slots(&fake, &config(1), "p", &fast(), || {
        Err(VivariumError::Other("must not mint".into()))
    })
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
    let err = create_slots(&fake, &config(3), "p", &options, inputs).unwrap_err();
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
    let report = create_slots(&fake, &config(4), "p", &options, inputs).unwrap();
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
    let report = create_slots(&fake, &config(1), "p", &options, inputs).unwrap();
    assert!(report.created[0].timed_out);
    assert!(!report.created[0].seeded());
    assert!(fake.destroyed.borrow().is_empty());
}

#[test]
fn nonzero_exit_is_not_a_clean_seed() {
    let outcome = SlotOutcome {
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
    let one = destroy_slots(&fake, "p", &DestroyTarget::Slot("p-slot-2".into())).unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!(*fake.destroyed.borrow(), ["m2"]);
    let err = destroy_slots(&fake, "p", &DestroyTarget::Slot("slot-quad-1".into())).unwrap_err();
    assert!(err.to_string().contains("no slot"));
    let all = destroy_slots(&fake, "p", &DestroyTarget::All).unwrap();
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
    let slots = pool_status(&fake, "p").unwrap();
    assert_eq!(slots[0].state, "free");
}

#[test]
fn a_clean_seed_clears_the_fetch_token_then_marks_the_slot_ready() {
    let fake = Fake::default();
    let report = create_slots(&fake, &config(1), "p", &fast(), inputs).unwrap();
    assert!(report.created[0].seeded());
    let cleared = fake.env_cleared.borrow();
    assert_eq!(cleared.len(), 1);
    assert_eq!(cleared[0].1, ["GH_TOKEN"]);
    assert_eq!(
        fake.meta("id-p-slot-1", META_STATE).as_deref(),
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
    create_slots(&fake, &config(1), "p", &options, inputs).unwrap();
    let slots = pool_status(&fake, "p").unwrap();
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
    let report = create_slots(&fake, &config(2), "p", &fast(), inputs).unwrap();
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
fn prepare_app_creates_a_missing_app_once() {
    let fake = Fake::default();
    let config = with_env(&[]);
    assert!(prepare_app(&fake, &config, "a").unwrap().created);
    assert_eq!(*fake.apps_created.borrow(), ["personal"]);
    assert!(!prepare_app(&fake, &config, "a").unwrap().created);
    assert_eq!(fake.apps_created.borrow().len(), 1);
}

#[test]
fn missing_secrets_are_the_harness_names_the_app_lacks() {
    let fake = Fake::default();
    fake.secrets.borrow_mut().push("HAVE_KEY".into());
    let config = with_env(&["HAVE_KEY", "NEED_KEY", "OTHER_KEY"]);
    let missing = missing_secrets(&fake, &config).unwrap();
    assert_eq!(missing, ["NEED_KEY", "OTHER_KEY"]);
    assert!(missing_secrets(&fake, &with_env(&[])).unwrap().is_empty());
    assert!(
        missing_secrets(&fake, &config_no_harness())
            .unwrap()
            .is_empty()
    );
}

fn config_no_harness() -> PoolConfig {
    config(1)
}

#[test]
fn the_secrets_warning_names_the_secrets_and_the_command_and_no_value() {
    assert!(secrets_warning("app", &[]).is_none());
    let text = secrets_warning("my-app", &["A_KEY".to_string(), "B_KEY".to_string()]).unwrap();
    assert!(text.contains("A_KEY, B_KEY"));
    assert!(text.contains("fly secrets set -a my-app A_KEY=<value> B_KEY=<value>"));
}

#[test]
fn create_does_not_return_until_a_fresh_listing_shows_the_slot_free() {
    let fake = Fake::default();
    fake.stale_lists.set(4);
    let options = CreateOptions {
        confirm_timeout: Duration::from_secs(5),
        ..fast()
    };
    let report = create_slots(&fake, &config(1), "p", &options, inputs).unwrap();
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
    let report = create_slots(&fake, &config(1), "p", &options, inputs).unwrap();
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
    let mut slots = slot::pool_slots("p", &[busy]);
    let open = OpenDispatch {
        task: "aaaa1111".into(),
        record: DispatchRecord {
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
