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
//! [`provider::PoolProvider`] so tests use a fake; `fly`, `docker`, `ssh` and `local` are the
//! four machine hosts (see [`open_provider`]). `vivi pool doctor --test`
//! runs a live exercise of all of this (see [`selftest`]).

pub mod bootscript;
pub mod claim;
pub mod collect;
pub mod config;
pub mod dispatch;
pub mod docker;
pub mod docker_cli;
pub mod docker_doctor;
pub mod docker_state;
pub mod docker_tar;
pub mod doctor;
pub mod doctor_pool;
pub mod doctor_repo;
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
pub mod local_slots;
pub mod pools;
pub mod porcelain;
pub mod progress;
pub mod provider;
pub mod record;
pub mod requires;
pub mod run;
pub mod scale;
pub mod selftest;
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

pub use config::{PoolCommitter, PoolConfig, PoolGithub, PoolHarness};
pub use provider::PoolProvider;

/// The machine host `config` names, ready to use. A `fly` pool without an
/// `app` gets an empty one here; validation rejects it before anything acts.
#[must_use]
pub fn open_provider(config: &PoolConfig) -> Box<dyn PoolProvider> {
    match config.provider.as_str() {
        "docker" => Box::new(docker::DockerProvider::new(config)),
        "ssh" => Box::new(ssh::SshProvider::new(config)),
        "local" => Box::new(local::LocalProvider::new(config)),
        _ => Box::new(fly::FlyctlProvider::new(
            config.fly.app.as_deref().unwrap_or_default(),
            &config.fly,
        )),
    }
}
