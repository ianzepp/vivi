//! Dependency edges of held work letters.
//!
//! A work letter names its prerequisites in repeated `X-Vivi-Depends-On`
//! headers, so the order of work is a fact of the letters, not of the route
//! that filed them. Every route that attaches a work letter's node (local
//! send, pull, fetch, resync, raw delivery `vivi mail deliver`, reference
//! follow, audit repair, role adoption) goes through
//! [`Mailspace::backlog_attach_letter`], which mints the node and
//! one prerequisite edge for each dependency this mailspace holds as backlog
//! work.
//!
//! A dependency not held yet is recorded as pending in the store itself (not
//! in sync state: a mailspace with no `[sync]` files letters too) and its
//! edge is made when that letter is attached. A reference that can never name
//! work here (foreign-held, plain mail, not a handle) makes no edge and
//! blocks nothing. `graph audit` reads the same headers, so a drifted graph is
//! reported and repaired from the letters alone.

use std::collections::{HashMap, HashSet};

use super::Mailspace;
use super::backlog::{BacklogDep, backlog_node_by_token, resolve_backlog_dep};
use super::holding::is_foreign_account;
use crate::error::VivariumError;
use crate::storage::{
    BACKLOG_GRAPH_CODE, BacklogMintInput, BacklogNodeInput, Storage, StoredMessage,
    WorkGraphEdgeInput, WorkGraphEdgeRow, WorkGraphNodeRow,
};

const DEPENDS_ON_HEADER: &str = "X-Vivi-Depends-On";

/// A letter written to the store: `(message id, content id)`.
pub type ArrivedLetter = (String, String);

/// What a dependency reference names in this mailspace.
pub(super) enum DepLookup {
    /// A backlog item held here.
    Held(BacklogDep),
    /// Not held yet, or held for a role this mailspace does not have: the
    /// edge waits for the letter.
    Pending,
    /// Can never order work here.
    Never,
}

/// A held, open work letter whose header names a held item the graph has no
/// edge from.
pub(super) struct MissingEdge {
    pub(super) message_id: String,
    pub(super) handle: String,
    pub(super) reference: String,
    pub(super) dependency: String,
}

/// The dependency references a letter's bytes name: trimmed, in header
/// order, without duplicates.
pub(crate) fn depends_on_refs(bytes: &[u8]) -> Vec<String> {
    let Some(parsed) = mail_parser::MessageParser::default().parse(bytes) else {
        return Vec::new();
    };
    let mut refs: Vec<String> = Vec::new();
    for header in parsed.headers() {
        if !header.name().eq_ignore_ascii_case(DEPENDS_ON_HEADER) {
            continue;
        }
        if let mail_parser::HeaderValue::Text(text) = header.value() {
            let value = text.trim().to_string();
            if !value.is_empty() && !refs.contains(&value) {
                refs.push(value);
            }
        }
    }
    refs
}

/// Whether a reference could ever name a held letter: hex digits, with an
/// optional `msg_` prefix.
fn looks_like_reference(token: &str) -> bool {
    let hex = token.strip_prefix("msg_").unwrap_or(token);
    (4..=64).contains(&hex.len()) && hex.chars().all(|c| c.is_ascii_hexdigit())
}

/// Whether `reference` names the arrived letter, as a message id prefix, a
/// handle, or a content id prefix.
fn names_arrival(reference: &str, arrived: &ArrivedLetter) -> bool {
    let reference = reference.to_ascii_lowercase();
    let wanted = reference.strip_prefix("msg_").unwrap_or(&reference);
    let message = arrived.0.to_ascii_lowercase();
    let message = message.strip_prefix("msg_").unwrap_or(&message);
    message.starts_with(wanted) || arrived.1.to_ascii_lowercase().starts_with(wanted)
}

/// Whether following `edges` from node handle `from` reaches `to`.
fn reaches(edges: &[WorkGraphEdgeRow], from: &str, to: &str) -> bool {
    let mut next: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in edges {
        next.entry(edge.from_node.as_str())
            .or_default()
            .push(edge.to_node.as_str());
    }
    let mut seen: HashSet<&str> = HashSet::new();
    let mut stack = vec![from];
    while let Some(node) = stack.pop() {
        if node == to {
            return true;
        }
        if seen.insert(node) {
            stack.extend(next.get(node).into_iter().flatten());
        }
    }
    false
}

/// The nodes and edges one letter's headers ask for.
#[derive(Default)]
struct LinkPlan {
    input: Option<BacklogMintInput>,
    pending: Vec<String>,
}

impl Mailspace {
    /// Attach the backlog node of a work letter this mailspace holds and
    /// build the prerequisite edges its `X-Vivi-Depends-On` headers name.
    /// Idempotent. A dependency that is not held yet waits (see the module
    /// docs); the caller settles waiting edges with
    /// [`Mailspace::backlog_settle_pending_dependencies`] once the letter's
    /// own rows are all filed.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on an invalid handle or storage failure.
    pub fn backlog_attach_letter(
        &self,
        kind: &str,
        message_id: &str,
        subject: &str,
        letter: &[u8],
    ) -> Result<(), VivariumError> {
        let handle = self.storage()?.display_handle(message_id)?;
        self.backlog_attach(kind, &handle, subject, &[])?;
        let refs = depends_on_refs(letter);
        if refs.is_empty() {
            return Ok(());
        }
        self.link_dependencies(message_id, &refs).map(|_| ())
    }

    /// Attach the backlog nodes of a raw delivery's stored rows. A work
    /// letter mints its node and prerequisite edges at ingest, exactly as a
    /// pulled letter does; plain mail and event letters mint none. Edges whose
    /// dependency names a letter that only now arrived are settled at the same
    /// time.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on an invalid handle or storage failure.
    pub(super) fn backlog_attach_raw_delivery(
        &self,
        stored: &[StoredMessage],
        kind: Option<&str>,
        subject: &str,
        letter: &[u8],
    ) -> Result<(), VivariumError> {
        let mut arrived: Vec<ArrivedLetter> = Vec::new();
        for msg in stored {
            if let Some(kind) = kind {
                self.backlog_attach_letter(kind, &msg.message_id, subject, letter)?;
            }
            arrived.push((msg.message_id.clone(), msg.content_id.clone()));
        }
        self.backlog_settle_pending_dependencies(&arrived)?;
        Ok(())
    }

    /// Make the edges that were waiting for letters that just arrived.
    /// Returns how many edges were made.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure.
    pub fn backlog_settle_pending_dependencies(
        &self,
        arrived: &[ArrivedLetter],
    ) -> Result<usize, VivariumError> {
        if arrived.is_empty() {
            return Ok(0);
        }
        let pending = self.storage()?.pending_dependencies()?;
        let mut made = 0;
        for (message_id, refs) in pending {
            let named = refs
                .iter()
                .any(|reference| arrived.iter().any(|a| names_arrival(reference, a)));
            if named {
                made += self.link_dependencies(&message_id, &refs)?;
            }
        }
        Ok(made)
    }

    /// Build the edges for `refs` onto the node of `message_id`, record what
    /// is still pending, and return how many edges were made. A letter with
    /// no backlog node (held foreign or for an unknown role) orders nothing.
    pub(super) fn link_dependencies(
        &self,
        message_id: &str,
        refs: &[String],
    ) -> Result<usize, VivariumError> {
        let storage = self.storage()?;
        let handle = storage.display_handle(message_id)?;
        let Some(graph) = storage.work_graph_by_code(BACKLOG_GRAPH_CODE)? else {
            return Ok(0);
        };
        let nodes = storage.work_graph_nodes(&graph.handle)?;
        let edges = storage.work_graph_edges(&graph.handle)?;
        let Some(dependent) = backlog_node_by_token(&storage, &nodes, &handle)? else {
            storage.set_pending_dependencies(message_id, &[])?;
            return Ok(0);
        };
        let plan = self.plan_links(&storage, &nodes, &edges, &dependent, refs)?;
        let made = match plan.input {
            Some(input) => storage.mint_backlog(&input)?.minted_edges,
            None => 0,
        };
        storage.set_pending_dependencies(message_id, &plan.pending)?;
        Ok(made)
    }

    fn plan_links(
        &self,
        storage: &Storage,
        nodes: &[WorkGraphNodeRow],
        edges: &[WorkGraphEdgeRow],
        dependent: &WorkGraphNodeRow,
        refs: &[String],
    ) -> Result<LinkPlan, VivariumError> {
        let mut plan = LinkPlan::default();
        for token in refs {
            match self.lookup_dependency(storage, token)? {
                DepLookup::Pending => plan.pending.push(token.clone()),
                DepLookup::Never => {}
                DepLookup::Held(dep) => {
                    let existing = backlog_node_by_token(storage, nodes, &dep.handle)?;
                    add_edge(&mut plan, edges, dependent, &dep, existing.as_ref());
                }
            }
        }
        Ok(plan)
    }

    /// What a dependency reference names here.
    pub(super) fn lookup_dependency(
        &self,
        storage: &Storage,
        token: &str,
    ) -> Result<DepLookup, VivariumError> {
        let Ok(message_id) = storage.resolve_message_token(token) else {
            return Ok(if looks_like_reference(token) {
                DepLookup::Pending
            } else {
                DepLookup::Never
            });
        };
        let Some(view) = storage.message_by_id(&message_id)? else {
            return Ok(DepLookup::Pending);
        };
        if is_foreign_account(&view.account) {
            return Ok(DepLookup::Never);
        }
        if self.is_unowned_account(&view.account) {
            return Ok(DepLookup::Pending);
        }
        Ok(match resolve_backlog_dep(storage, token) {
            Ok(dep) => DepLookup::Held(dep),
            Err(_) => DepLookup::Never,
        })
    }

    /// Whether `token` names held work that is not done: the one rule
    /// `task list --blocked` and the graph share.
    pub(super) fn dependency_is_unmet(&self, token: &str) -> Result<bool, VivariumError> {
        let storage = self.storage()?;
        Ok(matches!(
            self.lookup_dependency(&storage, token)?,
            DepLookup::Held(dep) if dep.state != "done"
        ))
    }

    /// The edges the copies of one letter should have and do not: open nodes
    /// whose header names held backlog work with no edge from it.
    pub(super) fn missing_edges(
        &self,
        storage: &Storage,
        nodes: &[WorkGraphNodeRow],
        edges: &[WorkGraphEdgeRow],
        content_id: &str,
    ) -> Result<Vec<MissingEdge>, VivariumError> {
        let mut missing = Vec::new();
        for message_id in storage.message_ids_by_content(content_id)? {
            let refs = depends_on_refs(&storage.read_message(&message_id)?);
            if refs.is_empty() {
                continue;
            }
            let handle = storage.display_handle(&message_id)?;
            let Some(node) = backlog_node_by_token(storage, nodes, &handle)? else {
                continue;
            };
            if node.state != "open" {
                continue;
            }
            for reference in refs {
                let DepLookup::Held(dep) = self.lookup_dependency(storage, &reference)? else {
                    continue;
                };
                let prereq = backlog_node_by_token(storage, nodes, &dep.handle)?;
                let linked = prereq.as_ref().is_some_and(|p| {
                    p.handle == node.handle
                        || edges
                            .iter()
                            .any(|e| e.from_node == p.handle && e.to_node == node.handle)
                });
                if !linked {
                    missing.push(MissingEdge {
                        message_id: message_id.clone(),
                        handle: handle.clone(),
                        reference,
                        dependency: dep.handle,
                    });
                }
            }
        }
        Ok(missing)
    }
}

/// Add the edge `dep -> dependent` to the plan unless it would order a node
/// after itself or close a cycle.
fn add_edge(
    plan: &mut LinkPlan,
    edges: &[WorkGraphEdgeRow],
    dependent: &WorkGraphNodeRow,
    dep: &BacklogDep,
    existing: Option<&WorkGraphNodeRow>,
) {
    if let Some(node) = existing
        && (node.handle == dependent.handle || reaches(edges, &dependent.handle, &node.handle))
    {
        return;
    }
    let input = plan.input.get_or_insert_with(|| BacklogMintInput {
        nodes: Vec::new(),
        edges: Vec::new(),
    });
    let from_source_id = match existing {
        Some(node) => node.source_id.clone(),
        None => {
            input.nodes.push(BacklogNodeInput {
                source_id: dep.handle.clone(),
                label: dep.subject.clone(),
                state: dep.state.into(),
                kind: dep.kind.into(),
            });
            dep.handle.clone()
        }
    };
    input.edges.push(WorkGraphEdgeInput {
        from_source_id,
        to_source_id: dependent.source_id.clone(),
        label: None,
        style: "solid".into(),
    });
}

#[cfg(test)]
#[path = "dependencies_test.rs"]
mod tests;
