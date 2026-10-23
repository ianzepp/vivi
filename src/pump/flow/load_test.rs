//! Tests for loading the shipped flows.

use super::load;
use crate::pump::flow::def::{FlowDef, FlowKind, NodeClass};

fn node_class<'a>(def: &'a FlowDef, id: &str) -> &'a NodeClass {
    &def.nodes
        .iter()
        .find(|node| node.id == id)
        .unwrap_or_else(|| panic!("node {id}"))
        .class
}

#[test]
fn all_three_shipped_flows_load() {
    for kind in [FlowKind::Build, FlowKind::Research, FlowKind::Release] {
        let def = load(kind).unwrap_or_else(|err| panic!("{}: {err}", kind.name()));
        assert_eq!(def.kind, kind);
        for entry in &def.entries {
            assert!(def.nodes.iter().any(|node| &node.id == entry), "{entry}");
        }
    }
}

#[test]
fn build_flow_has_the_plan_counts() {
    let def = load(FlowKind::Build).unwrap();
    assert_eq!(def.nodes.len(), 51);
    assert_eq!(def.edges.len(), 84);
    assert_eq!(def.entries, ["plan", "research", "slice", "repair1"]);
}

#[test]
fn research_and_release_counts() {
    let research = load(FlowKind::Research).unwrap();
    assert_eq!((research.nodes.len(), research.edges.len()), (3, 2));
    let release = load(FlowKind::Release).unwrap();
    assert_eq!((release.nodes.len(), release.edges.len()), (2, 1));
}

#[test]
fn edge_labels_split_into_words() {
    let def = load(FlowKind::Build).unwrap();
    let edge = def
        .edges
        .iter()
        .find(|edge| edge.from == "rq" && edge.to == "cto")
        .unwrap();
    assert_eq!(edge.words, ["NOT READY", "NEEDS FURTHER REVIEW", "failed"]);
    let plain = def.edges.iter().find(|e| e.from == "plan").unwrap();
    assert!(plain.words.is_empty());
}

#[test]
fn node_classes_map() {
    let def = load(FlowKind::Build).unwrap();
    assert_eq!(node_class(&def, "rq"), &NodeClass::Decision);
    assert_eq!(node_class(&def, "j1"), &NodeClass::Join);
    assert_eq!(node_class(&def, "done"), &NodeClass::Done);
    assert_eq!(node_class(&def, "build1"), &NodeClass::Fanout);
    assert_eq!(node_class(&def, "merge"), &NodeClass::MergeMain);
    assert_eq!(
        node_class(&def, "r_plan"),
        &NodeClass::Restart("plan".to_string())
    );
    assert_eq!(node_class(&def, "parked"), &NodeClass::Park);
    assert_eq!(node_class(&def, "locked"), &NodeClass::Lock);
    let release = load(FlowKind::Release).unwrap();
    assert_eq!(node_class(&release, "wait"), &NodeClass::Milestone);
}
