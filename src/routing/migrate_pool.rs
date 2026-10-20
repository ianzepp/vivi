//! The pump-host pool `config migrate` adds for the merge role, filled from
//! the hand role's pool so it can run jobs at once.

use toml_edit::{DocumentMut, Item, Table};

use crate::mailspace::MailspaceConfig;
use crate::mailspace::config_edit::PoolAdd;
use crate::pump::config::PumpConfig;

use super::migrate::{MigratePlan, NewPool};

/// Pool keys copied as they stand: provider, source, harness (its `run`,
/// `models` and `env` names), committer, build directory and bands. The
/// merge pool keeps its own `slots`, and has no `fallback` or `caps`.
const COPIED_KEYS: [&str; 7] = [
    "provider",
    "repo",
    "build_dir",
    "committer",
    "harness",
    "bands",
    "image",
];

/// The pool the new merge pool takes its settings from: the hand role's
/// pool, when it is a local pool the file defines. A pool on another
/// provider needs that provider's own tables, which are not copied.
pub(super) fn settings_source(
    config: &MailspaceConfig,
    pump: &PumpConfig,
    plan: &MigratePlan,
) -> Option<String> {
    let name = super::migrate::hand_pool(config, pump, plan)?;
    config
        .pools
        .get(&name)
        .filter(|pool| pool.provider == "local")
        .map(|_| name)
}

/// Add the pool with its own slots, then copy the hand pool's settings.
pub(super) fn add_pool(
    doc: &mut DocumentMut,
    pool: &NewPool,
) -> Result<(), crate::error::VivariumError> {
    crate::mailspace::config_edit::upsert_pool(
        doc,
        &PoolAdd {
            name: pool.name.clone(),
            provider: pool.provider.clone(),
            slots: pool.slots,
            worktree_of: None,
            repo: None,
            image: None,
            root: None,
        },
    )?;
    if let Some(from) = &pool.copy_from {
        copy_settings(doc, from, &pool.name);
    }
    Ok(())
}

fn copy_settings(doc: &mut DocumentMut, from: &str, to: &str) {
    let Some(source) = pools(doc).and_then(|pools| pools.get(from)?.as_table().cloned()) else {
        return;
    };
    let Some(target) = pools(doc).and_then(|pools| pools.get_mut(to)?.as_table_mut()) else {
        return;
    };
    for key in COPIED_KEYS {
        if let Some(item) = source.get(key) {
            target.insert(key, item.clone());
        }
    }
    if let Some(path) = source
        .get("local")
        .and_then(|local| local.get("worktree_of"))
    {
        target["local"]["worktree_of"] = path.clone();
    }
}

fn pools(doc: &mut DocumentMut) -> Option<&mut Table> {
    doc.get_mut("pools").and_then(Item::as_table_mut)
}
