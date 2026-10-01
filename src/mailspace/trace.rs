//! Communication trace: captured reply links, lifecycle-event links, and
//! inferred citation/thread links, walked outward from one handle.
//!
//! Edge building is index-based. Every candidate lookup that used to scan the
//! whole mailbox is a map hit, bodies are read once in a single pass, and only
//! the nodes the walk includes are materialized for output.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use serde::Serialize;

use super::{Mailspace, kind::effective_kind};
use crate::error::VivariumError;
use crate::storage::{MailspaceEvent, SHORT_HANDLE_LEN, Storage, StoredMessageView};

/// Print a trace graph for the given handle.
///
/// # Errors
/// Returns an error if the handle cannot be resolved, the graph cannot be
/// assembled, or JSON serialization fails when `--json` is used.
pub fn print_trace(
    mailspace: &Mailspace,
    handle: &str,
    max_depth: usize,
    limit: usize,
    json: bool,
) -> Result<(), VivariumError> {
    let graph = mailspace.trace(handle, max_depth, limit)?;
    if json {
        print_trace_json(&graph)?;
    } else {
        print_trace_text(&graph);
    }
    Ok(())
}

fn print_trace_text(graph: &TraceGraph) {
    let seed = graph.nodes.first();
    let seed_handle = seed.map_or(graph.seed.as_str(), |node| node.handle.as_str());
    println!("trace {} ({} node(s))", seed_handle, graph.nodes.len());

    let handle_by_content: HashMap<&str, &str> = graph
        .nodes
        .iter()
        .map(|node| (node.content_id.as_str(), node.handle.as_str()))
        .collect();

    for node in &graph.nodes {
        let kind = kind_for_node(node);
        println!("\n## {} - {} [{}]", node.date, node.handle, kind);
        println!("subject: {}", node.subject);
        if node.messages.len() > 1 {
            println!("copies:");
            for message in &node.messages {
                println!(
                    "  {} {} {}",
                    message.account, message.role, message.message_id
                );
            }
        }
        if !node.edges.is_empty() {
            println!("edges:");
            for edge in &node.edges {
                let target_handle = handle_by_content
                    .get(edge.target.as_str())
                    .copied()
                    .unwrap_or(edge.target.as_str());
                println!(
                    "  {} -> {} ({})",
                    edge.direction, target_handle, edge.source
                );
            }
        }
    }
}

fn kind_for_node(node: &TraceNode) -> String {
    node.messages
        .iter()
        .find_map(|message| message.kind.as_deref())
        .unwrap_or("mail")
        .to_string()
}

/// Print a trace graph as JSON.
///
/// # Errors
/// Returns JSON serialization errors.
pub fn print_trace_json(graph: &TraceGraph) -> Result<(), VivariumError> {
    println!(
        "{}",
        serde_json::to_string_pretty(graph)
            .map_err(|e| VivariumError::Other(format!("failed to encode trace JSON: {e}")))?
    );
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceMessageRef {
    pub message_id: String,
    pub handle: String,
    pub account: String,
    pub role: String,
    pub kind: Option<String>,
    pub date: String,
    pub from: String,
    pub to: String,
    pub subject: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceEdge {
    pub target: String,
    pub source: String,
    pub direction: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceNode {
    pub content_id: String,
    pub handle: String,
    pub messages: Vec<TraceMessageRef>,
    pub date: String,
    pub subject: String,
    pub body: String,
    pub edges: Vec<TraceEdge>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceGraph {
    pub seed: String,
    pub nodes: Vec<TraceNode>,
}

/// One logical message: the metadata every copy shares, plus the copies.
struct TraceContent {
    content_id: String,
    /// Message id of the primary copy: earliest date, then lowest id.
    message_id: String,
    handle: String,
    date: String,
    subject: String,
    participants: BTreeSet<String>,
    /// Blob path of the content, from the listing.
    blob_relpath: String,
    /// Every copy in date order, for the node's copy list.
    copies: Vec<StoredMessageView>,
}

/// Lifecycle events, plus the index the node views need.
struct EventIndex {
    events: Vec<MailspaceEvent>,
    by_message: HashMap<String, Vec<usize>>,
}

impl EventIndex {
    fn load(storage: &Storage) -> Result<Self, VivariumError> {
        let events = storage.list_mailspace_events_after(0)?;
        let mut by_message: HashMap<String, Vec<usize>> = HashMap::new();
        for (position, event) in events.iter().enumerate() {
            by_message
                .entry(event.message_id.clone())
                .or_default()
                .push(position);
        }
        Ok(Self { events, by_message })
    }

    /// Events of one message, for kind resolution.
    fn of_message(&self, message_id: &str) -> Vec<MailspaceEvent> {
        self.by_message
            .get(message_id)
            .into_iter()
            .flatten()
            .map(|position| self.events[*position].clone())
            .collect()
    }
}

/// Every lookup edge building needs, built without reading a body.
struct TraceIndex {
    contents: Vec<TraceContent>,
    by_content: HashMap<String, usize>,
    /// Primary handle -> contents carrying it.
    by_handle: HashMap<String, Vec<usize>>,
    /// `message_id` -> content id, for full-id references in event notes.
    content_of_message: HashMap<String, String>,
    /// Message-body citation targets, keyed two ways: an 8-byte window for
    /// fixed-width handles, and exact strings for every other shape.
    windows: HashMap<u64, Vec<usize>>,
    odd_handles: Vec<(String, usize)>,
    /// (reply-stripped subject, participants) -> contents, oldest first.
    threads: HashMap<(String, BTreeSet<String>), Vec<usize>>,
    /// Content id -> captured reply edges. Captured links are symmetric.
    links: HashMap<String, Vec<TraceEdge>>,
    events: EventIndex,
}

impl TraceIndex {
    fn build(storage: &Storage) -> Result<Self, VivariumError> {
        let (contents, content_of_message) = group_contents(storage)?;
        let by_content = contents
            .iter()
            .enumerate()
            .map(|(index, content)| (content.content_id.clone(), index))
            .collect();
        let handles = HandleIndexes::build(&contents);
        Ok(Self {
            by_content,
            by_handle: handles.by_handle,
            windows: handles.windows,
            odd_handles: handles.odd_handles,
            threads: build_threads(&contents),
            links: build_links(storage)?,
            events: EventIndex::load(storage)?,
            content_of_message,
            contents,
        })
    }

    /// Content id for a handle or a full message id written into an event
    /// note. Ambiguous handles resolve to nothing, as in token resolution.
    fn content_for_token(&self, token: &str) -> Option<String> {
        if let Some(matches) = self.by_handle.get(token) {
            return match matches.as_slice() {
                [only] => Some(self.contents[*only].content_id.clone()),
                _ => None,
            };
        }
        self.content_of_message.get(token).cloned()
    }

    /// Newest content cited by `body` that is not newer than `child`.
    ///
    /// Ties on date are broken by content id, so a body citing two contents
    /// stamped in the same second always resolves to the same parent.
    fn cited_parent(&self, child: usize, body: &str) -> Option<usize> {
        let child_date = self.contents[child].date.as_str();
        citing_contents(body, &self.windows, &self.odd_handles)
            .into_iter()
            .filter(|cited| *cited != child && self.contents[*cited].date.as_str() <= child_date)
            .max_by(|left, right| {
                self.contents[*left]
                    .date
                    .cmp(&self.contents[*right].date)
                    .then_with(|| {
                        self.contents[*right]
                            .content_id
                            .cmp(&self.contents[*left].content_id)
                    })
            })
    }

    /// Newest content sharing the child's reply-stripped subject and
    /// participants that is not newer than the child.
    fn thread_parent(&self, child: usize) -> Option<usize> {
        let content = &self.contents[child];
        let key = (
            strip_reply_prefix(&content.subject),
            content.participants.clone(),
        );
        thread_candidate(&self.contents, self.threads.get(&key)?, child)
    }
}

impl Mailspace {
    /// Build a trace graph rooted at the given handle.
    ///
    /// The graph walks captured reply links, inferred body-citation links, and
    /// lifecycle-event links (e.g. `tasked`) up to `max_depth` and `limit`.
    /// Multiple copies of the same logical message (same `content_id`) are
    /// collapsed into a single node.
    ///
    /// # Errors
    /// Returns an error if the handle cannot be resolved, storage queries fail,
    /// or the graph cannot be assembled.
    pub fn trace(
        &self,
        handle: &str,
        max_depth: usize,
        limit: usize,
    ) -> Result<TraceGraph, VivariumError> {
        let storage = self.storage()?;
        let seed_id = storage.resolve_message_token(handle)?;
        let seed = storage
            .message_by_id(&seed_id)?
            .ok_or_else(|| VivariumError::Message(format!("message not found: {handle}")))?;
        let seed_content_id = seed.content_id.clone();

        let index = TraceIndex::build(&storage)?;
        let adjacency = build_adjacency(&index, &storage)?;
        let included = walk_from_seed(&seed_content_id, max_depth, limit, &adjacency);
        let nodes = assemble_graph(&index, &storage, &adjacency, &seed_content_id, &included)?;
        Ok(TraceGraph {
            seed: seed_content_id,
            nodes,
        })
    }
}

/// Load every active message and group it into its logical content.
fn group_contents(
    storage: &Storage,
) -> Result<(Vec<TraceContent>, HashMap<String, String>), VivariumError> {
    let mut grouped: HashMap<String, Vec<StoredMessageView>> = HashMap::new();
    let mut content_of_message = HashMap::new();
    for view in storage.list_messages()? {
        content_of_message.insert(view.message_id.clone(), view.content_id.clone());
        grouped
            .entry(view.content_id.clone())
            .or_default()
            .push(view);
    }
    let mut contents: Vec<TraceContent> = grouped
        .into_iter()
        .filter_map(|(content_id, mut copies)| {
            copies.sort_by(|left, right| {
                left.date
                    .cmp(&right.date)
                    .then_with(|| left.message_id.cmp(&right.message_id))
            });
            let primary = copies.first()?;
            let message_id = primary.message_id.clone();
            let handle = primary.handle.clone();
            let date = primary.date.clone();
            let subject = primary.subject.clone();
            let people = participants(&primary.from_addr, &primary.to_addr, &primary.cc_addr);
            let blob_relpath = primary.blob_relpath.clone();
            Some(TraceContent {
                content_id,
                message_id,
                handle,
                date,
                subject,
                participants: people,
                blob_relpath,
                copies,
            })
        })
        .collect();
    // Content order drives every tie-break, so keep it deterministic.
    contents.sort_by(|left, right| left.content_id.cmp(&right.content_id));
    Ok((contents, content_of_message))
}

/// Content handles, keyed for exact lookup and for body citation scans.
struct HandleIndexes {
    by_handle: HashMap<String, Vec<usize>>,
    windows: HashMap<u64, Vec<usize>>,
    odd_handles: Vec<(String, usize)>,
}

impl HandleIndexes {
    fn build(contents: &[TraceContent]) -> Self {
        let mut by_handle: HashMap<String, Vec<usize>> = HashMap::new();
        let mut windows: HashMap<u64, Vec<usize>> = HashMap::new();
        let mut odd_handles = Vec::new();
        for (position, content) in contents.iter().enumerate() {
            by_handle
                .entry(content.handle.clone())
                .or_default()
                .push(position);
            match handle_window(&content.handle) {
                Some(key) => windows.entry(key).or_default().push(position),
                None => odd_handles.push((content.handle.clone(), position)),
            }
        }
        Self {
            by_handle,
            windows,
            odd_handles,
        }
    }
}

/// Contents whose handle appears anywhere in `body`.
///
/// Fixed-width handles are found with a sliding byte window, so a handle that
/// is embedded in a longer token is still found; handles of any other shape
/// fall back to an exact substring test.
fn citing_contents(
    body: &str,
    windows: &HashMap<u64, Vec<usize>>,
    odd_handles: &[(String, usize)],
) -> Vec<usize> {
    let mut found = Vec::new();
    if !windows.is_empty() {
        let bytes = body.as_bytes();
        let mut run = 0usize;
        for (position, byte) in bytes.iter().enumerate() {
            if !is_lower_hex(*byte) {
                run = 0;
                continue;
            }
            run += 1;
            if run < SHORT_HANDLE_LEN {
                continue;
            }
            let start = position + 1 - SHORT_HANDLE_LEN;
            let mut window = [0u8; SHORT_HANDLE_LEN];
            window.copy_from_slice(&bytes[start..start + SHORT_HANDLE_LEN]);
            if let Some(matches) = windows.get(&u64::from_be_bytes(window)) {
                found.extend(matches.iter().copied());
            }
        }
    }
    for (handle, position) in odd_handles {
        if body.contains(handle.as_str()) {
            found.push(*position);
        }
    }
    found
}

/// Group contents into reply-thread buckets, oldest first.
fn build_threads(contents: &[TraceContent]) -> HashMap<(String, BTreeSet<String>), Vec<usize>> {
    let mut threads: HashMap<(String, BTreeSet<String>), Vec<usize>> = HashMap::new();
    for (position, content) in contents.iter().enumerate() {
        threads
            .entry((
                strip_reply_prefix(&content.subject),
                content.participants.clone(),
            ))
            .or_default()
            .push(position);
    }
    for bucket in threads.values_mut() {
        bucket.sort_by(|left, right| {
            contents[*left]
                .date
                .cmp(&contents[*right].date)
                .then_with(|| contents[*left].content_id.cmp(&contents[*right].content_id))
        });
    }
    threads
}

/// Newest bucket entry that is not the child itself and not newer than it.
///
/// Buckets are ordered oldest first, so the first hit from the end is the
/// newest eligible parent.
fn thread_candidate(contents: &[TraceContent], bucket: &[usize], child: usize) -> Option<usize> {
    let child_date = contents[child].date.as_str();
    bucket
        .iter()
        .rev()
        .copied()
        .find(|candidate| *candidate != child && contents[*candidate].date.as_str() <= child_date)
}

/// Captured reply links, indexed by the content on each end.
fn build_links(storage: &Storage) -> Result<HashMap<String, Vec<TraceEdge>>, VivariumError> {
    let mut links: HashMap<String, Vec<TraceEdge>> = HashMap::new();
    for link in storage.list_mailspace_links()? {
        push_edge(
            &mut links,
            &link.parent_content_id,
            TraceEdge {
                target: link.child_content_id.clone(),
                source: link.source.clone(),
                direction: "descendant".into(),
            },
        );
        push_edge(
            &mut links,
            &link.child_content_id,
            TraceEdge {
                target: link.parent_content_id,
                source: link.source,
                direction: "ancestor".into(),
            },
        );
    }
    Ok(links)
}

/// Every edge in the mailspace: captured links, then lifecycle events, then
/// inferred parents. That order is the order a node reports its edges in.
fn build_adjacency(
    index: &TraceIndex,
    storage: &Storage,
) -> Result<HashMap<String, Vec<TraceEdge>>, VivariumError> {
    let mut adjacency: HashMap<String, Vec<TraceEdge>> = HashMap::new();
    for (content_id, edges) in &index.links {
        for edge in edges {
            push_edge(&mut adjacency, content_id, edge.clone());
        }
    }
    add_event_edges(&mut adjacency, index);
    add_inferred_edges(&mut adjacency, index, storage)?;
    Ok(adjacency)
}

/// `task from` events link the sourced item to every item tasked from it.
fn add_event_edges(adjacency: &mut HashMap<String, Vec<TraceEdge>>, index: &TraceIndex) {
    for event in &index.events.events {
        if event.command != "task from" || event.event_type != "tasked" {
            continue;
        }
        let Some(note) = &event.note else {
            continue;
        };
        for handle in parse_task_handles(note) {
            let Some(task_content_id) = index.content_for_token(&handle) else {
                continue;
            };
            push_edge(
                adjacency,
                &event.content_id,
                TraceEdge {
                    target: task_content_id.clone(),
                    source: "event".into(),
                    direction: "descendant".into(),
                },
            );
            push_edge(
                adjacency,
                &task_content_id,
                TraceEdge {
                    target: event.content_id.clone(),
                    source: "event".into(),
                    direction: "ancestor".into(),
                },
            );
        }
    }
}

/// Inferred parent links, one body read per content: the citation pass needs
/// the text, the thread pass only the subject and participants.
fn add_inferred_edges(
    adjacency: &mut HashMap<String, Vec<TraceEdge>>,
    index: &TraceIndex,
    storage: &Storage,
) -> Result<(), VivariumError> {
    for position in 0..index.contents.len() {
        let content = &index.contents[position];
        let data = storage.read_listed_blob(&content.blob_relpath)?;
        let body = text_body(&data);
        let parent = index
            .cited_parent(position, &body)
            .or_else(|| index.thread_parent(position));
        let Some(parent) = parent else {
            continue;
        };
        let parent_id = index.contents[parent].content_id.clone();
        push_edge(
            adjacency,
            &parent_id,
            TraceEdge {
                target: content.content_id.clone(),
                source: "inferred".into(),
                direction: "descendant".into(),
            },
        );
        push_edge(
            adjacency,
            &content.content_id,
            TraceEdge {
                target: parent_id,
                source: "inferred".into(),
                direction: "ancestor".into(),
            },
        );
    }
    Ok(())
}

fn push_edge(adjacency: &mut HashMap<String, Vec<TraceEdge>>, from: &str, edge: TraceEdge) {
    if edge.target == from {
        return;
    }
    adjacency.entry(from.to_string()).or_default().push(edge);
}

/// Contents reachable from the seed within `max_depth` and `limit`.
fn walk_from_seed(
    seed: &str,
    max_depth: usize,
    limit: usize,
    adjacency: &HashMap<String, Vec<TraceEdge>>,
) -> HashSet<String> {
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    queue.push_back((seed.to_string(), 0usize));
    visited.insert(seed.to_string());

    while let Some((content_id, depth)) = queue.pop_front() {
        if depth >= max_depth || visited.len() >= limit {
            continue;
        }
        let targets: Vec<String> = adjacency
            .get(&content_id)
            .into_iter()
            .flatten()
            .map(|edge| edge.target.clone())
            .collect();
        for target in targets {
            if visited.insert(target.clone()) {
                queue.push_back((target, depth + 1));
            }
        }
    }
    visited
}

/// Materialize the included contents and their edges into output nodes.
fn assemble_graph(
    index: &TraceIndex,
    storage: &Storage,
    adjacency: &HashMap<String, Vec<TraceEdge>>,
    seed: &str,
    included: &HashSet<String>,
) -> Result<Vec<TraceNode>, VivariumError> {
    let mut nodes = Vec::with_capacity(included.len());
    for content_id in included {
        let Some(&position) = index.by_content.get(content_id) else {
            continue;
        };
        nodes.push(node_view(index, storage, adjacency, position, included)?);
    }
    nodes.sort_by(|left, right| {
        left.date
            .cmp(&right.date)
            .then_with(|| left.content_id.cmp(&right.content_id))
    });
    // Ensure seed is first.
    if let Some(seed_index) = nodes.iter().position(|node| node.content_id == seed)
        && seed_index > 0
    {
        nodes.swap(0, seed_index);
    }
    Ok(nodes)
}

fn node_view(
    index: &TraceIndex,
    storage: &Storage,
    adjacency: &HashMap<String, Vec<TraceEdge>>,
    position: usize,
    included: &HashSet<String>,
) -> Result<TraceNode, VivariumError> {
    let content = &index.contents[position];
    let body = storage.read_listed_blob(&content.blob_relpath)?;
    let events = index.events.of_message(&content.message_id);
    let messages = content
        .copies
        .iter()
        .map(|view| TraceMessageRef {
            message_id: view.message_id.clone(),
            handle: view.handle.clone(),
            account: view.account.clone(),
            role: view.local_role.clone(),
            kind: effective_kind(&view.local_role, &body, &events),
            date: view.date.clone(),
            from: view.from_addr.clone(),
            to: view.to_addr.clone(),
            subject: view.subject.clone(),
        })
        .collect();
    let edges = adjacency
        .get(&content.content_id)
        .into_iter()
        .flatten()
        .filter(|edge| included.contains(&edge.target))
        .cloned()
        .collect();
    Ok(TraceNode {
        content_id: content.content_id.clone(),
        handle: content.handle.clone(),
        messages,
        date: content.date.clone(),
        subject: content.subject.clone(),
        body: text_body(&body),
        edges,
    })
}

fn parse_task_handles(note: &str) -> Vec<String> {
    let Some(prefix) = note.strip_prefix("active_tasks=") else {
        return Vec::new();
    };
    let Some(rest) = prefix.split(';').next() else {
        return Vec::new();
    };
    rest.split(',').map(|s| s.trim().to_string()).collect()
}

fn participants(from_addr: &str, to_addr: &str, cc_addr: &str) -> BTreeSet<String> {
    let mut set = BTreeSet::from([from_addr.to_ascii_lowercase()]);
    set.extend(to_addr.split(", ").map(str::to_ascii_lowercase));
    set.extend(cc_addr.split(", ").map(str::to_ascii_lowercase));
    set
}

/// Big-endian window key for an 8-character lowercase-hex handle.
fn handle_window(handle: &str) -> Option<u64> {
    let bytes = handle.as_bytes();
    if bytes.len() != SHORT_HANDLE_LEN || !bytes.iter().all(|byte| is_lower_hex(*byte)) {
        return None;
    }
    let mut window = [0u8; SHORT_HANDLE_LEN];
    window.copy_from_slice(bytes);
    Some(u64::from_be_bytes(window))
}

fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

fn strip_reply_prefix(subject: &str) -> String {
    let mut subject = subject.trim();
    loop {
        let lower = subject.to_ascii_lowercase();
        let Some(after_re) = lower.strip_prefix("re") else {
            break;
        };
        let Some(colon_offset) = reply_prefix_colon(after_re) else {
            break;
        };
        subject = subject[2 + colon_offset + 1..].trim_start();
    }
    subject.to_ascii_lowercase()
}

fn reply_prefix_colon(after_re: &str) -> Option<usize> {
    if after_re.starts_with(':') {
        return Some(0);
    }
    let closing = after_re.strip_prefix('[')?.find(']')? + 2;
    after_re.get(closing..)?.starts_with(':').then_some(closing)
}

fn text_body(data: &[u8]) -> String {
    mail_parser::MessageParser::default()
        .parse(data)
        .and_then(|parsed| parsed.body_text(0).map(|body| body.to_string()))
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "trace_test.rs"]
mod tests;
