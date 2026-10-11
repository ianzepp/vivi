//! Remote lane pool: `[pool]` config, slot discovery, and the create, status,
//! destroy, dispatch, wait and reconcile commands.
//!
//! A pool is a set of preseeded remote machines ("slots"). A slot is created
//! once, seeds itself (tools, a shallow checkout at a base commit, a warmed
//! build output packed onto its persistent disk), and then stops; a stopped
//! slot is free. Nothing here is specific to a language, a repository, or an
//! agent harness: image, setup, seed, harness, and build output are
//! configuration.
//!
//! Slot state lives in the provider, never in local files: slots are found by
//! machine metadata (see [`slot`]). A slot is `free`, `seeding`, `busy` (a
//! task holds it and the machine runs), or `finished-uncollected` (a task
//! holds it and the machine stopped, its result still on the disk). A dispatch
//! claims a slot atomically (see [`claim`]), delivers one job archive, and is
//! recorded on the task (see [`record`]); `wait` and `reconcile` collect the
//! result and free the slot (see [`collect`]). The provider sits behind
//! [`provider::PoolProvider`] so tests use a fake. `vivi pool doctor --test`
//! runs a live exercise of all of this (see [`selftest`]).

pub mod bootscript;
pub mod claim;
pub mod collect;
pub mod config;
pub mod dispatch;
pub mod doctor;
pub mod doctor_pool;
pub mod doctor_repo;
#[cfg(test)]
#[path = "pool/fake_provider_test.rs"]
pub(crate) mod fake_provider;
pub mod fly;
pub mod fly_api;
pub mod fly_doctor;
pub mod fly_token;
pub mod github;
pub mod job;
pub mod provider;
pub mod record;
pub mod run;
pub mod selftest;
pub mod selftest_source;
pub mod selftest_steps;
pub mod selftest_world;
pub mod slot;
pub mod work;

pub use config::{PoolCommitter, PoolConfig, PoolGithub, PoolHarness};
pub use provider::PoolProvider;
