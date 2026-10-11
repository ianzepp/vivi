//! A fake machine host shared by the pool tests. Machines live in memory;
//! leases, metadata, delivered files, starts and stops are all recorded.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};

use super::doctor::Check;
use super::provider::{CreateSpec, ExecOutput, Hold, MachineRecord, PoolProvider};
use super::slot::{
    META_BASE, META_CLAIMED, META_POOL, META_SLOT, META_STATE, META_TASK, STATE_READY,
};
use crate::error::VivariumError;

type Hook = Box<dyn Fn(&Fake, &str)>;
/// Decides what `exec` prints on a machine; `None` falls back to `exec_out`.
type ExecHook = Box<dyn Fn(&Fake, &str) -> Option<String>>;

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
    /// The next this-many `list` calls show every ready slot as still seeding
    /// (a listing that trails the seal).
    pub stale_lists: Cell<u32>,
    /// Whether the home exists, and what `ensure_home` created.
    pub app_exists: Cell<bool>,
    pub apps_created: RefCell<Vec<String>>,
    /// The app's secret names.
    pub secrets: RefCell<Vec<String>>,
    /// What `doctor` reports for the provider.
    pub doctor_checks: RefCell<Vec<Check>>,
    /// The next this-many `exec` calls fail (the provider is unreachable).
    pub exec_fails: Cell<u32>,
    /// The next this-many `machine` reads fail.
    pub machine_fails: Cell<u32>,
    /// The next this-many `stop` calls are accepted but do not take.
    pub stops_ignored: Cell<u32>,
    /// Runs before every `machine` read (machines that change on their own).
    pub on_machine_read: RefCell<Option<Hook>>,
    /// Computes the output of `exec` on a running machine.
    pub exec_hook: RefCell<Option<ExecHook>>,
    /// Destroying a machine fails and leaves it in place.
    pub destroy_fails: Cell<bool>,
    /// When set, the provider reads a stopped machine's files directly (every
    /// stopped machine holds these) instead of asking for a restart.
    pub stopped_files: RefCell<Option<BTreeMap<String, Vec<u8>>>>,
}

impl Fake {
    pub fn add(&self, machine: MachineRecord) {
        self.machines.borrow_mut().push(machine);
    }

    pub fn state_of(&self, id: &str) -> String {
        self.get(id).state
    }

    /// Change a machine's state the way the machine itself would.
    pub fn set_state(&self, id: &str, state: &str) {
        let mut machines = self.machines.borrow_mut();
        if let Some(machine) = machines.iter_mut().find(|m| m.id == id) {
            machine.state = state.to_string();
        }
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

    pub fn set_metadata(&self, id: &str, key: &str, value: &str) -> Result<(), VivariumError> {
        self.edit(id, |m| {
            m.metadata.insert(key.to_string(), value.to_string());
        })
    }

    pub fn calls_to(&self, verb: &str) -> usize {
        self.calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with(verb))
            .count()
    }
}

/// True and counts down while `n` is positive.
fn take_one(n: &Cell<u32>) -> bool {
    let left = n.get();
    n.set(left.saturating_sub(1));
    left > 0
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
    fn ensure_home(&self) -> Result<Option<String>, VivariumError> {
        if self.app_exists.get() {
            return Ok(None);
        }
        self.app_exists.set(true);
        self.apps_created.borrow_mut().push("fake".to_string());
        Ok(Some("created fake home".to_string()))
    }

    fn doctor(&self, _harness_env: &[String]) -> Vec<Check> {
        self.doctor_checks.borrow().clone()
    }

    fn harness_env_gaps(&self, names: &[String]) -> Result<Vec<String>, VivariumError> {
        let have = self.secrets.borrow();
        Ok(names
            .iter()
            .filter(|n| !have.contains(n))
            .cloned()
            .collect())
    }

    fn env_gaps_at_dispatch(&self, names: &[String]) -> Result<Vec<String>, VivariumError> {
        self.harness_env_gaps(names)
    }

    fn supplies_build_mount(&self) -> bool {
        false
    }

    fn build_persists(&self) -> bool {
        false
    }

    fn runs_unprivileged(&self) -> bool {
        false
    }

    fn env_gap_hint(&self, missing: &[String]) -> String {
        format!("fake hint: set {}", missing.join(", "))
    }

    fn live_test_notice(&self) -> String {
        "it uses the fake".to_string()
    }

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
        let mut machines = self.machines.borrow().clone();
        if take_one(&self.stale_lists) {
            for m in &mut machines {
                m.metadata
                    .insert(META_STATE.to_string(), "seeding".to_string());
            }
        }
        Ok(machines)
    }

    fn create(&self, spec: &CreateSpec) -> Result<String, VivariumError> {
        let id = format!("id-{}", spec.name);
        self.add(MachineRecord {
            id: id.clone(),
            name: spec.name.clone(),
            state: "started".into(),
            region: "iad".into(),
            created_at: "now".into(),
            metadata: spec.metadata.clone(),
            exit_code: None,
        });
        self.created.borrow_mut().push(spec.clone());
        Ok(id)
    }

    fn destroy(&self, id: &str) -> Result<(), VivariumError> {
        if self.destroy_fails.get() {
            return Err(VivariumError::Message("cannot destroy".into()));
        }
        self.destroyed.borrow_mut().push(id.to_string());
        self.machines.borrow_mut().retain(|m| m.id != id);
        Ok(())
    }

    fn machine(&self, id: &str) -> Result<MachineRecord, VivariumError> {
        if let Some(hook) = self.on_machine_read.borrow().as_ref() {
            hook(self, id);
        }
        if take_one(&self.machine_fails) {
            return Err(VivariumError::Message("Fly unreachable".into()));
        }
        let found = self.machines.borrow().iter().find(|m| m.id == id).cloned();
        Ok(found.unwrap_or_else(|| MachineRecord::gone(id)))
    }

    fn destroy_command(&self, id: &str) -> String {
        format!("fake destroy {id}")
    }

    fn claim(&self, id: &str, task: &str, now: u64) -> Result<Option<Hold>, VivariumError> {
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
        // The list may be stale: re-read under the hold before marking.
        let fresh = self.machine(id)?;
        if !super::slot::is_free(&fresh) {
            self.leases.borrow_mut().remove(id);
            return Ok(None);
        }
        self.set_metadata(id, META_TASK, task)?;
        self.set_metadata(id, META_CLAIMED, &now.to_string())?;
        Ok(Some(Hold { nonce }))
    }

    fn unclaim(&self, id: &str) -> Result<(), VivariumError> {
        self.edit(id, |m| {
            m.metadata.remove(META_TASK);
            m.metadata.remove(META_CLAIMED);
        })
    }

    fn release(&self, id: &str, _hold: &Hold) -> Result<(), VivariumError> {
        self.leases.borrow_mut().remove(id);
        Ok(())
    }

    fn put_file(
        &self,
        id: &str,
        hold: &Hold,
        path: &str,
        content: &[u8],
    ) -> Result<(), VivariumError> {
        if self.leases.borrow().get(id) != Some(&hold.nonce) {
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

    fn read_stopped(
        &self,
        id: &str,
        paths: &[&str],
    ) -> Result<Option<BTreeMap<String, Vec<u8>>>, VivariumError> {
        self.calls.borrow_mut().push(format!("read_stopped {id}"));
        let held = self.stopped_files.borrow();
        Ok(held.as_ref().map(|files| {
            files
                .iter()
                .filter(|(p, _)| paths.contains(&p.as_str()))
                .map(|(p, b)| (p.clone(), b.clone()))
                .collect()
        }))
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
        if take_one(&self.stops_ignored) {
            return Ok(());
        }
        self.edit(id, |m| m.state = "stopped".into())
    }

    fn exec(&self, id: &str, _script: &str, _timeout: u32) -> Result<ExecOutput, VivariumError> {
        self.calls.borrow_mut().push(format!("exec {id}"));
        if take_one(&self.exec_fails) {
            return Err(VivariumError::Message("Fly exec failed (HTTP 401)".into()));
        }
        if self.state_of(id) != "started" {
            return Err(VivariumError::Message("machine not running".into()));
        }
        let hooked = self
            .exec_hook
            .borrow()
            .as_ref()
            .and_then(|hook| hook(self, id));
        Ok(ExecOutput {
            stdout: hooked
                .or_else(|| self.exec_out.borrow().get(id).cloned())
                .unwrap_or_default(),
            stderr: String::new(),
            exit_code: 0,
        })
    }
}
