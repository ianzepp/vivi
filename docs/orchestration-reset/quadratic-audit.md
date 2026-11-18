# Quadratic cost audit, 2026-11-18

**Status**: a read-only sweep (no code run) after the board fix (`0434273b`). The operator asked for the recurring pattern to be found systematically. The findings below are not fixed.

Marks: **C** = confirmed by reading; **S** = the shape is confirmed but its size is suspected. Paths are under `crates/vivi-core/src/` unless noted. "Faberlang scale" means about 78k letters, 96k events, about 300 wants, and thousands of backlog nodes (estimated).

## Findings, split by what survives the move to Temporal

### Vivi record (stays): fix before Temporal intake reads and writes it

| # | Site | What is quadratic | Hot path | Sev | Fix |
|---|---|---|---|---|---|
| 2 C | `mailspace/want_state.rs:107` `serving_needs` (via `want_states`, `state_of_handle`, `need_closed`) | Each want loads every backlog node, then scans them linearly for each serving row | board wants, boot, `want list`, cycle intake, the pump cadence on every tick | Critical | Point lookup on the existing UNIQUE `(graph_handle, source_id)`, or load once per batch |
| 4 C | `vivi-storage/src/storage/backlog_graph.rs:253` `mint_backlog_tx` | Every mint loads all node ids and edge pairs into sets | every send, pull, merge, import and repair | High | `INSERT OR IGNORE`; the unique constraints already exist |
| 5 C | `mailspace/dependencies.rs:387` `link_dependencies`, `add_edge`/`reaches`, `item_node`, `want_node.rs:59`, `backlog.rs` `backlog_node_of` | Each arriving letter does 1–4 full loads of the nodes and edges | sync filer, merge, adoption, `settle_item`, delivery | High | One graph snapshot carried through the batch, or point lookups |
| 6 C | `vivi-storage/src/storage/pending_deps.rs:26` and `dependencies.rs:313` | Pending dependencies are one JSON blob. Each letter reads and parses it, and a change rewrites the whole blob. `names_arrival` does a message lookup per (pending × arrived) pair | pull, import | High | A table keyed by `(message_id, ref)` with an index |
| 7 C | `backlog.rs:97` `backlog_complete_item_collect`, `reopen_item_cascading`, `graph_mutate.rs:679` | Each completion loads nodes and edges, projects them twice, and settles the served want | per event in `sync_item_nodes`, join cascades, repair, step apply | Med-High | Newly-ready from the node's successors only |
| 8 C | `backlog_audit.rs` `audit_item`, `audit_orphan_nodes` | Linear token lookup per candidate. The orphan check scans nodes × message prefixes | `vivi graph audit` | High | HashMap of nodes; a sorted set of bases searched by prefix |
| 9 C | `sync/select.rs:178` `select` (`vivi-server/.../hosted_index.rs`) | Each pass lists every message and re-classifies the letters it never binds | hub server: on a timer and before reads | High | A persisted classified-through cursor or a skip marker |
| 10 C | `mailspace/board.rs:277` `board_graph_node` | Rebuilds the source→handle map once per node | `board --graph`, MCP board with graph | High | Build the map once |
| 11 C/S | `dependencies.rs:478` `mark_awaiting_dependencies` | Linear `find` per pending entry | every backlog `graph_show` (board graph, boot, step, MCP) | Med-High | A handle→index map |
| 12 C | `vivi-storage/src/storage/schema.rs:200` and `work_graph_nodes_by_subgraph` | No index on `subgraph`, so each lookup scans every node of the graph | join completion | Medium | Index `(graph_handle, subgraph)` |
| 14 C | `vivi-storage/src/storage/handles.rs:427` `content_prefix_matches` | `LIKE` is case-insensitive, so no index applies | `resolve_message_token` misses | Medium | Range predicate |
| 15 S | `holding.rs:190` `mark_holdings`, `delegation.rs:346` | Linear, but heavy: blob reads, plus reply blobs for each row. `list_kind` reads everything before the limit | `graph_show`, boot, work list, MCP | Medium | Skip the reads when there are no replies; push the limit down |

### Pump and pool (retire with the pump in Phase 4): fix only if a pump must run before then

| # | Site | What is quadratic | Sev |
|---|---|---|---|
| 1 C | `pump/flow/mail/read.rs:149` `sent_step` → `delivery.rs` `list_kind(pump, "sent", "task")` | Every step or slice send reads every blob in the sent folder, which only grows | Critical |
| 3 C | `mailspace/pump_record.rs:398` `pump_records` | A scan of `mailspace_events` with no index on every call, plus record reads, per live run per tick | High |
| 13 C | `pool/work.rs:85` → `pool_record.rs:166` `pool_events` | A scan with no index on `event_type` for each collected dispatch, each tick, and each monitor refresh | Medium |
| 16 S | `pump/store.rs:455` `save`, `pump/run.rs`, `plan.rs:140`, … | A whole-state rewrite on every save, with history that is never pruned (this is the cause of the 1.8 TB/day incident) | Low-Med |

## Recurring patterns

1. **Single-handle APIs that load the whole collection.** Examples are `backlog_node_of`, `serving_needs`, `state_of_handle`, `pump_open_record`, `sent_step` and `mint_backlog`. Each looks cheap where it is called, so each new caller loops over it. Point lookups already exist (UNIQUE `(graph_handle, source_id)`), but the code loads everything and then calls `find`.
2. **Computed state re-derived from scratch on every read or event.** This covers want state, run counts, pending dependencies, graph projections, the hosted index pass and the pump snapshot. Every new "computed X" feature has added another of these.
3. **Predicates no index can serve:** `command`, `event_type`, `subgraph`, and a case-insensitive `LIKE`.
4. **`Vec::find` or `any` inside loops**, and history that is never pruned.

## Guards proposed

- **Row-count scaling test.** It is deterministic: it counts rows, so no timers are involved. `Storage` gets test-only counters for the rows returned by the bulk loaders and for blobs read. The test seeds N and 2N items, runs board, boot, graph show, audit, want list and a pull, and asserts `ops(2N) ≤ ~2.3 × ops(N)`.
- **Query-plan check.** Extend the console's `no_console_query_scans_a_big_table` (`crates/vivi-console/src/queries_test.rs`) to the core storage queries.
- **Call-site budget for bulk loaders.** Applies to `work_graph_nodes`, `work_graph_edges`, `pending_dependencies`, `list_messages*` and the event scans, through clippy `disallowed-methods` or a hygiene-style budget. A new use then has to say why it is a once-per-command load.
