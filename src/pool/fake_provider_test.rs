//! A fake machine host shared by the pool tests. Machines live in memory;
//! leases, metadata, delivered files, starts and stops are all recorded.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};

use super::provider::{CreateSpec, ExecOutput, Lease, MachineRecord, PoolProvider};
use super::slot::{META_BASE, META_POOL, META_SLOT, META_STATE, META_TASK, STATE_READY};
use crate::error::VivariumError;

type Hook = Box<dyn Fn(&Fake, &str)>;

#[derive(Default)]
pub struct Fake {
    pub machines: RefCell<Vec<MachineRecord>>,
    pub lists: RefCell<u32>,
    pub created: RefCell<Vec<CreateSpec>>,
    pub destroyed: RefCell<Vec<String>>,
    /// Created machines never stop (for seed timeouts).
    pub never_stop: bool,
    pub leases: RefCell<HashMap<String, String>>,
    /// `(machine, path, content)` of every delivered file.
    pub files: RefCell<Vec<(String, String, Vec<u8>)>>,
    /// `(machine, names)` of every env removal.
    pub env_cleared: RefCell<Vec<(String, Vec<String>)>>,
    /// What `exec` prints on a running machine, by machine id.
    pub exec_out: RefCell<HashMap<String, String>>,
    /// Every start/stop/exec in order, as `verb id`.
    pub calls: RefCell<Vec<String>>,
    /// Runs right after a lease is granted (race simulation).
    pub after_lease: RefCell<Option<Hook>>,
    /// Starting a machine fails.
    pub fail_start: Cell<bool>,
}

impl Fake {
    pub fn add(&self, machine: MachineRecord) {
        self.machines.borrow_mut().push(machine);
    }

    pub fn state_of(&self, id: &str) -> String {
        self.get(id).state
    }

    pub fn meta(&self, id: &str, key: &str) -> Option<String> {
        self.get(id).metadata.get(key).cloned()
    }

    fn get(&self, id: &str) -> MachineRecord {
        self.machines
            .borrow()
            .iter()
            .find(|m| m.id == id)
            .cloned()
            .unwrap_or_else(|| panic!("no machine {id}"))
    }

    fn edit(&self, id: &str, change: impl FnOnce(&mut MachineRecord)) -> Result<(), VivariumError> {
        let mut machines = self.machines.borrow_mut();
        let machine = machines
            .iter_mut()
            .find(|m| m.id == id)
            .ok_or_else(|| VivariumError::Message(format!("no machine {id}")))?;
        change(machine);
        Ok(())
    }

    pub fn calls_to(&self, verb: &str) -> usize {
        self.calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with(verb))
            .count()
    }
}

/// A seeded pool slot.
pub fn slot(id: &str, name: &str, state: &str, pool: &str, index: u32) -> MachineRecord {
    MachineRecord {
        id: id.into(),
        name: name.into(),
        state: state.into(),
        region: "iad".into(),
        created_at: "then".into(),
        metadata: BTreeMap::from([
            (META_POOL.to_string(), pool.to_string()),
            (META_SLOT.to_string(), index.to_string()),
            (META_BASE.to_string(), "a".repeat(40)),
            (META_STATE.to_string(), STATE_READY.to_string()),
        ]),
        exit_code: None,
    }
}

/// A seeded slot already claimed by `task`.
pub fn claimed(id: &str, name: &str, state: &str, pool: &str, task: &str) -> MachineRecord {
    let mut machine = slot(id, name, state, pool, 1);
    machine.metadata.insert(META_TASK.into(), task.into());
    machine
}

impl PoolProvider for Fake {
    fn list(&self) -> Result<Vec<MachineRecord>, VivariumError> {
        *self.lists.borrow_mut() += 1;
        if !self.never_stop && *self.lists.borrow() > 2 {
            for m in self.machines.borrow_mut().iter_mut() {
                let fresh = m.metadata.get(META_STATE).map(String::as_str) == Some("seeding");
                if m.state == "started" && fresh {
                    m.state = "stopped".into();
                    m.exit_code = Some(0);
                }
            }
        }
        Ok(self.machines.borrow().clone())
    }

    fn create(&self, spec: &CreateSpec) -> Result<String, VivariumError> {
        let id = format!("id-{}", spec.name);
        self.add(MachineRecord {
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

    fn machine(&self, id: &str) -> Result<MachineRecord, VivariumError> {
        self.machines
            .borrow()
            .iter()
            .find(|m| m.id == id)
            .cloned()
            .ok_or_else(|| VivariumError::Message(format!("no machine {id}")))
    }

    fn lease(&self, id: &str, _ttl_secs: u32) -> Result<Option<Lease>, VivariumError> {
        if self.leases.borrow().contains_key(id) {
            return Ok(None);
        }
        let nonce = format!("nonce-{id}");
        self.leases
            .borrow_mut()
            .insert(id.to_string(), nonce.clone());
        if let Some(hook) = self.after_lease.borrow().as_ref() {
            hook(self, id);
        }
        Ok(Some(Lease { nonce }))
    }

    fn release(&self, id: &str, _lease: &Lease) -> Result<(), VivariumError> {
        self.leases.borrow_mut().remove(id);
        Ok(())
    }

    fn set_metadata(&self, id: &str, key: &str, value: &str) -> Result<(), VivariumError> {
        self.edit(id, |m| {
            m.metadata.insert(key.to_string(), value.to_string());
        })
    }

    fn clear_metadata(&self, id: &str, key: &str) -> Result<(), VivariumError> {
        self.edit(id, |m| {
            m.metadata.remove(key);
        })
    }

    fn put_file(
        &self,
        id: &str,
        lease: &Lease,
        path: &str,
        content: &[u8],
    ) -> Result<(), VivariumError> {
        if self.leases.borrow().get(id) != Some(&lease.nonce) {
            return Err(VivariumError::Message("not the lease holder".into()));
        }
        self.files
            .borrow_mut()
            .push((id.to_string(), path.to_string(), content.to_vec()));
        Ok(())
    }

    fn seal(
        &self,
        id: &str,
        drop_env: &[&str],
        key: &str,
        value: &str,
    ) -> Result<(), VivariumError> {
        let names = drop_env.iter().map(ToString::to_string).collect();
        self.env_cleared.borrow_mut().push((id.to_string(), names));
        self.set_metadata(id, key, value)
    }

    fn start(&self, id: &str) -> Result<(), VivariumError> {
        if self.fail_start.get() {
            return Err(VivariumError::Message("cannot start".into()));
        }
        self.calls.borrow_mut().push(format!("start {id}"));
        self.edit(id, |m| m.state = "started".into())
    }

    fn stop(&self, id: &str) -> Result<(), VivariumError> {
        self.calls.borrow_mut().push(format!("stop {id}"));
        self.edit(id, |m| m.state = "stopped".into())
    }

    fn exec(&self, id: &str, _script: &str, _timeout: u32) -> Result<ExecOutput, VivariumError> {
        self.calls.borrow_mut().push(format!("exec {id}"));
        if self.state_of(id) != "started" {
            return Err(VivariumError::Message("machine not running".into()));
        }
        Ok(ExecOutput {
            stdout: self.exec_out.borrow().get(id).cloned().unwrap_or_default(),
            stderr: String::new(),
            exit_code: 0,
        })
    }
}
