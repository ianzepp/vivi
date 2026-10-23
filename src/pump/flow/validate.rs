//! Flow validation: the load-time rules of plan section 3.2.
//!
//! Run as a unit test over the shipped flows and at pump start.

use std::collections::{BTreeMap, BTreeSet};

use super::def::{FlowDef, FlowNode, NodeClass};

/// Check a loaded flow.
///
/// # Errors
/// Returns every violated rule, one message each.
pub fn validate(def: &FlowDef) -> Result<(), Vec<String>> {
    let mut problems = Vec::new();
    let ids: BTreeSet<&str> = def.nodes.iter().map(|node| node.id.as_str()).collect();
    check_edges_dangling(def, &ids, &mut problems);
    check_entries(def, &ids, &mut problems);
    check_acyclic(def, &ids, &mut problems);
    check_decisions(def, &mut problems);
    check_terminals_reached(def, &ids, &mut problems);
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

fn label(def: &FlowDef) -> &'static str {
    def.kind.name()
}

fn check_edges_dangling(def: &FlowDef, ids: &BTreeSet<&str>, problems: &mut Vec<String>) {
    for edge in &def.edges {
        for end in [&edge.from, &edge.to] {
            if !ids.contains(end.as_str()) {
                problems.push(format!(
                    "{} flow: edge {} -> {} names dangling node `{end}`",
                    label(def),
                    edge.from,
                    edge.to
                ));
            }
        }
    }
}

fn check_entries(def: &FlowDef, ids: &BTreeSet<&str>, problems: &mut Vec<String>) {
    for entry in &def.entries {
        if !ids.contains(entry.as_str()) {
            problems.push(format!(
                "{} flow: declared entry `{entry}` is not a node",
                label(def)
            ));
        }
    }
}

/// Kahn's algorithm: whatever cannot be ordered sits on or behind a cycle.
fn check_acyclic(def: &FlowDef, ids: &BTreeSet<&str>, problems: &mut Vec<String>) {
    let mut indegree: BTreeMap<&str, usize> = ids.iter().map(|id| (*id, 0)).collect();
    let mut out: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in &def.edges {
        if ids.contains(edge.from.as_str()) && ids.contains(edge.to.as_str()) {
            *indegree.entry(edge.to.as_str()).or_default() += 1;
            out.entry(edge.from.as_str())
                .or_default()
                .push(edge.to.as_str());
        }
    }
    let mut ready: Vec<&str> = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(id, _)| *id)
        .collect();
    let mut ordered = 0;
    while let Some(id) = ready.pop() {
        ordered += 1;
        for next in out.get(id).into_iter().flatten() {
            if let Some(degree) = indegree.get_mut(next) {
                *degree -= 1;
                if *degree == 0 {
                    ready.push(next);
                }
            }
        }
    }
    if ordered < ids.len() {
        let stuck: Vec<&str> = indegree
            .iter()
            .filter(|(_, degree)| **degree > 0)
            .map(|(id, _)| *id)
            .collect();
        problems.push(format!(
            "{} flow: cycle through {}",
            label(def),
            stuck.join(", ")
        ));
    }
}

fn node<'a>(def: &'a FlowDef, id: &str) -> Option<&'a FlowNode> {
    def.nodes.iter().find(|candidate| candidate.id == id)
}

fn is_decision(class: &NodeClass) -> bool {
    matches!(
        class,
        NodeClass::Decision | NodeClass::Join | NodeClass::Raisecheck
    )
}

fn check_decisions(def: &FlowDef, problems: &mut Vec<String>) {
    for decision in def.nodes.iter().filter(|node| is_decision(&node.class)) {
        let preds: Vec<&str> = def
            .edges
            .iter()
            .filter(|edge| edge.to == decision.id)
            .map(|edge| edge.from.as_str())
            .collect();
        check_predecessors(def, decision, &preds, problems);
        check_routing(def, decision, problems);
    }
}

fn check_predecessors(
    def: &FlowDef,
    decision: &FlowNode,
    preds: &[&str],
    problems: &mut Vec<String>,
) {
    match decision.class {
        NodeClass::Join if preds.len() != 2 => problems.push(format!(
            "{} flow: join `{}` has {} predecessors, expected 2",
            label(def),
            decision.id,
            preds.len()
        )),
        NodeClass::Decision => {
            let step = preds
                .first()
                .and_then(|id| node(def, id))
                .is_some_and(|pred| pred.class.is_step());
            if preds.len() != 1 || !step {
                problems.push(format!(
                    "{} flow: decision `{}` needs exactly one step predecessor, has {}",
                    label(def),
                    decision.id,
                    preds.len()
                ));
            }
        }
        _ => {}
    }
}

/// The word set a decision must route: a join and a raise check own theirs;
/// a plain decision takes its single predecessor's.
fn expected_words(def: &FlowDef, decision: &FlowNode) -> Option<&'static [&'static str]> {
    match decision.class {
        NodeClass::Decision => {
            let mut preds = def.edges.iter().filter(|edge| edge.to == decision.id);
            let pred = node(def, &preds.next()?.from)?;
            pred.class.is_step().then(|| pred.class.words())
        }
        _ => Some(decision.class.words()),
    }
}

fn check_routing(def: &FlowDef, decision: &FlowNode, problems: &mut Vec<String>) {
    let Some(expected) = expected_words(def, decision) else {
        return;
    };
    let mut targets: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in def.edges.iter().filter(|edge| edge.from == decision.id) {
        for word in &edge.words {
            targets
                .entry(word.as_str())
                .or_default()
                .push(edge.to.as_str());
        }
    }
    for word in expected {
        if !targets.contains_key(word) {
            problems.push(format!(
                "{} flow: decision `{}` leaves word `{word}` unrouted",
                label(def),
                decision.id
            ));
        }
    }
    for (word, to) in &targets {
        if !expected.contains(word) {
            problems.push(format!(
                "{} flow: decision `{}` routes unknown word `{word}`",
                label(def),
                decision.id
            ));
        }
        if to.len() > 1 && !parallel_successors(def, to) {
            problems.push(format!(
                "{} flow: decision `{}` sends word `{word}` to several targets that are not parallel steps joining together",
                label(def),
                decision.id
            ));
        }
    }
}

/// Parallel successors are steps that all feed the same join.
fn parallel_successors(def: &FlowDef, targets: &[&str]) -> bool {
    let joins_of = |id: &str| -> BTreeSet<&str> {
        def.edges
            .iter()
            .filter(|edge| edge.from == id)
            .filter(|edge| node(def, &edge.to).is_some_and(|n| n.class == NodeClass::Join))
            .map(|edge| edge.to.as_str())
            .collect()
    };
    let all_steps = targets
        .iter()
        .all(|id| node(def, id).is_some_and(|n| n.class.is_step()));
    let mut common = joins_of(targets[0]);
    for id in &targets[1..] {
        let next = joins_of(id);
        common.retain(|join| next.contains(join));
    }
    all_steps && !common.is_empty()
}

fn is_terminal(class: &NodeClass) -> bool {
    matches!(
        class,
        NodeClass::Done | NodeClass::Restart(_) | NodeClass::Lock | NodeClass::Park
    )
}

/// Walk the edges backwards from every terminal; a node left unvisited cannot
/// reach one.
fn check_terminals_reached(def: &FlowDef, ids: &BTreeSet<&str>, problems: &mut Vec<String>) {
    let mut reached: BTreeSet<&str> = def
        .nodes
        .iter()
        .filter(|node| is_terminal(&node.class))
        .map(|node| node.id.as_str())
        .collect();
    let mut frontier: Vec<&str> = reached.iter().copied().collect();
    while let Some(id) = frontier.pop() {
        for edge in def.edges.iter().filter(|edge| edge.to == id) {
            if ids.contains(edge.from.as_str()) && reached.insert(edge.from.as_str()) {
                frontier.push(edge.from.as_str());
            }
        }
    }
    for node in &def.nodes {
        if !reached.contains(node.id.as_str()) {
            problems.push(format!(
                "{} flow: node `{}` cannot reach done, a restart, lock or park",
                label(def),
                node.id
            ));
        }
    }
}

#[cfg(test)]
#[path = "validate_test.rs"]
mod tests;
