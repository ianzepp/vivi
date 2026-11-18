//! Queue selection. Steps with host affinity (implement, check, review,
//! gate) name a host when they are scheduled: the slice's last host first,
//! for its warm tree, then the others. Everything here is deterministic,
//! because workflow code calls it.

use std::time::Duration;

use crate::types::Routing;

/// The task queue a template names for a host: `agent-{host}` → `agent-studio`.
pub fn queue_for(template: &str, host: &str) -> String {
    template.replace("{host}", host)
}

/// The hosts to try, in order: `preferred` first when it is one of
/// `hosts`, then the rest starting at a slot chosen by `key`, so different
/// slices spread across hosts while each one keeps the same order.
pub fn hosts_in_order(hosts: &[String], preferred: Option<&str>, key: &str) -> Vec<String> {
    if hosts.is_empty() {
        return Vec::new();
    }
    let start = usize::try_from(fnv1a(key) % hosts.len() as u64).unwrap_or(0);
    let mut order: Vec<String> = hosts[start..]
        .iter()
        .chain(hosts[..start].iter())
        .cloned()
        .collect();
    if let Some(p) = preferred
        && let Some(i) = order.iter().position(|h| h == p)
    {
        let host = order.remove(i);
        order.insert(0, host);
    }
    order
}

/// One placement: the queue and how long the task may wait to start there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub host: String,
    pub queue: String,
    pub wait: Duration,
}

/// Placements for one step: the first host gets the short affinity wait,
/// the others the longer fallback wait. A lone host gets the fallback wait:
/// with nowhere to move the step, a short wait would only park it while
/// that host is busy.
pub fn placements(
    routing: &Routing,
    template: &str,
    hosts: &[String],
    preferred: Option<&str>,
    key: &str,
) -> Vec<Placement> {
    let order = hosts_in_order(hosts, preferred, key);
    let alone = order.len() == 1;
    order
        .into_iter()
        .enumerate()
        .map(|(i, host)| Placement {
            queue: queue_for(template, &host),
            wait: Duration::from_millis(if i == 0 && !alone {
                routing.affinity_wait_ms
            } else {
                routing.fallback_wait_ms
            }),
            host,
        })
        .collect()
}

/// FNV-1a: stable across runs and platforms, unlike `DefaultHasher`.
fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(test)]
#[path = "routing_test.rs"]
mod tests;
