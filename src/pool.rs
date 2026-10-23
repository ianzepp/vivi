//! Remote lane pools: `[pools.<name>]` config, slot discovery, and the create,
//! status, destroy, scale, dispatch, wait and reconcile commands.
//!
//! A pool is a set of preseeded remote machines ("slots"). A slot is created
//! once, seeds itself (tools, a shallow checkout at a base commit, a warmed
//! build output packed onto its persistent disk), and then stops; a stopped
//! slot is free. Nothing here is specific to a language, a repository, or an
//! agent harness: image, setup, seed, harness, and build output are
//! configuration.
//!
//! Slot state lives in the provider, never in local files: slots are found by
//! machine metadata (see [`slot`]); a slot belongs to a mailspace AND a pool,
//! and both marks must match before a pool sees or touches it. A slot is `free`, `seeding`, `busy` (a
//! task holds it and the machine runs), or `finished-uncollected` (a task
//! holds it and the machine stopped, its result still on the disk). A dispatch
//! claims a slot atomically (see [`claim`]), delivers one job archive, and is
//! recorded on the task (see [`record`]); `wait` and `reconcile` collect the
//! result and free the slot (see [`collect`]). The provider sits behind
//! [`provider::PoolProvider`] so tests use a fake; `fly`, `docker`, `ssh`, `local` and
//! `runpod` are the five machine hosts (see [`open_provider`]). `runpod` alone has no
//! resting slots: a slot is a pod rented for one dispatch (see
//! [`provider::SlotLifetime`]). `vivi pool doctor --test`
//! runs a live exercise of all of this (see [`selftest`]).

pub mod apfs;
pub mod bands;
pub mod bootscript;
pub mod bridge;
pub mod claim;
pub mod clean;
pub mod clean_rule;
pub mod clean_slot;
pub mod collect;
pub mod commit_hygiene;
pub mod config;
pub mod config_bands;
pub mod config_bridge;
pub mod config_depth;
pub mod config_restore;
pub mod config_worker;
pub mod dispatch;
pub mod docker;
pub mod docker_cli;
pub mod docker_doctor;
pub mod docker_state;
pub mod docker_tar;
pub mod doctor;
pub mod doctor_pool;
pub mod doctor_repo;
pub mod extras;
pub mod facts;
#[cfg(test)]
#[path = "pool/fake_provider_test.rs"]
pub(crate) mod fake_provider;
pub mod fly;
pub mod fly_api;
pub mod fly_doctor;
pub mod fly_token;
pub mod github;
pub mod job;
pub mod local;
pub mod local_branch;
pub mod local_doctor;
#[cfg(test)]
#[path = "pool/local_fixture_test.rs"]
pub(crate) mod local_fixture;
pub mod local_git;
pub mod local_proc;
pub mod local_site;
pub mod local_slots;
pub mod plain_git;
pub mod pools;
pub mod porcelain;
pub mod progress;
pub mod provider;
pub mod ramdisk;
pub mod reconcile_parallel;
pub mod record;
pub mod requires;
pub mod reset;
pub mod role_dispatch;
pub mod run;
pub mod runpod;
pub mod runpod_claim;
pub mod runpod_cli;
pub mod runpod_config;
pub mod runpod_doctor;
#[cfg(test)]
#[path = "pool/runpod_fixture_test.rs"]
pub(crate) mod runpod_fixture;
pub mod runpod_lock;
pub mod runpod_name;
pub mod runpod_open;
pub mod runpod_pin;
pub mod runpod_pod;
pub mod scale;
pub mod selftest;
pub mod selftest_pods;
pub mod selftest_source;
pub mod selftest_steps;
pub mod selftest_world;
pub mod slot;
pub mod ssh;
pub mod ssh_cli;
pub mod ssh_doctor;
pub mod ssh_script;
pub mod ssh_state;
pub mod work;
pub mod worker;

pub use config::{PoolCommitter, PoolConfig, PoolGithub, PoolHarness};
pub use provider::PoolProvider;

/// The machine host `config` names, ready to use. A `fly` pool without an
/// `app` gets an empty one here; validation rejects it before anything acts.
/// `ctx` says where the mailspace keeps files, its storage identity and its
/// pool names, for hosts that need project-local or id-scoped storage.
#[must_use]
pub fn open_provider(config: &PoolConfig, ctx: &runpod::HostContext) -> Box<dyn PoolProvider> {
    match config.provider.as_str() {
        "runpod" => Box::new(runpod::RunpodProvider::new(config, ctx)),
        "docker" => Box::new(docker::DockerProvider::new(config)),
        "ssh" => Box::new(ssh::SshProvider::new(config, ctx)),
        "worker" => Box::new(worker::WorkerProvider::new(config, ctx)),
        "local" => Box::new(local::LocalProvider::new(config, ctx)),
        _ => Box::new(fly::FlyctlProvider::new(
            config.fly.app.as_deref().unwrap_or_default(),
            &config.fly,
        )),
    }
}
