//! Dependency edges of held work letters.
//!
//! A work letter names its prerequisites in the `depends_on` front matter key
//! of a format-1 letter, so the order of work is a fact of the letters, not of
//! the route that filed them. Every route that attaches a work letter's node
//! (local send, pull, fetch, resync, raw delivery `vivi mail deliver`,
//! reference follow, audit repair, role adoption, `mailspace import`) goes
//! through [`Mailspace::backlog_attach_letter`], which mints the node and
//! one prerequisite edge for each dependency this mailspace holds as backlog
//! work.
//!
//! A dependency not held yet is recorded as pending in the store itself (not
//! in sync state: a mailspace with no `[sync]` files letters too) and its
//! edge is made when that letter is attached. A reference that can never name
//! work here (foreign-held, plain mail, not a handle) is recorded the same
//! way: unresolved references are remembered and block readiness until the
//! named letter is present and done. Only a held reference mints an edge.
//! `graph audit` reads the same front matter, so a drifted graph is reported
//! and repaired from the letters alone.

use std::collections::{HashMap, HashSet};

use mail_parser::MessageParser;

use super::Mailspace;
use super::backlog::{BacklogDep, backlog_node_by_token, resolve_backlog_dep, resolve_dep_message};
use super::graph::GraphNodeView;
use super::holding::is_foreign_account;
use crate::error::VivariumError;
use crate::letter::Front;
use crate::message::normalize_message_id;
use crate::storage::{
    BACKLOG_GRAPH_CODE, BacklogMintInput, BacklogNodeInput, Storage, StoredMessage,
    WorkGraphEdgeInput, WorkGraphEdgeRow, WorkGraphNodeRow,
};

/// A letter written to the store: `(message id, content id)`.
pub type ArrivedLetter = (String, String);

/// What a dependency reference names in this mailspace.
pub(super) enum DepLookup {
    /// A backlog item held here.
    Held(BacklogDep),
    /// Not held yet, or held for a role this mailspace does not have:
    /// recorded as unresolved and blocking readiness until the letter is
    /// present and done; the edge waits for the letter.
    Pending,
    /// Can never order work here (a foreign-held letter, plain mail, not a
    /// handle): recorded as unresolved exactly like [`DepLookup::Pending`], so
    /// it blocks readiness until a present, done letter answers it, but it
    /// mints no edge.
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

/// The dependency references a letter's bytes name: trimmed, in order,
/// without duplicates. A format-1 work letter names them in its front matter;
/// an old-format letter or one with invalid front matter names none.
pub(crate) fn depends_on_refs(bytes: &[u8]) -> Vec<String> {
    let listed: Vec<String> = match crate::letter::of_message(bytes, "") {
        Ok(letter) => match letter.front {
            Front::Need(front) => front.depends_on,
            Front::Want(front) => front.depends_on,
            Front::Task(front) => front.depends_on,
            _ => return Vec::new(),
        },
        Err(_) => return Vec::new(),
    };
    let mut refs: Vec<String> = Vec::new();
    for value in listed {
        let value = value.trim().to_string();
        if !value.is_empty() && !refs.contains(&value) {
            refs.push(value);
        }
    }
    refs
}

/// The normalized RFC `Message-ID` a task letter names as its parent: the
/// source a `task from` was lowered from. `None` when the letter names none
/// (plain mail, an event letter, an old-format letter).
pub(crate) fn parent_ref(bytes: &[u8]) -> Option<String> {
    match crate::letter::of_message(bytes, "") {
        Ok(letter) => match letter.front {
            Front::Task(front) => front.parent.as_deref().and_then(normalize_message_id),
            _ => None,
        },
        Err(_) => None,
    }
}

/// The normalized `Message-ID` of the letter itself, if it carries one.
fn own_rfc_id(bytes: &[u8]) -> Option<String> {
    MessageParser::default()
        .parse(bytes)
        .and_then(|parsed| parsed.message_id().and_then(normalize_message_id))
}

/// The backlog node of the item behind `content_id`, from whichever of its
/// rows this mailspace tracks (a held foreign item has none).
fn item_node(
    storage: &Storage,
    content_id: &str,
) -> Result<Option<WorkGraphNodeRow>, VivariumError> {
    for row in storage.live_messages_for_content(content_id)? {
        let handle = storage.display_handle(&row.message_id)?;
        if let Some(node) = Mailspace::backlog_node_of(storage, &handle)? {
            return Ok(Some(node));
        }
    }
    Ok(None)
}

/// Whether a reference could ever name a held letter: hex digits, with an
/// optional `msg_` prefix, or a bracketed RFC `Message-ID` a dependency
/// letter names a dependency by.
fn looks_like_reference(token: &str) -> bool {
    if token.starts_with('<') {
        return true;
    }
    let hex = token.strip_prefix("msg_").unwrap_or(token);
    (4..=64).contains(&hex.len()) && hex.chars().all(|c| c.is_ascii_hexdigit())
}

/// Whether `reference` names the arrived letter, as a message id prefix, a
/// handle, or a content id prefix.
fn names_arrival(storage: &Storage, reference: &str, arrived: &ArrivedLetter) -> bool {
    if reference.starts_with('<') {
        let named = normalize_message_id(reference);
        let held = match storage.message_by_id(&arrived.0) {
            Ok(Some(view)) => view.normalized_message_id,
            _ => None,
        };
        return named.is_some() && held == named;
    }
    let reference = reference.to_ascii_lowercase();
    let wanted = reference.strip_prefix("msg_").unwrap_or(&reference);
    let message = arrived.0.to_ascii_lowercase();
    let message = message.strip_prefix("msg_").unwrap_or(&message);
    message.starts_with(wanted) || arrived.1.to_ascii_lowercase().starts_with(wanted)
}

/// Whether following `edges` from node handle `from` reaches `to`.
pub(super) fn reaches(edges: &[WorkGraphEdgeRow], from: &str, to: &str) -> bool {
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
    /// build the prerequisite edges its `depends_on` front matter names.
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
        if let Ok(parsed) = crate::letter::of_message(letter, "") {
            let storage = self.storage()?;
            super::serves::index_front(&storage, message_id, &parsed)?;
        }
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

    /// Rebuild this item's need bind from the `X-Vivi-Parent` headers the
    /// letters carry. A `task from` a need binds its unit locally and writes
    /// no bind letter, so a mailspace that receives the letter learns the
    /// bind from the header instead: the item's own `X-Vivi-Parent` naming a
    /// need held here, and each reply of it whose header names this item.
    /// That covers both arrival orders — the task after the need, and the
    /// task before it, once the reply link is made. Idempotent: a unit whose
    /// node already carries a `subgraph` is left alone, so a later pull of
    /// the same letters writes no second `unit_bound` event.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure.
    pub(super) fn rebuild_parent_binds(
        &self,
        storage: &Storage,
        content_id: &str,
    ) -> Result<(), VivariumError> {
        let Some(view) = storage.message_by_content_id(content_id)? else {
            return Ok(());
        };
        let Ok(bytes) = storage.read_listed_blob(&view.blob_relpath) else {
            return Ok(());
        };
        if let Some(parent) = parent_ref(&bytes) {
            self.bind_unit_to_parent(storage, content_id, &parent)?;
        }
        let Some(own) = own_rfc_id(&bytes) else {
            return Ok(());
        };
        for child in storage.reply_child_contents(content_id)? {
            let Some(child_view) = storage.message_by_content_id(&child)? else {
                continue;
            };
            let Ok(child_bytes) = storage.read_listed_blob(&child_view.blob_relpath) else {
                continue;
            };
            if parent_ref(&child_bytes).as_deref() == Some(own.as_str()) {
                self.bind_unit_to_parent(storage, &child, &own)?;
            }
        }
        Ok(())
    }

    /// Bind the node of the item behind `unit_content_id` to the node of the
    /// open need `parent_rfc_id` names, when this mailspace holds both. A
    /// parent that is not held here as an open need binds nothing, and a unit
    /// that is already bound is left as it is.
    fn bind_unit_to_parent(
        &self,
        storage: &Storage,
        unit_content_id: &str,
        parent_rfc_id: &str,
    ) -> Result<(), VivariumError> {
        let Some(parent) = storage.live_message_by_rfc_id(parent_rfc_id)? else {
            return Ok(());
        };
        let Some(parent_node) = Mailspace::backlog_node_of(storage, &parent.handle)? else {
            return Ok(());
        };
        if parent_node.kind != "need" || parent_node.state != "open" {
            return Ok(());
        }
        let Some(unit) = item_node(storage, unit_content_id)? else {
            return Ok(());
        };
        if unit.subgraph.is_some() || unit.handle == parent_node.handle {
            return Ok(());
        }
        self.bind_units_without_letter(&parent_node.source_id, &[unit.source_id])
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
        let storage = self.storage()?;
        let pending = storage.pending_dependencies()?;
        let mut made = 0;
        for (message_id, refs) in pending {
            let named = refs.iter().any(|reference| {
                arrived
                    .iter()
                    .any(|a| names_arrival(&storage, reference, a))
            });
            if named {
                made += self.link_dependencies(&message_id, &refs)?;
            }
        }
        Ok(made)
    }

    /// The references the item behind `message_id` stands for now: `refs`
    /// (its header's, or the ones still pending) minus those a newer
    /// `dependency-removed` letter dropped, plus the dependencies a newer
    /// `dependency-added` letter names that `refs` does not: by handle when
    /// held here, else by bracketed `Message-ID`, which waits as pending.
    /// With no dependency letters held, `refs` as given.
    pub(super) fn effective_dependency_refs(
        &self,
        storage: &Storage,
        message_id: &str,
        refs: &[String],
    ) -> Result<Vec<String>, VivariumError> {
        let Some(view) = storage.message_by_id(message_id)? else {
            return Ok(refs.to_vec());
        };
        let verdicts = self.dependency_verdicts(storage, &view.content_id)?;
        if verdicts.is_empty() {
            return Ok(refs.to_vec());
        }
        let mut kept: Vec<String> = Vec::new();
        let mut named: HashSet<String> = HashSet::new();
        for token in refs {
            let rfc = dependency_rfc_id(storage, token);
            if let Some(rfc) = &rfc {
                if verdicts.get(rfc) == Some(&false) {
                    continue;
                }
                named.insert(rfc.clone());
            }
            if !kept.contains(token) {
                kept.push(token.clone());
            }
        }
        for (rfc, present) in &verdicts {
            if !present || named.contains(rfc) {
                continue;
            }
            let token = match storage.live_message_by_rfc_id(rfc)? {
                Some(held) => held.handle,
                None => format!("<{rfc}>"),
            };
            if !kept.contains(&token) {
                kept.push(token);
            }
        }
        Ok(kept)
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
        let refs = self.effective_dependency_refs(&storage, message_id, refs)?;
        let plan = self.plan_links(&storage, &nodes, &edges, &dependent, &refs)?;
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
                // Unresolved references are remembered and block readiness
                // until the named letter is present and done; only held
                // references mint edges.
                DepLookup::Pending | DepLookup::Never => plan.pending.push(token.clone()),
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
        let Ok(message_id) = resolve_dep_message(storage, token) else {
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

    /// Whether `token` names work this mailspace does not hold as done: unmet
    /// unless the named dependency is present and done.
    pub(super) fn dependency_is_unmet(&self, token: &str) -> Result<bool, VivariumError> {
        let storage = self.storage()?;
        Ok(!dependency_satisfied(&storage, token))
    }

    /// Mark the nodes whose letter names a dependency this mailspace does not
    /// hold as done. A reference that does not resolve yet (not synced, or
    /// never held here) blocks its dependent exactly like a held, open one:
    /// `blocked_by` carries the raw reference token and `awaiting_dependencies`
    /// names what is still awaited. Stale map entries whose reference now
    /// resolves to a done row are dropped rather than marking anything.
    pub(super) fn mark_awaiting_dependencies(
        &self,
        storage: &Storage,
        nodes: &[WorkGraphNodeRow],
        views: &mut [GraphNodeView],
    ) -> Result<(), VivariumError> {
        for (message_id, refs) in storage.pending_dependencies()? {
            let handle = storage.display_handle(&message_id)?;
            let Some(node) = backlog_node_by_token(storage, nodes, &handle)? else {
                continue;
            };
            let awaiting: Vec<String> = refs
                .into_iter()
                .filter(|reference| !dependency_satisfied(storage, reference))
                .collect();
            if awaiting.is_empty() {
                continue;
            }
            let Some(view) = views.iter_mut().find(|view| view.handle == node.handle) else {
                continue;
            };
            if view.state != "open" {
                continue;
            }
            if view.readiness == "ready" {
                view.readiness = "blocked".into();
            }
            if view.readiness != "blocked" {
                continue;
            }
            for token in &awaiting {
                if !view.blocked_by.contains(token) {
                    view.blocked_by.push(token.clone());
                }
            }
            view.awaiting_dependencies = awaiting;
        }
        Ok(())
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
            let refs = self.effective_dependency_refs(
                storage,
                &message_id,
                &depends_on_refs(&storage.read_message(&message_id)?),
            )?;
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

/// The normalized RFC `Message-ID` of the letter a dependency token names,
/// when it is held (or the token is itself a bracketed `Message-ID`).
pub(super) fn dependency_rfc_id(storage: &Storage, token: &str) -> Option<String> {
    if token.starts_with('<') {
        return normalize_message_id(token);
    }
    let Ok(message_id) = resolve_dep_message(storage, token) else {
        return None;
    };
    match storage.message_by_id(&message_id) {
        Ok(Some(view)) => view.normalized_message_id,
        _ => None,
    }
}

/// Whether `token` names a dependency that is present and satisfied: a held
/// message row (any account: local, foreign, unowned) whose local role is
/// `done`. A token that resolves to no held row, or to a row still open, is
/// unmet.
pub(super) fn dependency_satisfied(storage: &Storage, token: &str) -> bool {
    let Ok(message_id) = resolve_dep_message(storage, token) else {
        return false;
    };
    matches!(
        storage.message_by_id(&message_id),
        Ok(Some(view)) if view.local_role == "done"
    )
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
