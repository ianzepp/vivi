//! Aggregated and per-provider health.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Instant;

use serde::Serialize;

/// One backend's last probe.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderHealth {
    pub name: String,
    pub kind: String,
    pub status: &'static str,
    pub last_error: Option<String>,
    pub checked_ms_ago: Option<u64>,
}

/// In-memory health table shared by the listener and supervisor.
#[derive(Default)]
pub struct HealthTable {
    inner: Mutex<BTreeMap<String, Snapshot>>,
}

#[derive(Debug, Clone)]
struct Snapshot {
    kind: String,
    ok: bool,
    last_error: Option<String>,
    checked: Instant,
}

impl HealthTable {
    /// Record a probe result. `error` is already redacted by the caller.
    pub fn record(&self, name: &str, kind: &str, ok: bool, error: Option<String>) {
        let Ok(mut map) = self.inner.lock() else {
            return;
        };
        map.insert(
            name.to_string(),
            Snapshot {
                kind: kind.to_string(),
                ok,
                last_error: error,
                checked: Instant::now(),
            },
        );
    }

    /// Snapshot every known provider, filling gaps from `names`.
    #[must_use]
    pub fn snapshot(&self, names: &[(String, String)]) -> Vec<ProviderHealth> {
        let Ok(map) = self.inner.lock() else {
            return names
                .iter()
                .map(|(name, kind)| unknown(name, kind))
                .collect();
        };
        names
            .iter()
            .map(|(name, kind)| match map.get(name) {
                Some(snap) => ProviderHealth {
                    name: name.clone(),
                    kind: snap.kind.clone(),
                    status: if snap.ok { "ok" } else { "down" },
                    last_error: snap.last_error.clone(),
                    checked_ms_ago: Some(ago_ms(snap.checked)),
                },
                None => unknown(name, kind),
            })
            .collect()
    }

    /// Last recorded status for one provider.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<ProviderHealth> {
        let Ok(map) = self.inner.lock() else {
            return None;
        };
        let snap = map.get(name)?;
        Some(ProviderHealth {
            name: name.to_string(),
            kind: snap.kind.clone(),
            status: if snap.ok { "ok" } else { "down" },
            last_error: snap.last_error.clone(),
            checked_ms_ago: Some(ago_ms(snap.checked)),
        })
    }
}

fn unknown(name: &str, kind: &str) -> ProviderHealth {
    ProviderHealth {
        name: name.to_string(),
        kind: kind.to_string(),
        status: "unknown",
        last_error: None,
        checked_ms_ago: None,
    }
}

fn ago_ms(checked: Instant) -> u64 {
    u64::try_from(checked.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Aggregate JSON: `ok` only when every named provider is `ok`.
#[must_use]
pub fn aggregate_json(rows: &[ProviderHealth]) -> serde_json::Value {
    let ok = !rows.is_empty() && rows.iter().all(|row| row.status == "ok");
    serde_json::json!({
        "ok": ok,
        "providers": rows,
    })
}
