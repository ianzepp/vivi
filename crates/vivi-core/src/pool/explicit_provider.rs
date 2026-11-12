//! Route declared slot names to their exact SSH host and directory.
use super::config::PoolConfig;
use super::explicit::{SlotConfig, repository};
use super::job::sh_quote;
use super::provider::{CreateSpec, ExecOutput, Hold, MachineRecord, PoolProvider};
use super::runpod::HostContext;
use super::ssh::SshProvider;
use std::collections::BTreeMap;
use vivi_error::VivariumError;

struct Entry {
    slot: SlotConfig,
    provider: SshProvider,
    wire_id: String,
}

/// A pool's explicitly routed slots, possibly on different hosts.
pub struct ExplicitProvider {
    entries: Vec<Entry>,
    config: PoolConfig,
    ctx: HostContext,
}

impl ExplicitProvider {
    #[must_use]
    pub fn new(config: &PoolConfig, ctx: &HostContext) -> Self {
        Self::with_runners(config, ctx, |slot| {
            Box::new(super::ssh_cli::SshCli::new(&slot.host))
        })
    }

    fn with_runners(
        config: &PoolConfig,
        ctx: &HostContext,
        runner: impl Fn(&SlotConfig) -> Box<dyn super::docker_cli::Runner>,
    ) -> Self {
        let entries = config
            .explicit_slots
            .iter()
            .map(|slot| {
                let mut host_config = config.clone();
                host_config.ssh.host = Some(slot.host.clone());
                let script = format!(
                    "EXPLICIT_DIR={}\nEXPLICIT_SPACE={}\nEXPLICIT_POOL={}\n{}",
                    sh_quote(&slot.dir),
                    sh_quote(&ctx.mailspace),
                    sh_quote(&ctx.pool),
                    super::ssh_script::SLOT_SCRIPT
                );
                Entry {
                    slot: slot.clone(),
                    provider: SshProvider::with_runner_for_pool(
                        &host_config,
                        &ctx.mailspace,
                        &ctx.pool,
                        runner(slot),
                    )
                    .with_script(script, None),
                    wire_id: format!("{}/{}/slot-1", ctx.mailspace, ctx.pool),
                }
            })
            .collect();
        Self {
            entries,
            config: config.clone(),
            ctx: ctx.clone(),
        }
    }

    fn entry(&self, id: &str) -> Result<&Entry, VivariumError> {
        self.entries
            .iter()
            .find(|e| e.slot.name == id)
            .ok_or_else(|| VivariumError::Config(format!("unknown declared slot {id}")))
    }

    fn record(entry: &Entry, mut record: MachineRecord) -> MachineRecord {
        record.id.clone_from(&entry.slot.name);
        record.name.clone_from(&entry.slot.name);
        record
    }

    fn take(&self, entry: &Entry, task: &str, now: u64) -> Result<Option<Hold>, VivariumError> {
        let Some(hold) = entry.provider.claim(&entry.wire_id, task, now)? else {
            return Ok(None);
        };
        if let Err(error) =
            entry
                .provider
                .install_explicit(&self.config, &self.ctx, &entry.wire_id, &hold)
        {
            entry.provider.unclaim(&entry.wire_id)?;
            return Err(error);
        }
        Ok(Some(hold))
    }
}

impl PoolProvider for ExplicitProvider {
    fn doctor(&self, names: &[String]) -> Vec<super::doctor::Check> {
        self.entries
            .iter()
            .flat_map(|e| e.provider.doctor(names))
            .collect()
    }
    fn env_gaps_at_dispatch(&self, names: &[String]) -> Result<Vec<String>, VivariumError> {
        self.harness_env_gaps(names)
    }
    fn env_gap_hint(&self, names: &[String]) -> String {
        format!(
            "set {} in each declared host's login environment",
            names.join(", ")
        )
    }
    fn live_test_notice(&self) -> String {
        "declared working trees are persistent and are never deleted".into()
    }
    fn ensure_home(&self) -> Result<Option<String>, VivariumError> {
        Ok(None)
    }
    fn supplies_build_mount(&self) -> bool {
        false
    }
    fn build_persists(&self) -> bool {
        true
    }
    fn runs_unprivileged(&self) -> bool {
        true
    }
    fn list(&self) -> Result<Vec<MachineRecord>, VivariumError> {
        let mut records = Vec::new();
        for entry in &self.entries {
            records.push(Self::record(entry, entry.provider.machine(&entry.wire_id)?));
        }
        Ok(records)
    }
    fn machine(&self, id: &str) -> Result<MachineRecord, VivariumError> {
        let e = self.entry(id)?;
        Ok(Self::record(e, e.provider.machine(&e.wire_id)?))
    }
    fn create(&self, _spec: &CreateSpec) -> Result<String, VivariumError> {
        Err(VivariumError::Config(
            "declared slots are created by their first job; no pool create is needed".into(),
        ))
    }
    fn destroy(&self, id: &str) -> Result<(), VivariumError> {
        let e = self.entry(id)?;
        e.provider.destroy(&e.wire_id)?;
        e.provider.unclaim(&e.wire_id)
    }
    fn destroy_command(&self, id: &str) -> String {
        format!("stop declared slot {id} (working tree is never deleted)")
    }
    fn claim(&self, _id: &str, _task: &str, _now: u64) -> Result<Option<Hold>, VivariumError> {
        Ok(None)
    }
    fn claim_repository(
        &self,
        id: &str,
        task: &str,
        now: u64,
        source: Option<&str>,
    ) -> Result<Option<Hold>, VivariumError> {
        let e = self.entry(id)?;
        if source.and_then(repository).as_deref() != Some(&e.slot.repository) {
            return Ok(None);
        }
        self.take(e, task, now)
    }
    fn unclaim(&self, id: &str) -> Result<(), VivariumError> {
        let e = self.entry(id)?;
        e.provider.unclaim(&e.wire_id)
    }
    fn release(&self, _id: &str, _hold: &Hold) -> Result<(), VivariumError> {
        Ok(())
    }
    fn put_file(
        &self,
        id: &str,
        hold: &Hold,
        path: &str,
        content: &[u8],
    ) -> Result<(), VivariumError> {
        let e = self.entry(id)?;
        e.provider.put_file(&e.wire_id, hold, path, content)
    }
    fn seal(
        &self,
        id: &str,
        drop_env: &[&str],
        key: &str,
        value: &str,
    ) -> Result<(), VivariumError> {
        let e = self.entry(id)?;
        e.provider.seal(&e.wire_id, drop_env, key, value)
    }
    fn start(&self, id: &str) -> Result<(), VivariumError> {
        let e = self.entry(id)?;
        e.provider.start(&e.wire_id)
    }
    fn stop(&self, id: &str) -> Result<(), VivariumError> {
        let e = self.entry(id)?;
        e.provider.stop(&e.wire_id)
    }
    fn exec(&self, id: &str, script: &str, timeout: u32) -> Result<ExecOutput, VivariumError> {
        let e = self.entry(id)?;
        e.provider.exec(&e.wire_id, script, timeout)
    }
    fn read_stopped(
        &self,
        id: &str,
        paths: &[&str],
    ) -> Result<Option<BTreeMap<String, Vec<u8>>>, VivariumError> {
        let e = self.entry(id)?;
        e.provider.read_stopped(&e.wire_id, paths)
    }
    fn harness_env_gaps(&self, names: &[String]) -> Result<Vec<String>, VivariumError> {
        let mut gaps = Vec::new();
        for e in &self.entries {
            gaps.extend(e.provider.harness_env_gaps(names)?);
        }
        gaps.sort();
        gaps.dedup();
        Ok(gaps)
    }
}

#[cfg(test)]
#[path = "explicit_provider_test.rs"]
mod tests;
