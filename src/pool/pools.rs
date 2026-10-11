//! The named pools of a mailspace: which exist, which one a command covers,
//! and the set a command works over.
//!
//! The rules for `--pool`: when the mailspace has exactly one pool the flag
//! may be omitted. With more than one, the commands that create, destroy,
//! dispatch or scale (and `doctor --test`) require it; the read and collect
//! commands (`status`, `doctor`, `wait`, `reconcile`) cover every pool when it
//! is not given, and one pool when it is. There is no default-pool setting.

use std::collections::BTreeMap;

use super::config::PoolConfig;
use super::open_provider;
use super::provider::PoolProvider;
use super::runpod::HostContext;
use super::slot::PoolId;
use crate::error::VivariumError;
use crate::mailspace::Mailspace;

/// Whether a command needs `--pool` once there is more than one pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// The command acts on exactly one pool.
    One,
    /// The command covers every pool when the flag is absent.
    Any,
}

/// One configured pool, open: its identity, its settings, its machine host.
pub struct PoolHandle {
    pub id: PoolId,
    pub config: PoolConfig,
    pub provider: Box<dyn PoolProvider>,
}

/// The pool tables of a mailspace.
///
/// # Errors
/// Returns a configuration error when the retired single `[pool]` table is
/// present, or when no `[pools.<name>]` table exists.
pub fn configured(space: &Mailspace) -> Result<&BTreeMap<String, PoolConfig>, VivariumError> {
    if space.config.retired_pool.is_some() {
        return Err(VivariumError::Config(
            "the single [pool] table is retired: write each pool as [pools.<name>] (for example [pools.local]) with every setting in it, and its host settings as [pools.<name>.fly], [pools.<name>.docker] or [pools.<name>.ssh]; a fly pool needs [pools.<name>.fly] app".into(),
        ));
    }
    if space.config.pools.is_empty() {
        return Err(VivariumError::Config(
            "no [pools.<name>] table in .vivi/mailspace.toml (for example [pools.local])".into(),
        ));
    }
    Ok(&space.config.pools)
}

/// The pool names a command covers: the `--pool` flag if given, else the one
/// pool there is, else (for [`Need::Any`]) all of them.
///
/// # Errors
/// Returns an error for an unknown name, or for [`Need::One`] with several
/// pools and no flag; both list the pool names.
pub fn select(
    names: &[&str],
    flag: Option<&str>,
    need: Need,
) -> Result<Vec<String>, VivariumError> {
    let listed = names.join(", ");
    if let Some(wanted) = flag {
        return if names.contains(&wanted) {
            Ok(vec![wanted.to_string()])
        } else {
            Err(VivariumError::Message(format!(
                "unknown pool \"{wanted}\" (pools: {listed})"
            )))
        };
    }
    match (names, need) {
        ([only], _) => Ok(vec![(*only).to_string()]),
        (_, Need::Any) => Ok(names.iter().map(|n| (*n).to_string()).collect()),
        (_, Need::One) => Err(VivariumError::Message(format!(
            "this mailspace has several pools ({listed}); pass --pool <name>"
        ))),
    }
}

/// The selected pools, open but not validated (the doctor validates as one of
/// its checks).
///
/// # Errors
/// Returns the errors of [`configured`] and [`select`].
pub fn open_unvalidated(
    space: &Mailspace,
    flag: Option<&str>,
    need: Need,
) -> Result<Vec<PoolHandle>, VivariumError> {
    let all = configured(space)?;
    let names: Vec<&str> = all.keys().map(String::as_str).collect();
    let chosen = select(&names, flag, need)?;
    let host = |pool: &str| HostContext {
        dir: space.dir.clone(),
        mailspace: super::slot::space_slug(&space.config.name),
        pool: pool.to_string(),
        pools: names.iter().map(|n| (*n).to_string()).collect(),
    };
    Ok(chosen
        .into_iter()
        .filter_map(|name| {
            let config = all.get(&name)?.clone();
            let ctx = host(&name);
            Some(PoolHandle {
                id: PoolId::new(&space.config.name, &name),
                provider: open_provider(&config, &ctx),
                config,
            })
        })
        .collect())
}

/// The selected pools, open and validated.
///
/// # Errors
/// Returns the errors of [`open_unvalidated`], or the first pool's
/// validation error (which names the pool).
pub fn open(
    space: &Mailspace,
    flag: Option<&str>,
    need: Need,
) -> Result<Vec<PoolHandle>, VivariumError> {
    let handles = open_unvalidated(space, flag, need)?;
    for handle in &handles {
        handle.config.validate(&handle.id)?;
    }
    Ok(handles)
}

/// The pools a `wait` or `reconcile` works over, open and validated. Unlike
/// the other commands it accepts a mailspace with no pool table left (and no
/// `--pool`): the open dispatches of pools that were removed are then still
/// its business.
///
/// # Errors
/// Returns the errors of [`open`], except for a mailspace with no pools.
pub fn open_for_collect(
    space: &Mailspace,
    flag: Option<&str>,
) -> Result<Vec<PoolHandle>, VivariumError> {
    if flag.is_none() && space.config.retired_pool.is_none() && space.config.pools.is_empty() {
        return Ok(Vec::new());
    }
    open(space, flag, Need::Any)
}

/// The one pool a [`Need::One`] command works on, open and validated.
///
/// # Errors
/// Returns the errors of [`open`].
pub fn open_one(space: &Mailspace, flag: Option<&str>) -> Result<PoolHandle, VivariumError> {
    let mut handles = open(space, flag, Need::One)?;
    handles
        .pop()
        .ok_or_else(|| VivariumError::Config("no pool selected".into()))
}

/// A pool as the collect and reconcile engines use it: its identity and its
/// machine host.
#[derive(Clone, Copy)]
pub struct Member<'a> {
    pub id: &'a PoolId,
    pub provider: &'a dyn PoolProvider,
}

/// The pools a collect or reconcile round works over.
pub struct PoolSet<'a> {
    pub members: Vec<Member<'a>>,
    /// True when no `--pool` was given: a dispatch whose pool is not a member
    /// (it was removed from the config) is then this round's business.
    pub all: bool,
}

impl<'a> PoolSet<'a> {
    /// The set for `handles`, covering every pool when `all`.
    #[must_use]
    pub fn of(handles: &'a [PoolHandle], all: bool) -> Self {
        Self {
            members: handles
                .iter()
                .map(|h| Member {
                    id: &h.id,
                    provider: h.provider.as_ref(),
                })
                .collect(),
            all,
        }
    }

    /// A set of one pool (an explicit `--pool`, or the self-test).
    #[must_use]
    pub fn single(id: &'a PoolId, provider: &'a dyn PoolProvider) -> Self {
        Self {
            members: vec![Member { id, provider }],
            all: false,
        }
    }

    /// The member named `pool`.
    #[must_use]
    pub fn find(&self, pool: &str) -> Option<Member<'a>> {
        self.members.iter().copied().find(|m| m.id.pool == pool)
    }

    /// True when a dispatch recorded in `pool` is this set's business.
    #[must_use]
    pub fn covers(&self, pool: &str) -> bool {
        self.all || self.find(pool).is_some()
    }
}

#[cfg(test)]
#[path = "pools_test.rs"]
mod tests;
