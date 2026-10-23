use super::*;

const FANOUT: &str = r#"
flowchart LR
  subgraph mir["mir-swarm"]
    verify["G-P-10/U1/reverify"]
    accept["G-P-10/U1/accept"]
    u2["G-P-10/U2"]
    u3["G-P-10/U3"]
    verify --> accept
    accept --> u2
    accept --> u3
  end
"#;

#[test]
fn parses_fanout_fixture() {
    let g = parse_flowchart(FANOUT).unwrap();
    assert_eq!(g.direction, "LR");
    assert_eq!(g.nodes.len(), 4);
    assert_eq!(g.edges.len(), 3);
    let verify = g.nodes.iter().find(|n| n.source_id == "verify").unwrap();
    assert_eq!(verify.label, "G-P-10/U1/reverify");
    assert_eq!(verify.subgraph.as_deref(), Some("mir"));
}

#[test]
fn rejects_cycle() {
    let src = "flowchart TD\na --> b\nb --> a\n";
    let err = parse_flowchart(src).unwrap_err().to_string();
    assert!(err.contains("cycle"), "{err}");
}

#[test]
fn rejects_unsupported_diagram() {
    let err = parse_flowchart("sequenceDiagram\nA->>B: hi\n")
        .unwrap_err()
        .to_string();
    assert!(err.contains("unsupported"), "{err}");
}

#[test]
fn parses_chained_edges() {
    let g = parse_flowchart("flowchart TD\na --> b --> c\n").unwrap();
    assert_eq!(g.edges.len(), 2);
    assert!(g.edges.iter().any(|e| e.from == "a" && e.to == "b"));
    assert!(g.edges.iter().any(|e| e.from == "b" && e.to == "c"));
}

#[test]
fn parses_edge_label() {
    let g = parse_flowchart("flowchart LR\na -->|gate| b\n").unwrap();
    assert_eq!(g.edges[0].label.as_deref(), Some("gate"));
}

#[test]
fn parses_rhombus_as_decision_kind() {
    let g = parse_flowchart("flowchart TD\nreview{\"Operator ruling\"}\na --> review\n").unwrap();
    let node = g.nodes.iter().find(|n| n.source_id == "review").unwrap();
    assert_eq!(node.kind, "decision");
    assert_eq!(node.label, "Operator ruling");
}

#[test]
fn explicit_kind_overrides_rhombus_default() {
    let g = parse_flowchart("flowchart TD\nd{gate}:::parked\n").unwrap();
    let node = g.nodes.iter().find(|n| n.source_id == "d").unwrap();
    assert_eq!(node.kind, "parked");
}

#[test]
fn parses_stadium_as_work_node() {
    let g = parse_flowchart("flowchart TD\nstart([kick off]) --> a[work]\n").unwrap();
    let node = g.nodes.iter().find(|n| n.source_id == "start").unwrap();
    assert_eq!(node.kind, "task");
    assert_eq!(node.label, "kick off");
}

#[test]
fn class_suffix_marks_gate_kinds() {
    let g = parse_flowchart(
        "flowchart TD\nres_provision[\"provision index\"]:::stub\nfollow[\"follow-on\"]:::parked\n",
    )
    .unwrap();
    let stub = g
        .nodes
        .iter()
        .find(|n| n.source_id == "res_provision")
        .unwrap();
    assert_eq!(stub.kind, "stub");
    let parked = g.nodes.iter().find(|n| n.source_id == "follow").unwrap();
    assert_eq!(parked.kind, "parked");
}

#[test]
fn class_suffix_with_unknown_name_is_styling() {
    let g = parse_flowchart("flowchart TD\na[work]:::urgent\n").unwrap();
    assert_eq!(g.nodes[0].kind, "task");
}

#[test]
fn class_statement_applies_gate_kind_regardless_of_order() {
    let g = parse_flowchart(
        "flowchart TD\nclass dec40, dec44 decision\na[work] --> dec40\nclass a done\ndec44{second}\n",
    )
    .unwrap();
    let dec40 = g.nodes.iter().find(|n| n.source_id == "dec40").unwrap();
    assert_eq!(dec40.kind, "decision");
    let dec44 = g.nodes.iter().find(|n| n.source_id == "dec44").unwrap();
    assert_eq!(dec44.kind, "decision");
    let work = g.nodes.iter().find(|n| n.source_id == "a").unwrap();
    assert_eq!(work.kind, "task");
}

#[test]
fn ignores_class_def_and_style_lines() {
    let src = "flowchart TD\nclassDef done fill:#cfc,stroke:#393;\nstyle a fill:#fcc\na[work]\n";
    let g = parse_flowchart(src).unwrap();
    assert_eq!(g.nodes.len(), 1);
    assert_eq!(g.nodes[0].source_id, "a");
}

#[test]
fn parses_dotted_edges_as_couplings() {
    let g = parse_flowchart("flowchart LR\na -.-> b\nb -.- c\na --> d\n").unwrap();
    let dotted_arrow = g
        .edges
        .iter()
        .find(|e| e.from == "a" && e.to == "b")
        .unwrap();
    assert_eq!(dotted_arrow.style, "dotted");
    let dotted_bare = g
        .edges
        .iter()
        .find(|e| e.from == "b" && e.to == "c")
        .unwrap();
    assert_eq!(dotted_bare.style, "dotted");
    let solid = g
        .edges
        .iter()
        .find(|e| e.from == "a" && e.to == "d")
        .unwrap();
    assert_eq!(solid.style, "solid");
}

#[test]
fn parses_dotted_edge_labels() {
    let g = parse_flowchart("flowchart LR\na -.->|relates to| b\n").unwrap();
    assert_eq!(g.edges[0].label.as_deref(), Some("relates to"));
    assert_eq!(g.edges[0].style, "dotted");
}

#[test]
fn mixed_arrow_chain_keeps_per_edge_style() {
    let g = parse_flowchart("flowchart LR\na --> b -.-> c\n").unwrap();
    let first = g
        .edges
        .iter()
        .find(|e| e.from == "a" && e.to == "b")
        .unwrap();
    assert_eq!(first.style, "solid");
    let second = g
        .edges
        .iter()
        .find(|e| e.from == "b" && e.to == "c")
        .unwrap();
    assert_eq!(second.style, "dotted");
}

#[test]
fn unsupported_construct_error_names_the_subset() {
    let err = parse_flowchart("flowchart TD\na == b\n")
        .unwrap_err()
        .to_string();
    assert!(err.contains("line 2"), "{err}");
    assert!(err.contains("accepted subset"), "{err}");

    let err = parse_flowchart("flowchart TD\ntrapezoid{{widest}}\n")
        .unwrap_err()
        .to_string();
    assert!(err.contains("unexpected text"), "{err}");
    assert!(err.contains("accepted subset"), "{err}");
}

#[test]
fn gate_kind_check_covers_declared_kinds() {
    assert!(is_gate_kind("decision"));
    assert!(is_gate_kind("stub"));
    assert!(is_gate_kind("parked"));
    assert!(!is_gate_kind("task"));
    assert!(!is_gate_kind("urgent"));
}

#[test]
fn rhombus_join_class_keeps_decision_kind_and_class() {
    let g = parse_flowchart("flowchart TD\nj{all done}:::join\nj --> z\n").unwrap();
    let j = g.nodes.iter().find(|n| n.source_id == "j").unwrap();
    assert_eq!(j.kind, "decision");
    assert_eq!(j.class.as_deref(), Some("join"));
}

#[test]
fn hyphenated_classes_are_kept_inline_and_on_class_lines() {
    let src = "flowchart TD\n\
        a[need]:::rung-need --> b[plan]\n\
        b --> c[redo]\n\
        class c restart-plan\n";
    let g = parse_flowchart(src).unwrap();
    let find = |id: &str| g.nodes.iter().find(|n| n.source_id == id).unwrap();
    assert_eq!(find("a").class.as_deref(), Some("rung-need"));
    assert_eq!(find("a").kind, "task");
    assert_eq!(find("b").class, None);
    assert_eq!(find("c").class.as_deref(), Some("restart-plan"));
    assert_eq!(find("c").kind, "task");
}

#[test]
fn gate_class_sets_kind_and_class_on_class_lines() {
    let g = parse_flowchart("flowchart TD\na --> b\nclass b stub\n").unwrap();
    let b = g.nodes.iter().find(|n| n.source_id == "b").unwrap();
    assert_eq!(b.kind, "stub");
    assert_eq!(b.class.as_deref(), Some("stub"));
}

#[test]
fn classless_chart_serializes_without_class_field() {
    let g = parse_flowchart("flowchart TD\na[one] --> b{two}\n").unwrap();
    let json = serde_json::to_string(&g).unwrap();
    assert!(!json.contains("class"), "{json}");
    assert!(json.contains(r#""kind":"decision""#), "{json}");
}
