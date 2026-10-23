//! Tests for flow validation.

use super::validate;
use crate::pump::flow::def::{FlowDef, FlowEdge, FlowKind, FlowNode, NodeClass};
use crate::pump::flow::load::load;

fn build() -> FlowDef {
    load(FlowKind::Build).unwrap()
}

fn problems(def: &FlowDef) -> String {
    validate(def).unwrap_err().join("\n")
}

fn edge(from: &str, to: &str, words: &[&str]) -> FlowEdge {
    FlowEdge {
        from: from.to_string(),
        to: to.to_string(),
        words: words.iter().map(|word| (*word).to_string()).collect(),
    }
}

#[test]
fn shipped_flows_validate() {
    for kind in [FlowKind::Build, FlowKind::Research, FlowKind::Release] {
        let def = load(kind).unwrap();
        if let Err(found) = validate(&def) {
            panic!("{}: {found:?}", kind.name());
        }
    }
}

#[test]
fn unrouted_word_is_named() {
    let mut def = build();
    def.edges.retain(|e| !(e.from == "pq" && e.to == "parked"));
    let found = problems(&def);
    assert!(
        found.contains("decision `pq` leaves word `operator` unrouted"),
        "{found}"
    );
    assert!(found.contains("`hold` unrouted"), "{found}");
}

#[test]
fn unknown_word_is_named() {
    let mut def = build();
    def.edges.push(edge("rq", "cto", &["mystery"]));
    assert!(problems(&def).contains("routes unknown word `mystery`"));
}

#[test]
fn dangling_node_is_named() {
    let mut def = build();
    def.edges.push(edge("cto", "nowhere", &[]));
    assert!(problems(&def).contains("dangling node `nowhere`"));
}

#[test]
fn missing_entry_is_named() {
    let mut def = build();
    def.entries.push("ghost".to_string());
    assert!(problems(&def).contains("declared entry `ghost` is not a node"));
}

#[test]
fn cycle_is_named() {
    let mut def = build();
    def.edges.push(edge("done", "research", &[]));
    assert!(problems(&def).contains("cycle through"));
}

#[test]
fn node_without_a_terminal_is_named() {
    let mut def = build();
    def.nodes.push(FlowNode {
        id: "orphan".to_string(),
        class: NodeClass::Planner,
        label: "Orphan".to_string(),
    });
    assert!(problems(&def).contains("node `orphan` cannot reach done"));
}

#[test]
fn join_needs_two_predecessors() {
    let mut def = build();
    def.edges.retain(|e| !(e.from == "test1" && e.to == "j1"));
    assert!(problems(&def).contains("join `j1` has 1 predecessors, expected 2"));
}

#[test]
fn repeated_word_needs_parallel_targets() {
    let mut def = build();
    def.edges.push(edge("b1", "cto", &["built"]));
    assert!(problems(&def).contains("word `built` to several targets"));
}
