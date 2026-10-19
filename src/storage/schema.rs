use rusqlite::Connection;

use crate::error::VivariumError;

const STORAGE_SCHEMA_VERSION: &str = "9";

pub(super) fn ensure_schema(conn: &Connection) -> Result<(), VivariumError> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT value FROM storage_metadata WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .ok();
    if existing.as_deref() == Some(STORAGE_SCHEMA_VERSION) {
        return Ok(());
    }
    // The DDL is fully idempotent (CREATE TABLE/INDEX IF NOT EXISTS), so it
    // runs for upgrades too: tables added to an existing schema version must
    // still reach databases created before those tables existed. Bumping the
    // version forces one repair pass over every old database.
    conn.execute_batch(SCHEMA_DDL)
        .map_err(|e| VivariumError::Other(format!("failed to initialize storage schema: {e}")))?;
    ensure_absorb_columns(conn)?;
    ensure_work_graph_columns(conn)?;
    migrate_event_deferrals(conn)?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS message_metadata_from_addr_date_idx ON message_metadata(from_addr, date)",
        [],
    )
    .map_err(|e| VivariumError::Other(format!("failed to create from_addr index: {e}")))?;
    conn.execute(
        "INSERT OR REPLACE INTO storage_metadata (key, value) VALUES ('schema_version', ?1)",
        rusqlite::params![STORAGE_SCHEMA_VERSION],
    )
    .map(|_| ())
    .map_err(|e| VivariumError::Other(format!("failed to write storage schema version: {e}")))
}

const SCHEMA_DDL: &str = "BEGIN;
         CREATE TABLE IF NOT EXISTS storage_metadata (
           key TEXT PRIMARY KEY,
           value TEXT NOT NULL
         );

         CREATE TABLE IF NOT EXISTS blobs (
           content_id TEXT PRIMARY KEY,
           blob_relpath TEXT NOT NULL UNIQUE,
           byte_size INTEGER NOT NULL,
           rfc_message_id TEXT,
           parsed_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS messages (
           message_id TEXT PRIMARY KEY,
           account TEXT NOT NULL,
           content_id TEXT NOT NULL REFERENCES blobs(content_id) ON DELETE RESTRICT,
           local_role TEXT NOT NULL,
           read_state INTEGER NOT NULL DEFAULT 0,
           starred INTEGER NOT NULL DEFAULT 0,
           draft_state TEXT,
           discovered_at TEXT NOT NULL,
           updated_at TEXT NOT NULL,
           deleted_at TEXT,
           absorbed_at TEXT,
           absorbed_by TEXT
         );
        CREATE INDEX IF NOT EXISTS messages_account_role_idx
          ON messages(account, local_role, updated_at);
        CREATE INDEX IF NOT EXISTS messages_account_content_idx
          ON messages(account, content_id);
        CREATE INDEX IF NOT EXISTS messages_content_idx
          ON messages(content_id);
         CREATE TABLE IF NOT EXISTS remote_bindings (
           message_id TEXT PRIMARY KEY REFERENCES messages(message_id) ON DELETE CASCADE,
           account TEXT NOT NULL,
           provider TEXT NOT NULL,
           remote_mailbox TEXT NOT NULL,
           remote_uid INTEGER NOT NULL,
           remote_uidvalidity INTEGER NOT NULL,
           last_verified_at TEXT NOT NULL,
           stale INTEGER NOT NULL DEFAULT 0,
           UNIQUE (account, remote_mailbox, remote_uidvalidity, remote_uid)
         );
         CREATE TABLE IF NOT EXISTS message_metadata (
           content_id TEXT PRIMARY KEY REFERENCES blobs(content_id) ON DELETE CASCADE,
           date TEXT NOT NULL,
           from_addr TEXT NOT NULL,
           to_addr TEXT NOT NULL,
           cc_addr TEXT NOT NULL,
           bcc_addr TEXT NOT NULL,
           subject TEXT NOT NULL,
           normalized_message_id TEXT
         );
         CREATE TABLE IF NOT EXISTS mailspace_events (
           event_id INTEGER PRIMARY KEY AUTOINCREMENT,
           occurred_at TEXT NOT NULL,
           command TEXT NOT NULL,
           event_type TEXT NOT NULL,
           actor_identity TEXT,
           account TEXT NOT NULL,
           message_id TEXT NOT NULL REFERENCES messages(message_id) ON DELETE CASCADE,
           content_id TEXT NOT NULL,
           from_role TEXT,
           to_role TEXT,
           from_identity TEXT,
           to_identity TEXT,
           subject TEXT NOT NULL,
           note TEXT
         );
         CREATE INDEX IF NOT EXISTS mailspace_events_message_idx
           ON mailspace_events(message_id, occurred_at, event_id);
         CREATE INDEX IF NOT EXISTS mailspace_events_account_idx
           ON mailspace_events(account, occurred_at, event_id);
         CREATE TABLE IF NOT EXISTS mailspace_item_metadata (
           message_id TEXT NOT NULL REFERENCES messages(message_id) ON DELETE CASCADE,
           key TEXT NOT NULL,
           value TEXT NOT NULL,
           updated_at TEXT NOT NULL,
           PRIMARY KEY (message_id, key)
         );
         CREATE INDEX IF NOT EXISTS mailspace_item_metadata_key_idx
           ON mailspace_item_metadata(key, value);
         CREATE TABLE IF NOT EXISTS event_deferrals (
           message_id TEXT PRIMARY KEY REFERENCES messages(message_id) ON DELETE CASCADE,
           until INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS mailspace_links (
           child_content_id TEXT PRIMARY KEY REFERENCES blobs(content_id) ON DELETE CASCADE,
           parent_content_id TEXT NOT NULL REFERENCES blobs(content_id) ON DELETE RESTRICT,
           source TEXT NOT NULL CHECK (source IN ('captured', 'inferred', 'source'))
         );
         CREATE INDEX IF NOT EXISTS mailspace_links_parent_idx
           ON mailspace_links(parent_content_id, child_content_id);
         CREATE TABLE IF NOT EXISTS work_graphs (
           handle TEXT PRIMARY KEY,
           code TEXT NOT NULL UNIQUE,
           status TEXT NOT NULL DEFAULT 'open',
           current_revision INTEGER NOT NULL DEFAULT 0,
           created_at TEXT NOT NULL,
           updated_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS work_graph_revisions (
           graph_handle TEXT NOT NULL REFERENCES work_graphs(handle) ON DELETE CASCADE,
           revision INTEGER NOT NULL,
           mermaid_source TEXT NOT NULL,
           content_hash TEXT NOT NULL,
           created_at TEXT NOT NULL,
           PRIMARY KEY (graph_handle, revision)
         );
         CREATE TABLE IF NOT EXISTS work_graph_nodes (
           handle TEXT PRIMARY KEY,
           graph_handle TEXT NOT NULL REFERENCES work_graphs(handle) ON DELETE CASCADE,
           source_id TEXT NOT NULL,
           label TEXT NOT NULL,
           state TEXT NOT NULL DEFAULT 'open',
           subgraph TEXT,
           kind TEXT NOT NULL DEFAULT 'task',
           created_at TEXT NOT NULL,
           updated_at TEXT NOT NULL,
           UNIQUE (graph_handle, source_id)
         );
         CREATE INDEX IF NOT EXISTS work_graph_nodes_graph_idx
           ON work_graph_nodes(graph_handle, state);
         CREATE TABLE IF NOT EXISTS work_graph_edges (
           handle TEXT PRIMARY KEY,
           graph_handle TEXT NOT NULL REFERENCES work_graphs(handle) ON DELETE CASCADE,
           from_node TEXT NOT NULL REFERENCES work_graph_nodes(handle) ON DELETE CASCADE,
           to_node TEXT NOT NULL REFERENCES work_graph_nodes(handle) ON DELETE CASCADE,
           label TEXT,
           style TEXT NOT NULL DEFAULT 'solid',
           created_at TEXT NOT NULL,
           UNIQUE (graph_handle, from_node, to_node)
         );
         CREATE INDEX IF NOT EXISTS work_graph_edges_to_idx
           ON work_graph_edges(to_node);
         CREATE INDEX IF NOT EXISTS work_graph_edges_from_idx
           ON work_graph_edges(from_node);
         CREATE TABLE IF NOT EXISTS work_graph_events (
           event_id INTEGER PRIMARY KEY AUTOINCREMENT,
           graph_handle TEXT NOT NULL REFERENCES work_graphs(handle) ON DELETE CASCADE,
           occurred_at TEXT NOT NULL,
           event_type TEXT NOT NULL,
           node_handle TEXT,
           note TEXT
         );
         CREATE INDEX IF NOT EXISTS work_graph_events_graph_idx
           ON work_graph_events(graph_handle, occurred_at, event_id);
         CREATE TABLE IF NOT EXISTS work_graph_attempts (
           attempt_id INTEGER PRIMARY KEY AUTOINCREMENT,
           graph_handle TEXT NOT NULL REFERENCES work_graphs(handle) ON DELETE CASCADE,
           node_handle TEXT NOT NULL REFERENCES work_graph_nodes(handle) ON DELETE CASCADE,
           task_message_id TEXT NOT NULL,
           task_handle TEXT NOT NULL,
           role TEXT,
           state TEXT NOT NULL DEFAULT 'active',
           note TEXT,
           created_at TEXT NOT NULL,
           updated_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS work_graph_attempts_node_idx
           ON work_graph_attempts(node_handle, attempt_id);
         CREATE INDEX IF NOT EXISTS work_graph_attempts_task_idx
           ON work_graph_attempts(task_message_id);
         CREATE TABLE IF NOT EXISTS mailspace_goals (
           handle TEXT PRIMARY KEY,
           path TEXT NOT NULL UNIQUE,
           label TEXT,
           registered_by TEXT,
           created_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS mailspace_goals_path_idx
           ON mailspace_goals(path);
         COMMIT;";

fn ensure_work_graph_columns(conn: &Connection) -> Result<(), VivariumError> {
    add_column_if_missing(
        conn,
        "work_graph_nodes",
        "kind",
        "TEXT NOT NULL DEFAULT 'task'",
    )?;
    add_column_if_missing(
        conn,
        "work_graph_edges",
        "style",
        "TEXT NOT NULL DEFAULT 'solid'",
    )
}

/// Move the receiver's event-deferral marks out of the user item-metadata
/// table into their own table. Idempotent: a no-op once no legacy rows remain.
fn migrate_event_deferrals(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute(
        "INSERT OR IGNORE INTO event_deferrals (message_id, until)
         SELECT message_id, CAST(value AS INTEGER) FROM mailspace_item_metadata
         WHERE key = 'event_deferred_until'",
        [],
    )
    .map_err(|e| VivariumError::Other(format!("failed to migrate event deferrals: {e}")))?;
    conn.execute(
        "DELETE FROM mailspace_item_metadata WHERE key = 'event_deferred_until'",
        [],
    )
    .map(|_| ())
    .map_err(|e| VivariumError::Other(format!("failed to clear legacy event deferrals: {e}")))
}

fn ensure_absorb_columns(conn: &Connection) -> Result<(), VivariumError> {
    add_column_if_missing(conn, "messages", "absorbed_at", "TEXT")?;
    add_column_if_missing(conn, "messages", "absorbed_by", "TEXT")?;
    conn.execute(
        "UPDATE messages
         SET absorbed_at = (
               SELECT MIN(occurred_at) FROM mailspace_events
               WHERE mailspace_events.message_id = messages.message_id
                 AND mailspace_events.event_type = 'absorbed'
             ),
             absorbed_by = (
               SELECT actor_identity FROM mailspace_events
               WHERE mailspace_events.message_id = messages.message_id
                 AND mailspace_events.event_type = 'absorbed'
               ORDER BY occurred_at ASC, event_id ASC
               LIMIT 1
             )
         WHERE absorbed_at IS NULL
           AND EXISTS (
             SELECT 1 FROM mailspace_events
             WHERE mailspace_events.message_id = messages.message_id
               AND mailspace_events.event_type = 'absorbed'
           )",
        [],
    )
    .map(|_| ())
    .map_err(|e| VivariumError::Other(format!("failed to backfill absorbed messages: {e}")))
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    decl: &str,
) -> Result<(), VivariumError> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| VivariumError::Other(format!("failed to inspect {table}: {e}")))?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| VivariumError::Other(format!("failed to read {table} columns: {e}")))?;
    for name in names {
        let name =
            name.map_err(|e| VivariumError::Other(format!("failed to read column name: {e}")))?;
        if name == column {
            return Ok(());
        }
    }
    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
        [],
    )
    .map(|_| ())
    .map_err(|e| VivariumError::Other(format!("failed to add {table}.{column} column: {e}")))
}

pub(super) fn message_query(where_clause: &str) -> String {
    [
        "SELECT
            m.message_id,
            m.account,
            m.content_id,
            b.blob_relpath,
            b.byte_size,
            m.local_role,
            m.read_state,
            m.starred,
            md.date,
            md.from_addr,
            md.to_addr,
            md.cc_addr,
            md.bcc_addr,
            md.subject,
            md.normalized_message_id,
            rb.account,
            rb.provider,
            rb.remote_mailbox,
            rb.remote_uid,
            rb.remote_uidvalidity,
            m.absorbed_at,
            m.absorbed_by
         FROM messages m
         JOIN blobs b ON b.content_id = m.content_id
         JOIN message_metadata md ON md.content_id = m.content_id
         LEFT JOIN remote_bindings rb ON rb.message_id = m.message_id ",
        where_clause,
    ]
    .concat()
}
