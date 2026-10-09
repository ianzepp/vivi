//! Local chat state: joined rooms, read cursors, and the transactional
//! outbox in `<project>/.vivi/chat/state.sqlite`.

use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};

use crate::chat::ensure_chat_dir;
use crate::error::VivariumError;

const STATE_FILE: &str = "state.sqlite";

/// A locally recorded joined room.
#[derive(Debug, Clone)]
pub struct JoinedRoom {
    pub room_id: String,
    pub seat: u32,
    pub label: String,
    pub joined_at: String,
}

/// One undelivered outbox row.
#[derive(Debug, Clone)]
pub struct PendingMessage {
    pub id: i64,
    pub room_id: String,
    pub message_id: String,
    pub payload_json: String,
}

/// Local chat state store. Offline sends enqueue here and flush
/// at-least-once on later commands.
pub struct ChatState {
    conn: Connection,
}

impl ChatState {
    /// Open (creating if needed) the chat state database in `dir`.
    ///
    /// # Errors
    /// Returns an error if the directory, database file, or schema cannot be
    /// created.
    pub fn open(dir: &Path) -> Result<Self, VivariumError> {
        ensure_chat_dir(dir)?;
        let path = dir.join(STATE_FILE);
        let fresh = !path.exists();
        let conn = Connection::open(&path).map_err(|e| {
            VivariumError::Other(format!("failed to open chat state database: {e}"))
        })?;
        Self::apply_schema(&conn)?;
        if fresh {
            Self::restrict_mode(&path)?;
        }
        Ok(Self { conn })
    }

    fn apply_schema(conn: &Connection) -> Result<(), VivariumError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS rooms (
                room_id TEXT PRIMARY KEY,
                seat INTEGER NOT NULL,
                label TEXT NOT NULL,
                joined_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS cursors (
                room_id TEXT PRIMARY KEY,
                cursor TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS sync_cursors (
                room_id TEXT PRIMARY KEY,
                cursor TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS outbox (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                room_id TEXT NOT NULL,
                message_id TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                created_at TEXT NOT NULL,
                flushed_at TEXT
            );",
        )
        .map_err(|e| VivariumError::Other(format!("failed to init chat state schema: {e}")))?;
        Ok(())
    }

    /// Record a joined room (upsert on room_id).
    ///
    /// # Errors
    /// Returns an error on a database failure.
    pub fn join_room(&self, room_id: &str, seat: u32, label: &str) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "INSERT INTO rooms (room_id, seat, label, joined_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(room_id) DO UPDATE SET seat = excluded.seat, label = excluded.label",
                params![room_id, seat, label, now_rfc3339()],
            )
            .map_err(sql_err)?;
        Ok(())
    }

    /// Number of joined rooms.
    ///
    /// # Errors
    /// Returns an error on a database failure.
    #[must_use]
    pub fn joined_room_count(&self) -> Result<usize, VivariumError> {
        self.conn
            .query_row("SELECT COUNT(*) FROM rooms", [], |row| row.get::<_, i64>(0))
            .map(|n| n as usize)
            .map_err(sql_err)
    }

    /// Joined rooms in join order.
    ///
    /// # Errors
    /// Returns an error on a database failure.
    pub fn rooms(&self) -> Result<Vec<JoinedRoom>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare("SELECT room_id, seat, label, joined_at FROM rooms ORDER BY joined_at")
            .map_err(sql_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(JoinedRoom {
                    room_id: row.get(0)?,
                    seat: row.get::<_, i64>(1)? as u32,
                    label: row.get(2)?,
                    joined_at: row.get(3)?,
                })
            })
            .map_err(sql_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sql_err)
    }

    /// One joined room by id, if present.
    ///
    /// # Errors
    /// Returns an error on a database failure.
    pub fn room(&self, room_id: &str) -> Result<Option<JoinedRoom>, VivariumError> {
        self.conn
            .query_row(
                "SELECT room_id, seat, label, joined_at FROM rooms WHERE room_id = ?1",
                params![room_id],
                |row| {
                    Ok(JoinedRoom {
                        room_id: row.get(0)?,
                        seat: row.get::<_, i64>(1)? as u32,
                        label: row.get(2)?,
                        joined_at: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(sql_err)
    }

    /// Remove a joined room (local unjoin) and its read cursor. Returns
    /// whether a room row was actually removed.
    ///
    /// # Errors
    /// Returns an error on a database failure.
    pub fn leave_room(&self, room_id: &str) -> Result<bool, VivariumError> {
        self.conn
            .execute("DELETE FROM cursors WHERE room_id = ?1", params![room_id])
            .map_err(sql_err)?;
        self.conn
            .execute(
                "DELETE FROM sync_cursors WHERE room_id = ?1",
                params![room_id],
            )
            .map_err(sql_err)?;
        let removed = self
            .conn
            .execute("DELETE FROM rooms WHERE room_id = ?1", params![room_id])
            .map_err(sql_err)?;
        Ok(removed > 0)
    }

    /// Set the read cursor for a room.
    ///
    /// # Errors
    /// Returns an error on a database failure.
    pub fn set_cursor(&self, room_id: &str, cursor: &str) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "INSERT INTO cursors (room_id, cursor) VALUES (?1, ?2)
                 ON CONFLICT(room_id) DO UPDATE SET cursor = excluded.cursor",
                params![room_id, cursor],
            )
            .map_err(sql_err)?;
        Ok(())
    }

    /// Stored read cursor for a room, if any.
    ///
    /// # Errors
    /// Returns an error on a database failure.
    pub fn cursor(&self, room_id: &str) -> Result<Option<String>, VivariumError> {
        self.conn
            .query_row(
                "SELECT cursor FROM cursors WHERE room_id = ?1",
                params![room_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(sql_err)
    }

    /// Set the down-sync cursor for a room. It is independent of the read
    /// cursor: `vivi chat read` and the board badge track what a human has
    /// seen, this tracks what has been filed into the mailspace.
    ///
    /// # Errors
    /// Returns an error on a database failure.
    pub fn set_sync_cursor(&self, room_id: &str, cursor: &str) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "INSERT INTO sync_cursors (room_id, cursor) VALUES (?1, ?2)
                 ON CONFLICT(room_id) DO UPDATE SET cursor = excluded.cursor",
                params![room_id, cursor],
            )
            .map_err(sql_err)?;
        Ok(())
    }

    /// Stored down-sync cursor for a room, if any.
    ///
    /// # Errors
    /// Returns an error on a database failure.
    pub fn sync_cursor(&self, room_id: &str) -> Result<Option<String>, VivariumError> {
        self.conn
            .query_row(
                "SELECT cursor FROM sync_cursors WHERE room_id = ?1",
                params![room_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(sql_err)
    }

    /// Enqueue a message for at-least-once flush. Returns the outbox row id.
    ///
    /// # Errors
    /// Returns an error on a database failure.
    pub fn enqueue(
        &self,
        room_id: &str,
        message_id: &str,
        payload_json: &str,
    ) -> Result<i64, VivariumError> {
        self.conn
            .execute(
                "INSERT INTO outbox (room_id, message_id, payload_json, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![room_id, message_id, payload_json, now_rfc3339()],
            )
            .map_err(sql_err)?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Mark an outbox row delivered.
    ///
    /// # Errors
    /// Returns an error on a database failure.
    pub fn mark_flushed(&self, id: i64) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "UPDATE outbox SET flushed_at = ?1 WHERE id = ?2",
                params![now_rfc3339(), id],
            )
            .map_err(sql_err)?;
        Ok(())
    }

    /// Number of undelivered outbox rows.
    ///
    /// # Errors
    /// Returns an error on a database failure.
    #[must_use]
    pub fn outbox_depth(&self) -> Result<usize, VivariumError> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM outbox WHERE flushed_at IS NULL",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|n| n as usize)
            .map_err(sql_err)
    }

    /// Undelivered outbox rows, oldest first.
    ///
    /// # Errors
    /// Returns an error on a database failure.
    pub fn pending_outbox(&self) -> Result<Vec<PendingMessage>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, room_id, message_id, payload_json FROM outbox
                 WHERE flushed_at IS NULL ORDER BY id",
            )
            .map_err(sql_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(PendingMessage {
                    id: row.get(0)?,
                    room_id: row.get(1)?,
                    message_id: row.get(2)?,
                    payload_json: row.get(3)?,
                })
            })
            .map_err(sql_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sql_err)
    }

    #[cfg(unix)]
    fn restrict_mode(path: &Path) -> Result<(), VivariumError> {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(())
    }

    #[cfg(not(unix))]
    fn restrict_mode(_path: &Path) -> Result<(), VivariumError> {
        Ok(())
    }
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

fn sql_err(e: rusqlite::Error) -> VivariumError {
    VivariumError::Other(format!("chat state database error: {e}"))
}

#[cfg(test)]
#[path = "state_test.rs"]
mod tests;
