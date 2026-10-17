//! Executable work-graph CLI definitions (Mermaid topology, frontier
//! receipts, backlog citizenship audit and repair).

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Executable work-graph commands (Mermaid topology + compact receipts).
#[derive(Debug, Subcommand)]
pub enum GraphCommand {
    /// Import a Mermaid flowchart as a work graph
    Import(GraphImportCommand),
    /// Apply a Mermaid revision onto an existing work graph
    Apply(GraphApplyCommand),
    /// Show topology as Mermaid (not JSON)
    Show(GraphShowCommand),
    /// Export a work graph as Mermaid
    Export(GraphExportCommand),
    /// Show ready/blocked/active frontier (status loops; not topology)
    Ready(GraphReadyCommand),
    /// List blocked nodes with the prerequisites still unfinished
    Blocked(GraphBlockedCommand),
    /// Audit backlog citizenship; optionally repair drift
    Audit(GraphAuditCommand),
    /// Add a post-hoc prerequisite between two backlog items
    Connect(GraphConnectCommand),
    /// Mark a graph node done
    Complete(GraphCompleteCommand),
    /// Bind a task attempt and mark a ready node active
    Activate(GraphActivateCommand),
    /// Graph node subcommands
    Node {
        #[command(subcommand)]
        command: GraphNodeCommand,
    },
    /// Graph edge subcommands
    Edge {
        #[command(subcommand)]
        command: GraphEdgeCommand,
    },
}

/// Import a Mermaid flowchart into the project mailspace.
///
/// Accepted subset: `flowchart`/`graph` header with direction TD|TB|BT|RL|LR;
/// nodes as `id`, `id[label]`, `id{label}` (decision), `id([label])`
/// (stadium), each with an optional `id:::kind` suffix (decision, stub, or
/// parked); edges `-->` (prerequisite) and `-.->` / `-.-` (dotted couplings
/// that never gate readiness), optionally labeled `-->|text|`;
/// `subgraph id ... end`; `%%` comments. `classDef` and `style` lines are
/// ignored; `class a,b <kind>` sets gate kinds. Cycles are rejected.
/// Imported graphs are never adjudicated by `vivi step`, which reads only
/// the backlog graph.
#[derive(Debug, Clone, Parser)]
pub struct GraphImportCommand {
    /// Project-unique graph code
    #[arg(long)]
    pub code: String,

    /// Path to a Mermaid flowchart file
    #[arg(long)]
    pub file: PathBuf,

    /// Validate and preview without writing
    #[arg(long)]
    pub check: bool,

    /// Output compact JSON receipt (not full topology)
    #[arg(long)]
    pub json: bool,

    /// Allow large JSON stdout (over 64 KiB)
    #[arg(long = "confirm-large")]
    pub confirm_large: bool,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

/// Apply a Mermaid revision to an existing graph.
///
/// Uses the same Mermaid subset as `graph import` (see its help for the
/// full syntax summary).
#[derive(Debug, Clone, Parser)]
pub struct GraphApplyCommand {
    /// Graph code or handle
    pub graph: String,

    /// Path to a Mermaid flowchart file
    #[arg(long)]
    pub file: PathBuf,

    /// Validate and preview without writing
    #[arg(long)]
    pub check: bool,

    /// Output compact JSON receipt (not full topology)
    #[arg(long)]
    pub json: bool,

    /// Allow large JSON stdout (over 64 KiB)
    #[arg(long = "confirm-large")]
    pub confirm_large: bool,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

/// Show a stored work graph as Mermaid topology.
///
/// Topology is always Mermaid. Use `graph ready` for status/frontier loops.
#[derive(Debug, Clone, Parser)]
pub struct GraphShowCommand {
    /// Graph code or immutable handle
    pub graph: String,

    /// Include readiness/state styling classes
    #[arg(long)]
    pub include_state: bool,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

/// Show ready / blocked / active frontier for status loops.
#[derive(Debug, Clone, Parser)]
pub struct GraphReadyCommand {
    /// Graph code or handle; omit to list all graphs
    pub graph: Option<String>,

    /// Keep only nodes of this kind (task, need, want, decision, stub, parked)
    #[arg(long)]
    pub kind: Option<String>,

    /// Output compact JSON frontier
    #[arg(long)]
    pub json: bool,

    /// Allow large JSON stdout (over 64 KiB)
    #[arg(long = "confirm-large")]
    pub confirm_large: bool,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

/// List blocked nodes: the prerequisites each one is still waiting on.
#[derive(Debug, Clone, Parser)]
pub struct GraphBlockedCommand {
    /// Graph code or handle; defaults to the backlog graph
    #[arg(default_value = "backlog")]
    pub graph: String,

    /// Keep only nodes of this kind (task, need, want, decision, stub, parked)
    #[arg(long)]
    pub kind: Option<String>,

    /// Output compact JSON rows
    #[arg(long)]
    pub json: bool,

    /// Allow large JSON stdout (over 64 KiB)
    #[arg(long = "confirm-large")]
    pub confirm_large: bool,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

/// Audit backlog citizenship: every work-kind item sent while the backlog
/// graph existed must have a node in step with its folder state. Repair
/// mints missing nodes (from folder state and dependency headers),
/// completes nodes for done items, settles need mailboxes whose join fired,
/// and corrects node kinds minted before kinds were persisted.
#[derive(Debug, Clone, Parser)]
pub struct GraphAuditCommand {
    /// Repair what the audit can fix; without it the audit is read-only
    #[arg(long)]
    pub repair: bool,

    /// Output findings as JSON
    #[arg(long)]
    pub json: bool,

    /// Allow large JSON stdout (over 64 KiB)
    #[arg(long = "confirm-large")]
    pub confirm_large: bool,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

/// Add a prerequisite edge discovered after filing: the dependent stops
/// being ready until the prerequisite completes. Backlog items only; both
/// handles must already have graph nodes.
#[derive(Debug, Clone, Parser)]
pub struct GraphConnectCommand {
    /// Dependent item handle (the item that becomes blocked)
    pub dependent: String,

    /// Prerequisite item handle (must complete first)
    pub prereq: String,

    /// Optional edge label
    #[arg(long)]
    pub label: Option<String>,

    /// Output compact JSON receipt
    #[arg(long)]
    pub json: bool,

    /// Allow large JSON stdout (over 64 KiB)
    #[arg(long = "confirm-large")]
    pub confirm_large: bool,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

/// Export a work graph as Mermaid.
#[derive(Debug, Clone, Parser)]
pub struct GraphExportCommand {
    /// Graph code or immutable handle
    pub graph: String,

    /// Include state styling classes
    #[arg(long)]
    pub include_state: bool,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

/// Complete (mark done) a graph node.
#[derive(Debug, Clone, Parser)]
pub struct GraphCompleteCommand {
    /// Node as `graph:source-id` or source-id with --graph
    pub node: String,

    /// Graph code when `node` is only a source id
    #[arg(long)]
    pub graph: Option<String>,

    /// Optional completion note
    #[arg(long)]
    pub note: Option<String>,

    /// Ignored; task binding happens at `graph activate`
    #[arg(long)]
    pub task: Option<String>,

    /// Output compact JSON receipt (not full topology)
    #[arg(long)]
    pub json: bool,

    /// Allow large JSON stdout (over 64 KiB)
    #[arg(long = "confirm-large")]
    pub confirm_large: bool,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

/// Activate a ready graph node with a bound task attempt.
#[derive(Debug, Clone, Parser)]
pub struct GraphActivateCommand {
    /// Node as `graph:source-id` or source-id with --graph
    pub node: String,

    /// Graph code when `node` is only a source id
    #[arg(long)]
    pub graph: Option<String>,

    /// Task handle to bind as this attempt
    #[arg(long)]
    pub task: String,

    /// Optional activation note
    #[arg(long)]
    pub note: Option<String>,

    /// Output compact JSON receipt (not full topology)
    #[arg(long)]
    pub json: bool,

    /// Allow large JSON stdout (over 64 KiB)
    #[arg(long = "confirm-large")]
    pub confirm_large: bool,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

/// Node mutations.
#[derive(Debug, Subcommand)]
pub enum GraphNodeCommand {
    /// Add an open node
    Add(GraphNodeAddCommand),
}

/// Edge mutations.
#[derive(Debug, Subcommand)]
pub enum GraphEdgeCommand {
    /// Add a dependency edge
    Add(GraphEdgeAddCommand),
}

/// Add one open node to a graph.
#[derive(Debug, Clone, Parser)]
pub struct GraphNodeAddCommand {
    /// Graph code or handle
    #[arg(long)]
    pub graph: String,

    /// Mermaid source id
    #[arg(long)]
    pub id: String,

    /// Display label (defaults to id)
    #[arg(long)]
    pub label: Option<String>,

    /// Node kind: task (default), decision, stub, or parked; gated kinds
    /// never dispatch and are resolved with `graph complete`
    #[arg(long)]
    pub kind: Option<String>,

    /// Output compact JSON receipt (not full topology)
    #[arg(long)]
    pub json: bool,

    /// Allow large JSON stdout (over 64 KiB)
    #[arg(long = "confirm-large")]
    pub confirm_large: bool,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

/// Add one dependency edge.
#[derive(Debug, Clone, Parser)]
pub struct GraphEdgeAddCommand {
    /// Graph code or handle
    #[arg(long)]
    pub graph: String,

    /// Prerequisite source id
    #[arg(long = "from")]
    pub from: String,

    /// Dependent source id
    #[arg(long = "to")]
    pub to: String,

    /// Optional edge label
    #[arg(long)]
    pub label: Option<String>,

    /// Output compact JSON receipt (not full topology)
    #[arg(long)]
    pub json: bool,

    /// Allow large JSON stdout (over 64 KiB)
    #[arg(long = "confirm-large")]
    pub confirm_large: bool,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}
