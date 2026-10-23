//! Narrow Mermaid flowchart profile for work-graph import.
//!
//! The profile covers planning vocabulary: rect and stadium nodes are work,
//! rhombus nodes are operator decisions, `:::kind` / `class` statements mark
//! decision/stub/parked gates, and dotted edges are non-gating couplings.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::error::VivariumError;

/// Node kinds that never dispatch: `graph activate` refuses them and
/// frontiers list them as gates instead of ready work.
pub const GATE_KINDS: [&str; 3] = ["decision", "stub", "parked"];

/// One-line summary of the accepted syntax, appended to parse errors.
const SUBSET_HINT: &str = "accepted subset: flowchart|graph TD|TB|BT|RL|LR; nodes id, \
     id[label], id{label} (decision), id([label]) with optional id:::kind \
     (decision|stub|parked); edges --> and -.-> / -.- (dotted couplings never gate \
     readiness) with optional |label|; subgraph/end; %% comments; classDef and style \
     lines are ignored";

#[must_use]
pub fn is_gate_kind(kind: &str) -> bool {
    GATE_KINDS.contains(&kind)
}

/// Parsed flowchart under the supported work-graph profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MermaidFlowchart {
    pub direction: String,
    pub nodes: Vec<MermaidNode>,
    pub edges: Vec<MermaidEdge>,
}

/// One node declaration (explicit or inferred from an edge endpoint).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MermaidNode {
    pub source_id: String,
    pub label: String,
    pub subgraph: Option<String>,
    pub kind: String,
    /// Every `:::class` / `class X name` seen on the node, gate kind or not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
}

/// Directed edge: `to` requires `from` when the style is `solid`; `dotted`
/// edges are non-gating couplings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MermaidEdge {
    pub from: String,
    pub to: String,
    pub label: Option<String>,
    pub style: String,
}

/// Parse a supported Mermaid flowchart / graph document.
///
/// # Errors
/// Returns [`VivariumError::Parse`] for unsupported syntax or structural errors.
pub fn parse_flowchart(source: &str) -> Result<MermaidFlowchart, VivariumError> {
    let lines: Vec<&str> = source.lines().collect();
    let (direction, start) = find_header(&lines)?;
    let mut nodes: HashMap<String, MermaidNode> = HashMap::new();
    let mut edges: Vec<MermaidEdge> = Vec::new();
    let mut subgraph_stack: Vec<String> = Vec::new();
    let mut edge_set: HashSet<(String, String)> = HashSet::new();
    let mut class_assignments: Vec<(Vec<String>, String)> = Vec::new();

    for (idx, raw) in lines.iter().enumerate().skip(start) {
        let line_no = idx + 1;
        let line = strip_comment(raw).trim();
        if line.is_empty() || is_styling_line(line) {
            continue;
        }
        if let Some(rest) = line.strip_prefix("subgraph") {
            let (id, label) = parse_subgraph_header(rest.trim(), line_no)?;
            subgraph_stack.push(id.clone());
            let _ = label;
            continue;
        }
        if line == "end" {
            if subgraph_stack.pop().is_none() {
                return Err(parse_err(line_no, "unexpected 'end' without open subgraph"));
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("class ") {
            class_assignments.push(parse_class_statement(rest, line_no)?);
            continue;
        }
        if contains_arrow(line) {
            parse_edge_line(
                line,
                line_no,
                &subgraph_stack,
                &mut nodes,
                &mut edges,
                &mut edge_set,
            )?;
            continue;
        }
        parse_node_line(line, line_no, &subgraph_stack, &mut nodes)?;
    }

    if !subgraph_stack.is_empty() {
        return Err(parse_err(0, "unclosed subgraph block"));
    }
    if nodes.is_empty() {
        return Err(parse_err(0, "flowchart has no nodes"));
    }
    apply_class_kinds(&mut nodes, &class_assignments);
    assemble_flowchart(direction, nodes, edges)
}

/// Sort nodes/edges deterministically, then validate endpoints and reject
/// cycles across all edges regardless of style.
fn assemble_flowchart(
    direction: String,
    nodes: HashMap<String, MermaidNode>,
    edges: Vec<MermaidEdge>,
) -> Result<MermaidFlowchart, VivariumError> {
    let mut node_list: Vec<MermaidNode> = nodes.into_values().collect();
    node_list.sort_by(|a, b| a.source_id.cmp(&b.source_id));
    let mut edges = edges;
    edges.sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));
    validate_endpoints(&node_list, &edges)?;
    detect_cycle(&node_list, &edges)?;
    Ok(MermaidFlowchart {
        direction,
        nodes: node_list,
        edges,
    })
}

fn find_header(lines: &[&str]) -> Result<(String, usize), VivariumError> {
    for (idx, raw) in lines.iter().enumerate() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let kind = parts.next().unwrap_or("");
        if kind != "flowchart" && kind != "graph" {
            return Err(parse_err(
                idx + 1,
                &format!("unsupported Mermaid start '{kind}'; expected flowchart or graph"),
            ));
        }
        let direction = parts
            .next()
            .ok_or_else(|| parse_err(idx + 1, "missing flowchart direction"))?;
        if !matches!(direction, "TB" | "TD" | "BT" | "RL" | "LR") {
            return Err(parse_err(
                idx + 1,
                &format!("unsupported direction '{direction}'"),
            ));
        }
        if parts.next().is_some() {
            return Err(parse_err(idx + 1, "unexpected tokens after direction"));
        }
        return Ok((direction.to_string(), idx + 1));
    }
    Err(parse_err(0, "empty Mermaid source"))
}

fn parse_subgraph_header(
    rest: &str,
    line_no: usize,
) -> Result<(String, Option<String>), VivariumError> {
    if rest.is_empty() {
        return Err(parse_err(line_no, "subgraph requires an id"));
    }
    if let Some((id, after)) = rest.split_once('[') {
        let id = id.trim();
        validate_id(id, line_no)?;
        let (label, _) = parse_delimited_label(after, ']', line_no)?;
        return Ok((id.to_string(), Some(label)));
    }
    let id = rest.trim();
    validate_id(id, line_no)?;
    Ok((id.to_string(), None))
}

/// `class id1,id2 name` — applies a gate kind when the class name is one,
/// otherwise records styling to ignore. Whitespace inside the id list is
/// tolerated (`class a, b name`).
fn parse_class_statement(
    rest: &str,
    line_no: usize,
) -> Result<(Vec<String>, String), VivariumError> {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    let Some((class, id_parts)) = parts.split_last() else {
        return Err(parse_err(
            line_no,
            "class statement requires node ids and a class name",
        ));
    };
    let ids = id_parts
        .join("")
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect();
    Ok((ids, class.to_string()))
}

/// Apply gate-kind class assignments after parsing so statement order
/// relative to node declarations does not matter.
fn apply_class_kinds(
    nodes: &mut HashMap<String, MermaidNode>,
    assignments: &[(Vec<String>, String)],
) {
    for (ids, class) in assignments {
        for id in ids {
            if let Some(node) = nodes.get_mut(id) {
                if is_gate_kind(class) {
                    node.kind = class.clone();
                }
                node.class = Some(class.clone());
            }
        }
    }
}

fn parse_node_line(
    line: &str,
    line_no: usize,
    subgraph_stack: &[String],
    nodes: &mut HashMap<String, MermaidNode>,
) -> Result<(), VivariumError> {
    let shape = parse_shape(line, line_no)?;
    ensure_node(
        nodes,
        &shape.id,
        shape.label.as_deref().unwrap_or(&shape.id),
        current_subgraph(subgraph_stack),
        shape.kind.as_deref(),
        shape.class.as_deref(),
    );
    Ok(())
}

fn parse_edge_line(
    line: &str,
    line_no: usize,
    subgraph_stack: &[String],
    nodes: &mut HashMap<String, MermaidNode>,
    edges: &mut Vec<MermaidEdge>,
    edge_set: &mut HashSet<(String, String)>,
) -> Result<(), VivariumError> {
    let parts = split_edge_chain(line, line_no)?;
    if parts.len() < 2 {
        return Err(parse_err(
            line_no,
            "edge chain needs at least two endpoints",
        ));
    }
    for window in parts.windows(2) {
        let from_seg = &window[0];
        let to_seg = &window[1];
        let from = parse_shape(&from_seg.raw, line_no)?;
        let to = parse_shape(&to_seg.raw, line_no)?;
        ensure_node(
            nodes,
            &from.id,
            from.label.as_deref().unwrap_or(&from.id),
            current_subgraph(subgraph_stack),
            from.kind.as_deref(),
            from.class.as_deref(),
        );
        ensure_node(
            nodes,
            &to.id,
            to.label.as_deref().unwrap_or(&to.id),
            current_subgraph(subgraph_stack),
            to.kind.as_deref(),
            to.class.as_deref(),
        );
        let key = (from.id.clone(), to.id.clone());
        if edge_set.insert(key) {
            edges.push(MermaidEdge {
                from: from.id,
                to: to.id,
                label: from_seg.label.clone(),
                style: from_seg.style.clone().unwrap_or_else(|| "solid".into()),
            });
        }
    }
    Ok(())
}

/// One endpoint plus the arrow leaving it (absent on the final endpoint).
struct ChainSegment {
    raw: String,
    label: Option<String>,
    style: Option<String>,
}

/// Split `a --> b -.->|lbl| c` into endpoint segments carrying the style of
/// the arrow that leaves them.
fn split_edge_chain(line: &str, line_no: usize) -> Result<Vec<ChainSegment>, VivariumError> {
    let mut out = Vec::new();
    let mut rest = line;
    loop {
        if let Some((idx, len, style)) = find_arrow(rest) {
            let left = rest[..idx].trim().to_string();
            if left.is_empty() {
                return Err(parse_err(line_no, "empty edge endpoint"));
            }
            let after_arrow = &rest[idx + len..];
            let (edge_label, next) = parse_optional_edge_label(after_arrow, line_no)?;
            out.push(ChainSegment {
                raw: left,
                label: edge_label,
                style: Some(style.to_string()),
            });
            rest = next;
        } else {
            let right = rest.trim().to_string();
            if right.is_empty() {
                return Err(parse_err(line_no, "edge chain ends without endpoint"));
            }
            out.push(ChainSegment {
                raw: right,
                label: None,
                style: None,
            });
            break;
        }
    }
    Ok(out)
}

/// Arrow tokens, longest-first so `-.->` wins over its `-.-` prefix.
const ARROW_TOKENS: [(&str, &str); 3] = [("-.->", "dotted"), ("-->", "solid"), ("-.-", "dotted")];

fn find_arrow(s: &str) -> Option<(usize, usize, &'static str)> {
    let mut best: Option<(usize, usize, &'static str)> = None;
    for (text, style) in ARROW_TOKENS {
        if let Some(idx) = s.find(text) {
            let better = match best {
                Some((best_idx, best_len, _)) => {
                    idx < best_idx || (idx == best_idx && text.len() > best_len)
                }
                None => true,
            };
            if better {
                best = Some((idx, text.len(), style));
            }
        }
    }
    best
}

fn contains_arrow(s: &str) -> bool {
    find_arrow(s).is_some()
}

fn parse_optional_edge_label(
    after_arrow: &str,
    line_no: usize,
) -> Result<(Option<String>, &str), VivariumError> {
    let trimmed = after_arrow.trim_start();
    if let Some(rest) = trimmed.strip_prefix('|') {
        let end = rest
            .find('|')
            .ok_or_else(|| parse_err(line_no, "unclosed edge label"))?;
        let label = rest[..end].trim().to_string();
        Ok((Some(label), rest[end + 1..].trim_start()))
    } else {
        Ok((None, trimmed))
    }
}

/// One endpoint (or standalone node line) after shape and class parsing.
struct ParsedShape {
    id: String,
    label: Option<String>,
    kind: Option<String>,
    class: Option<String>,
}

/// Parse `id`, `id[label]`, `id{label}`, `id([label])`, each with an
/// optional trailing `:::kind`. Rhombus implies `decision`; an explicit
/// gate kind overrides the shape default.
fn parse_shape(raw: &str, line_no: usize) -> Result<ParsedShape, VivariumError> {
    let raw = raw.trim();
    let (id, rest) = split_id(raw, line_no)?;
    let rest = rest.trim_start();
    if rest.is_empty() {
        return Ok(ParsedShape {
            id,
            label: None,
            kind: None,
            class: None,
        });
    }
    if let Some(after) = rest.strip_prefix("([") {
        let (label, trailing) = parse_stadium_label(after, line_no)?;
        return finish_shape(id, Some(label), &trailing, line_no);
    }
    if let Some(after) = rest.strip_prefix('[') {
        let (label, trailing) = parse_delimited_label(after, ']', line_no)?;
        return finish_shape(id, Some(label), &trailing, line_no);
    }
    if let Some(after) = rest.strip_prefix('{') {
        let (label, trailing) = parse_delimited_label(after, '}', line_no)?;
        let mut shape = finish_shape(id, Some(label), &trailing, line_no)?;
        let kind = shape.kind.take().or_else(|| Some("decision".into()));
        shape.kind = kind;
        return Ok(shape);
    }
    Err(parse_err(
        line_no,
        &format!("unexpected text '{rest}' after node id '{id}'; {SUBSET_HINT}"),
    ))
}

fn finish_shape(
    id: String,
    label: Option<String>,
    trailing: &str,
    line_no: usize,
) -> Result<ParsedShape, VivariumError> {
    let trailing = trailing.trim();
    let (kind, class) = if trailing.is_empty() {
        (None, None)
    } else if let Some(class) = trailing.strip_prefix(":::") {
        let class = class.trim();
        (
            is_gate_kind(class).then(|| class.to_string()),
            (!class.is_empty()).then(|| class.to_string()),
        )
    } else {
        return Err(parse_err(
            line_no,
            &format!("unexpected text '{trailing}' after node shape; {SUBSET_HINT}"),
        ));
    };
    Ok(ParsedShape {
        id,
        label,
        kind,
        class,
    })
}

fn split_id(raw: &str, line_no: usize) -> Result<(String, &str), VivariumError> {
    let end = raw
        .char_indices()
        .find(|(_, c)| !is_id_char(*c))
        .map_or(raw.len(), |(idx, _)| idx);
    if end == 0 {
        return Err(parse_err(line_no, &format!("missing node id in '{raw}'")));
    }
    Ok((raw[..end].to_string(), &raw[end..]))
}

fn parse_stadium_label<'a>(
    after_open: &'a str,
    line_no: usize,
) -> Result<(String, &'a str), VivariumError> {
    if let Some(inner) = after_open.strip_prefix('"') {
        let end = inner
            .find('"')
            .ok_or_else(|| parse_err(line_no, "unclosed quoted node label"))?;
        let label = inner[..end].to_string();
        let rest = inner[end + 1..].trim_start();
        if !rest.starts_with("])") {
            return Err(parse_err(
                line_no,
                "expected '])' after quoted stadium label",
            ));
        }
        return Ok((label, &rest[2..]));
    }
    let end = after_open
        .find("])")
        .ok_or_else(|| parse_err(line_no, "unclosed stadium label (expected '])'"))?;
    let label = after_open[..end].trim().to_string();
    Ok((label, &after_open[end + 2..]))
}

fn parse_delimited_label<'a>(
    after_open: &'a str,
    close: char,
    line_no: usize,
) -> Result<(String, &'a str), VivariumError> {
    if let Some(inner) = after_open.strip_prefix('"') {
        let end = inner
            .find('"')
            .ok_or_else(|| parse_err(line_no, "unclosed quoted node label"))?;
        let label = inner[..end].to_string();
        let rest = inner[end + 1..].trim_start();
        if !rest.starts_with(close) {
            return Err(parse_err(
                line_no,
                &format!("expected '{close}' after quoted node label"),
            ));
        }
        return Ok((label, &rest[close.len_utf8()..]));
    }
    let end = after_open
        .find(close)
        .ok_or_else(|| parse_err(line_no, "unclosed node label"))?;
    let label = after_open[..end].trim().to_string();
    Ok((label, &after_open[end + close.len_utf8()..]))
}

fn ensure_node(
    nodes: &mut HashMap<String, MermaidNode>,
    source_id: &str,
    label: &str,
    subgraph: Option<String>,
    kind: Option<&str>,
    class: Option<&str>,
) {
    nodes
        .entry(source_id.to_string())
        .and_modify(|existing| {
            if existing.label == existing.source_id && label != source_id {
                existing.label = label.to_string();
            }
            if existing.subgraph.is_none() {
                existing.subgraph.clone_from(&subgraph);
            }
            if existing.kind == "task" && kind.is_some_and(|k| is_gate_kind(k)) {
                existing.kind = kind.unwrap_or("task").to_string();
            }
            if existing.class.is_none() {
                existing.class = class.map(str::to_string);
            }
        })
        .or_insert_with(|| MermaidNode {
            source_id: source_id.to_string(),
            label: label.to_string(),
            subgraph,
            kind: kind.unwrap_or("task").to_string(),
            class: class.map(str::to_string),
        });
}

fn current_subgraph(stack: &[String]) -> Option<String> {
    stack.last().cloned()
}

fn validate_id(id: &str, line_no: usize) -> Result<(), VivariumError> {
    if id.is_empty() {
        return Err(parse_err(line_no, "empty node id"));
    }
    if !id.chars().all(is_id_char) {
        return Err(parse_err(
            line_no,
            &format!("invalid node id '{id}' (use [A-Za-z0-9_-]+); {SUBSET_HINT}"),
        ));
    }
    Ok(())
}

fn is_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn is_styling_line(line: &str) -> bool {
    line.starts_with("classDef") || line.starts_with("style ")
}

fn validate_endpoints(nodes: &[MermaidNode], edges: &[MermaidEdge]) -> Result<(), VivariumError> {
    let ids: HashSet<&str> = nodes.iter().map(|n| n.source_id.as_str()).collect();
    for edge in edges {
        if !ids.contains(edge.from.as_str()) {
            return Err(parse_err(
                0,
                &format!("edge references missing node '{}'", edge.from),
            ));
        }
        if !ids.contains(edge.to.as_str()) {
            return Err(parse_err(
                0,
                &format!("edge references missing node '{}'", edge.to),
            ));
        }
    }
    Ok(())
}

fn detect_cycle(nodes: &[MermaidNode], edges: &[MermaidEdge]) -> Result<(), VivariumError> {
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for node in nodes {
        adj.entry(node.source_id.as_str()).or_default();
    }
    for edge in edges {
        adj.entry(edge.from.as_str())
            .or_default()
            .push(edge.to.as_str());
    }
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for node in nodes {
        if dfs_cycle(node.source_id.as_str(), &adj, &mut visiting, &mut visited)? {
            return Err(parse_err(0, "cycle detected in flowchart"));
        }
    }
    Ok(())
}

fn dfs_cycle<'a>(
    node: &'a str,
    adj: &HashMap<&'a str, Vec<&'a str>>,
    visiting: &mut HashSet<&'a str>,
    visited: &mut HashSet<&'a str>,
) -> Result<bool, VivariumError> {
    if visited.contains(node) {
        return Ok(false);
    }
    if !visiting.insert(node) {
        return Ok(true);
    }
    if let Some(nexts) = adj.get(node) {
        for next in nexts {
            if dfs_cycle(next, adj, visiting, visited)? {
                return Ok(true);
            }
        }
    }
    visiting.remove(&node);
    visited.insert(node);
    Ok(false)
}

fn strip_comment(line: &str) -> &str {
    if let Some(idx) = line.find("%%") {
        &line[..idx]
    } else {
        line
    }
}

fn parse_err(line_no: usize, msg: &str) -> VivariumError {
    if line_no == 0 {
        VivariumError::Parse(format!("mermaid: {msg}"))
    } else {
        VivariumError::Parse(format!("mermaid line {line_no}: {msg}"))
    }
}

#[cfg(test)]
#[path = "mermaid_test.rs"]
mod tests;
