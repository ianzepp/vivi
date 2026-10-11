//! Remote lane pool (first iteration): `[pool]` config, slot discovery, and
//! the create/status/destroy commands.
//!
//! A pool is a set of preseeded remote machines ("slots"). A slot is created
//! once, seeds itself (tools, a shallow checkout at a base commit, a warmed
//! build output packed onto its persistent disk), and then stops; a stopped
//! slot is free. Nothing here is specific to a language, a repository, or an
//! agent harness: image, setup, seed, and build output are configuration.
//!
//! Slot state lives in the provider, never in local files: slots are found by
//! machine metadata (see [`slot`]). The provider sits behind
//! [`provider::PoolProvider`] so tests use a fake and a direct API client can
//! replace the `flyctl` subprocess later.

pub mod bootscript;
pub mod config;
pub mod fly;
pub mod github;
pub mod provider;
pub mod run;
pub mod slot;

pub use config::{PoolConfig, PoolGithub, PoolHarness};
pub use provider::PoolProvider;
