//! Flow loading from shipped Mermaid into a `FlowDef`.

use super::def::{FlowDef, FlowEdge, FlowKind, FlowNode, NodeClass};
use super::registry::shipped;
use crate::error::VivariumError;
use crate::mailspace::{MermaidNode, parse_flowchart};

/// The id of the unclassed terminal that ends a run.
const TERMINAL_ID: &str = "done";

/// Load a shipped flow.
///
/// # Errors
/// Returns [`VivariumError::Parse`] when the Mermaid source does not parse or
/// a node has no recognised class.
pub fn load(kind: FlowKind) -> Result<FlowDef, VivariumError> {
    let chart = parse_flowchart(shipped(kind))?;
    let nodes = chart
        .nodes
        .iter()
        .map(|node| map_node(kind, node))
        .collect::<Result<Vec<_>, _>>()?;
    let edges = chart
        .edges
        .into_iter()
        .map(|edge| FlowEdge {
            from: edge.from,
            to: edge.to,
            words: split_words(edge.label.as_deref()),
        })
        .collect();
    Ok(FlowDef {
        kind,
        nodes,
        edges,
        entries: entries(kind),
    })
}

fn map_node(kind: FlowKind, node: &MermaidNode) -> Result<FlowNode, VivariumError> {
    let class = match node.class.as_deref() {
        Some(name) => NodeClass::parse(name).ok_or_else(|| {
            VivariumError::Parse(format!(
                "{} flow: node `{}` has unknown class `{name}`",
                kind.name(),
                node.source_id
            ))
        })?,
        None if node.kind == "decision" => NodeClass::Decision,
        None if node.source_id == TERMINAL_ID => NodeClass::Done,
        None => {
            return Err(VivariumError::Parse(format!(
                "{} flow: node `{}` has no class",
                kind.name(),
                node.source_id
            )));
        }
    };
    Ok(FlowNode {
        id: node.source_id.clone(),
        class,
        label: node.label.clone(),
    })
}

fn split_words(label: Option<&str>) -> Vec<String> {
    label
        .map(|text| {
            text.split(',')
                .map(str::trim)
                .filter(|word| !word.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn entries(kind: FlowKind) -> Vec<String> {
    let names: &[&str] = match kind {
        FlowKind::Build => &["plan", "research", "slice", "rework1"],
        FlowKind::Research => &["research"],
        FlowKind::Release => &["wait"],
    };
    names.iter().map(|name| (*name).to_string()).collect()
}

#[cfg(test)]
#[path = "load_test.rs"]
mod tests;
