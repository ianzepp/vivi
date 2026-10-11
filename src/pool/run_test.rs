use std::cell::RefCell;
use std::collections::BTreeMap;

use super::*;
use crate::pool::slot::{META_BASE, META_POOL, META_SLOT};

/// A fake host: created machines start `started` and stop after two lists.
#[derive(Default)]
struct Fake {
    machines: RefCell<Vec<MachineRecord>>,
    lists: RefCell<u32>,
    created: RefCell<Vec<CreateSpec>>,
    destroyed: RefCell<Vec<String>>,
    never_stop: bool,
}

impl PoolProvider for Fake {
    fn list(&self) -> Result<Vec<MachineRecord>, VivariumError> {
        *self.lists.borrow_mut() += 1;
        if !self.never_stop && *self.lists.borrow() > 2 {
            for m in self.machines.borrow_mut().iter_mut() {
                if m.state == "started" && m.metadata.contains_key(META_BASE) {
                    m.state = "stopped".into();
                    m.exit_code = Some(0);
                }
            }
        }
        Ok(self.machines.borrow().clone())
    }

    fn create(&self, spec: &CreateSpec) -> Result<String, VivariumError> {
        let id = format!("id-{}", spec.name);
        self.machines.borrow_mut().push(MachineRecord {
            id: id.clone(),
            name: spec.name.clone(),
            state: "started".into(),
            region: spec.region.clone(),
            created_at: "now".into(),
            metadata: spec.metadata.clone(),
            exit_code: None,
        });
        self.created.borrow_mut().push(spec.clone());
        Ok(id)
    }

    fn destroy(&self, id: &str) -> Result<(), VivariumError> {
        self.destroyed.borrow_mut().push(id.to_string());
        self.machines.borrow_mut().retain(|m| m.id != id);
        Ok(())
    }
}

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
    }
}

#[allow(clippy::unnecessary_wraps)]
fn inputs() -> Result<(Secret, String), VivariumError> {
    Ok((Secret::new("tok-secret".into()), "c".repeat(40)))
}

fn existing(name: &str, id: &str, state: &str, pool: &str, index: u32) -> MachineRecord {
    MachineRecord {
        id: id.into(),
        name: name.into(),
        state: state.into(),
        region: "iad".into(),
        created_at: "then".into(),
        metadata: BTreeMap::from([
            (META_POOL.to_string(), pool.to_string()),
            (META_SLOT.to_string(), index.to_string()),
        ]),
        exit_code: None,
    }
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
