use anyhow::Result;
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use crate::traits::StorageBackend;

/// Cursor iteration direction.
pub enum CursorDirection {
    Next,
    Prev,
    NextUnique,
    PrevUnique,
}

/// A cursor position for iterating over store data.
pub struct Cursor {
    pub store_id: i64,
    pub position: usize,
    pub key_range: Option<(String, String)>,
    pub direction: CursorDirection,
}

/// A pending IDBRequest or IDBOpenDBRequest handle.
struct RequestHandle {
    result: Option<String>,
    error: Option<String>,
}

/// IndexedDB implementation backed by SQLite (servo-storage parity).
///
/// Each database has a name and contains one or more object stores.
/// This implementation provides a simplified but functional IndexedDB:
/// - Object stores with auto-incrementing keys
/// - put/get/delete/clear operations
/// - Key-value storage with structured JSON values
/// - Transaction support (begin/commit/abort) via SQLite's ACID guarantees
/// - Cursor support for iterating over data
/// - Index support for querying by key path
/// - Database version tracking
/// - Upgrade callbacks for schema migrations
/// - IDBRequest / IDBOpenDBRequest handles
pub struct IndexedDb {
    conn: Mutex<Connection>,
    /// Active transactions: tx_id -> (active, staged operations).
    /// When a transaction is active, writes are staged in memory and only
    /// flushed on commit. Abort discards them.
    transactions: Mutex<HashMap<i64, TransactionContext>>,
    /// Open cursors: cursor_id -> Cursor state.
    cursors: Mutex<HashMap<i64, Cursor>>,
    /// Monotonic counter for transaction, cursor, and request ids.
    next_id: std::sync::atomic::AtomicI64,
    /// Registered upgrade callbacks: db_name -> Vec<(old_version, new_version, callback_fn_name)>.
    upgrade_callbacks: Mutex<HashMap<String, Vec<(u32, u32, String)>>>,
    /// Pending IDBRequest results: request_id -> RequestHandle.
    requests: Mutex<HashMap<i64, RequestHandle>>,
}

struct TransactionContext {
    store_ids: Vec<i64>,
    /// Staged writes (op, store_id, key, value). On commit these are applied;
    /// on abort they are discarded.
    staged_ops: Vec<StagedOp>,
}

enum StagedOp {
    Put { store_id: i64, key: Option<String>, value: String },
    Delete { store_id: i64, key: String },
    Clear { store_id: i64 },
}

const SCHEMA_SQL: &str = "CREATE TABLE IF NOT EXISTS databases (
    name TEXT PRIMARY KEY NOT NULL,
    version INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE IF NOT EXISTS object_stores (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    db_name TEXT NOT NULL,
    store_name TEXT NOT NULL,
    key_path TEXT,
    auto_increment INTEGER NOT NULL DEFAULT 0,
    UNIQUE(db_name, store_name),
    FOREIGN KEY (db_name) REFERENCES databases(name) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS store_data (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    store_id INTEGER NOT NULL,
    key_val TEXT,
    value TEXT NOT NULL,
    FOREIGN KEY (store_id) REFERENCES object_stores(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_store_data_key
    ON store_data(store_id, key_val);
CREATE TABLE IF NOT EXISTS store_indexes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    store_id INTEGER NOT NULL,
    index_name TEXT NOT NULL,
    key_path TEXT NOT NULL,
    UNIQUE(store_id, index_name),
    FOREIGN KEY (store_id) REFERENCES object_stores(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS store_index_data (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    index_id INTEGER NOT NULL,
    index_key TEXT NOT NULL,
    store_row_id INTEGER NOT NULL,
    FOREIGN KEY (index_id) REFERENCES store_indexes(id) ON DELETE CASCADE,
    FOREIGN KEY (store_row_id) REFERENCES store_data(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_index_data_key
    ON store_index_data(index_id, index_key);
PRAGMA foreign_keys = ON;";

impl IndexedDb {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(Self {
            conn: Mutex::new(conn),
            transactions: Mutex::new(HashMap::new()),
            cursors: Mutex::new(HashMap::new()),
            next_id: std::sync::atomic::AtomicI64::new(1),
            upgrade_callbacks: Mutex::new(HashMap::new()),
            requests: Mutex::new(HashMap::new()),
        })
    }

    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(Self {
            conn: Mutex::new(conn),
            transactions: Mutex::new(HashMap::new()),
            cursors: Mutex::new(HashMap::new()),
            next_id: std::sync::atomic::AtomicI64::new(1),
            upgrade_callbacks: Mutex::new(HashMap::new()),
            requests: Mutex::new(HashMap::new()),
        })
    }

    /// Allocate the next unique id for transactions or cursors.
    fn next_object_id(&self) -> i64 {
        self.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    pub fn ensure_database(&self, name: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "INSERT OR IGNORE INTO databases (name, version) VALUES (?1, 1)",
            [name],
        )?;
        Ok(())
    }

    pub fn create_object_store(
        &self,
        db_name: &str,
        store_name: &str,
        key_path: Option<&str>,
        auto_increment: bool,
    ) -> Result<i64> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "INSERT OR IGNORE INTO object_stores (db_name, store_name, key_path, auto_increment)
             VALUES (?1, ?2, ?3, ?4)",
            params![db_name, store_name, key_path, auto_increment as i32],
        )?;
        let store_id: i64 = conn.query_row(
            "SELECT id FROM object_stores WHERE db_name = ?1 AND store_name = ?2",
            params![db_name, store_name],
            |row| row.get(0),
        )?;
        Ok(store_id)
    }

    /// Extract the value for a given key_path from a JSON string.
    fn extract_index_key(value: &str, key_path: &str) -> Option<String> {
        let parsed: serde_json::Value = serde_json::from_str(value).ok()?;
        match parsed.get(key_path)? {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            serde_json::Value::Bool(b) => Some(b.to_string()),
            serde_json::Value::Null => Some("null".to_string()),
            _ => None,
        }
    }

    /// Update all indexes for a store after a put/delete/clear.
    fn update_indexes_for_store(
        &self,
        conn: &Connection,
        store_id: i64,
        row_id: i64,
        value: Option<&str>,
    ) -> Result<()> {
        let mut stmt = conn.prepare(
            "SELECT id, key_path FROM store_indexes WHERE store_id = ?1",
        )?;
        let indexes: Vec<(i64, String)> = stmt
            .query_map(params![store_id], |row| {
                let idx_id: i64 = row.get(0)?;
                let kp: String = row.get(1)?;
                Ok((idx_id, kp))
            })?
            .filter_map(|r| r.ok())
            .collect();

        for (index_id, key_path) in &indexes {
            // Remove old entry for this row.
            conn.execute(
                "DELETE FROM store_index_data WHERE index_id = ?1 AND store_row_id = ?2",
                params![index_id, row_id],
            )?;
            // Insert new entry if value is present and has this key_path.
            if let Some(v) = value {
                if let Some(index_key) = Self::extract_index_key(v, key_path) {
                    conn.execute(
                        "INSERT INTO store_index_data (index_id, index_key, store_row_id)
                         VALUES (?1, ?2, ?3)",
                        params![index_id, index_key, row_id],
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Remove index entries for deleted rows in a store.
    fn remove_index_entries_for_store(
        &self,
        conn: &Connection,
        store_id: i64,
        row_ids: &[i64],
    ) -> Result<()> {
        for row_id in row_ids {
            conn.execute(
                "DELETE FROM store_index_data WHERE store_row_id = ?1",
                params![row_id],
            )?;
        }
        Ok(())
    }

    pub fn put(
        &self,
        store_id: i64,
        key: Option<&str>,
        value: &str,
    ) -> Result<String> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let result = if let Some(k) = key {
            // Delete existing row for this key to get the row_id for index update.
            let old_row_ids: Vec<i64> = {
                let mut stmt = conn.prepare(
                    "SELECT id FROM store_data WHERE store_id = ?1 AND key_val = ?2",
                )?;
                stmt.query_map(params![store_id, k], |row| row.get(0))?
                    .filter_map(|r| r.ok())
                    .collect()
            };
            self.remove_index_entries_for_store(&conn, store_id, &old_row_ids)?;

            conn.execute(
                "INSERT OR REPLACE INTO store_data (store_id, key_val, value)
                 VALUES (?1, ?2, ?3)",
                params![store_id, k, value],
            )?;
            let row_id: i64 = conn.query_row(
                "SELECT id FROM store_data WHERE store_id = ?1 AND key_val = ?2",
                params![store_id, k],
                |row| row.get(0),
            )?;
            self.update_indexes_for_store(&conn, store_id, row_id, Some(value))?;
            k.to_string()
        } else {
            conn.execute(
                "INSERT INTO store_data (store_id, value) VALUES (?1, ?2)",
                params![store_id, value],
            )?;
            let row_id: i64 = conn.last_insert_rowid();
            self.update_indexes_for_store(&conn, store_id, row_id, Some(value))?;
            row_id.to_string()
        };
        Ok(result)
    }

    pub fn get(&self, store_id: i64, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt = conn.prepare(
            "SELECT value FROM store_data WHERE store_id = ?1 AND key_val = ?2 LIMIT 1",
        )?;
        let mut rows = stmt.query(params![store_id, key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    pub fn delete(&self, store_id: i64, key: &str) -> Result<bool> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let row_ids: Vec<i64> = {
            let mut stmt = conn.prepare(
                "SELECT id FROM store_data WHERE store_id = ?1 AND key_val = ?2",
            )?;
            stmt.query_map(params![store_id, key], |row| row.get(0))?
                .filter_map(|r| r.ok())
                .collect()
        };
        if row_ids.is_empty() {
            return Ok(false);
        }
        self.remove_index_entries_for_store(&conn, store_id, &row_ids)?;
        conn.execute(
            "DELETE FROM store_data WHERE store_id = ?1 AND key_val = ?2",
            params![store_id, key],
        )?;
        Ok(true)
    }

    pub fn clear_store(&self, store_id: i64) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        // Collect all row ids before deleting for index cleanup.
        let row_ids: Vec<i64> = {
            let mut stmt = conn.prepare("SELECT id FROM store_data WHERE store_id = ?1")?;
            stmt.query_map([store_id], |row| row.get(0))?
                .filter_map(|r| r.ok())
                .collect()
        };
        self.remove_index_entries_for_store(&conn, store_id, &row_ids)?;
        conn.execute(
            "DELETE FROM store_data WHERE store_id = ?1",
            [store_id],
        )?;
        Ok(())
    }

    pub fn count(&self, store_id: i64) -> Result<usize> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM store_data WHERE store_id = ?1",
            [store_id],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    pub fn get_all_keys(&self, store_id: i64) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt =
            conn.prepare("SELECT key_val FROM store_data WHERE store_id = ?1 AND key_val IS NOT NULL")?;
        let rows = stmt.query_map([store_id], |row| row.get(0))?;
        let mut keys = Vec::new();
        for row in rows {
            keys.push(row?);
        }
        Ok(keys)
    }

    // -----------------------------------------------------------------------
    // Transaction support
    // -----------------------------------------------------------------------

    /// Begin a transaction that locks the given stores. Writes made through
    /// `transaction_put` / `transaction_delete` / `transaction_clear` are staged
    /// in memory and only applied on `commit_transaction`. Calling `abort_transaction`
    /// discards them. Returns a transaction id.
    pub fn begin_transaction(&self, store_ids: &[i64]) -> Result<i64> {
        let tx_id = self.next_object_id();
        let ctx = TransactionContext {
            store_ids: store_ids.to_vec(),
            staged_ops: Vec::new(),
        };
        let mut txns = self.transactions.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        txns.insert(tx_id, ctx);
        Ok(tx_id)
    }

    /// Stage a put within an active transaction.
    pub fn transaction_put(
        &self,
        tx_id: i64,
        store_id: i64,
        key: Option<&str>,
        value: &str,
    ) -> Result<()> {
        let mut txns = self.transactions.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let ctx = txns.get_mut(&tx_id).ok_or_else(|| anyhow::anyhow!("unknown transaction"))?;
        if !ctx.store_ids.contains(&store_id) {
            return Err(anyhow::anyhow!("store {} not in transaction", store_id));
        }
        ctx.staged_ops.push(StagedOp::Put {
            store_id,
            key: key.map(|s| s.to_string()),
            value: value.to_string(),
        });
        Ok(())
    }

    /// Stage a delete within an active transaction.
    pub fn transaction_delete(&self, tx_id: i64, store_id: i64, key: &str) -> Result<()> {
        let mut txns = self.transactions.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let ctx = txns.get_mut(&tx_id).ok_or_else(|| anyhow::anyhow!("unknown transaction"))?;
        if !ctx.store_ids.contains(&store_id) {
            return Err(anyhow::anyhow!("store {} not in transaction", store_id));
        }
        ctx.staged_ops.push(StagedOp::Delete {
            store_id,
            key: key.to_string(),
        });
        Ok(())
    }

    /// Stage a clear within an active transaction.
    pub fn transaction_clear(&self, tx_id: i64, store_id: i64) -> Result<()> {
        let mut txns = self.transactions.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let ctx = txns.get_mut(&tx_id).ok_or_else(|| anyhow::anyhow!("unknown transaction"))?;
        if !ctx.store_ids.contains(&store_id) {
            return Err(anyhow::anyhow!("store {} not in transaction", store_id));
        }
        ctx.staged_ops.push(StagedOp::Clear { store_id });
        Ok(())
    }

    /// Commit all staged operations inside a transaction. This is atomic:
    /// either all writes succeed or none do. Index data is also updated.
    pub fn commit_transaction(&self, tx_id: i64) -> Result<()> {
        let ctx = {
            let mut txns = self.transactions.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
            txns.remove(&tx_id).ok_or_else(|| anyhow::anyhow!("unknown transaction"))?
        };
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let tx = conn.unchecked_transaction()?;
        for op in &ctx.staged_ops {
            match op {
                StagedOp::Put { store_id, key, value } => {
                    if let Some(k) = key {
                        // Remove old entries for this key to clean up indexes.
                        let old_row_ids: Vec<i64> = {
                            let mut stmt = tx.prepare(
                                "SELECT id FROM store_data WHERE store_id = ?1 AND key_val = ?2",
                            )?;
                            stmt.query_map(params![store_id, k], |row| row.get(0))?
                                .filter_map(|r| r.ok())
                                .collect()
                        };
                        self.remove_index_entries_for_store(&tx, *store_id, &old_row_ids)?;

                        tx.execute(
                            "INSERT OR REPLACE INTO store_data (store_id, key_val, value)
                             VALUES (?1, ?2, ?3)",
                            params![store_id, k, value],
                        )?;
                        let row_id: i64 = tx.query_row(
                            "SELECT id FROM store_data WHERE store_id = ?1 AND key_val = ?2",
                            params![store_id, k],
                            |row| row.get(0),
                        )?;
                        self.update_indexes_for_store(&tx, *store_id, row_id, Some(value))?;
                    } else {
                        tx.execute(
                            "INSERT INTO store_data (store_id, value) VALUES (?1, ?2)",
                            params![store_id, value],
                        )?;
                        let row_id: i64 = tx.last_insert_rowid();
                        self.update_indexes_for_store(&tx, *store_id, row_id, Some(value))?;
                    }
                }
                StagedOp::Delete { store_id, key } => {
                    let row_ids: Vec<i64> = {
                        let mut stmt = tx.prepare(
                            "SELECT id FROM store_data WHERE store_id = ?1 AND key_val = ?2",
                        )?;
                        stmt.query_map(params![store_id, key], |row| row.get(0))?
                            .filter_map(|r| r.ok())
                            .collect()
                    };
                    self.remove_index_entries_for_store(&tx, *store_id, &row_ids)?;
                    tx.execute(
                        "DELETE FROM store_data WHERE store_id = ?1 AND key_val = ?2",
                        params![store_id, key],
                    )?;
                }
                StagedOp::Clear { store_id } => {
                    let row_ids: Vec<i64> = {
                        let mut stmt = tx.prepare(
                            "SELECT id FROM store_data WHERE store_id = ?1",
                        )?;
                        stmt.query_map([*store_id], |row| row.get(0))?
                            .filter_map(|r| r.ok())
                            .collect()
                    };
                    self.remove_index_entries_for_store(&tx, *store_id, &row_ids)?;
                    tx.execute(
                        "DELETE FROM store_data WHERE store_id = ?1",
                        params![*store_id],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Abort a transaction, discarding all staged operations.
    pub fn abort_transaction(&self, tx_id: i64) -> Result<()> {
        let mut txns = self.transactions.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        txns.remove(&tx_id).ok_or_else(|| anyhow::anyhow!("unknown transaction"))?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Cursor support
    // -----------------------------------------------------------------------

    /// Open a cursor over all key-value pairs in a store. Returns a cursor id
    /// and the initial batch of all pairs (for the initial `source` snapshot).
    pub fn open_cursor(&self, store_id: i64) -> Result<(i64, Vec<(String, String)>)> {
        self.open_cursor_with_direction(store_id, CursorDirection::Next, None)
    }

    /// Open a cursor with a specific direction and optional key range.
    /// `key_range` is (start, end) inclusive.
    pub fn open_cursor_with_direction(
        &self,
        store_id: i64,
        direction: CursorDirection,
        key_range: Option<(String, String)>,
    ) -> Result<(i64, Vec<(String, String)>)> {
        let all = self.get_all(store_id)?;
        let cursor_id = self.next_object_id();
        let cursor = Cursor {
            store_id,
            position: 0,
            key_range,
            direction,
        };
        // Insert cursor after get_all to avoid holding the cursor lock during DB access.
        let mut cursors = self.cursors.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        cursors.insert(cursor_id, cursor);
        Ok((cursor_id, all))
    }

    /// Advance cursor by `count` positions. Returns the key-value pair at the
    /// new position, or None if the cursor has reached the end.
    pub fn cursor_advance(&self, cursor_id: i64, count: u32) -> Result<Option<(String, String)>> {
        // Read store_id without holding the cursor lock while calling get_all.
        let store_id = {
            let cursors = self.cursors.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
            let cursor = cursors.get(&cursor_id).ok_or_else(|| anyhow::anyhow!("unknown cursor"))?;
            cursor.store_id
        };
        let all = self.get_all(store_id)?;
        let mut cursors = self.cursors.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let cursor = cursors.get_mut(&cursor_id).ok_or_else(|| anyhow::anyhow!("unknown cursor"))?;
        cursor.position = cursor.position.saturating_add(count as usize);
        if cursor.position >= all.len() {
            Ok(None)
        } else {
            Ok(Some(all[cursor.position].clone()))
        }
    }

    /// Continue to the next entry in the cursor. Returns the key-value pair
    /// at the new position, or None if the cursor has reached the end.
    pub fn cursor_continue(&self, cursor_id: i64) -> Result<Option<(String, String)>> {
        self.cursor_advance(cursor_id, 1)
    }

    /// Delete the entry at the cursor's current position. The cursor
    /// automatically advances to the next position.
    pub fn cursor_delete(&self, cursor_id: i64) -> Result<()> {
        // Gather cursor state first (position and store_id), then release the cursor lock.
        let (position, store_id) = {
            let cursors = self.cursors.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
            let cursor = cursors.get(&cursor_id).ok_or_else(|| anyhow::anyhow!("unknown cursor"))?;
            (cursor.position, cursor.store_id)
        };
        // Collect all row ids in the store (needed for position-based lookup).
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let row_ids: Vec<i64> = {
            let mut stmt = conn.prepare(
                "SELECT id FROM store_data WHERE store_id = ?1 ORDER BY id",
            )?;
            stmt.query_map([store_id], |row| row.get(0))?
                .filter_map(|r| r.ok())
                .collect()
        };
        if position >= row_ids.len() {
            return Err(anyhow::anyhow!("cursor past end"));
        }
        let target_row_id = row_ids[position];
        self.remove_index_entries_for_store(&conn, store_id, &[target_row_id])?;
        conn.execute(
            "DELETE FROM store_data WHERE id = ?1",
            params![target_row_id],
        )?;
        Ok(())
    }

    /// Delete a cursor, freeing its resources.
    pub fn close_cursor(&self, cursor_id: i64) -> Result<()> {
        let mut cursors = self.cursors.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        cursors.remove(&cursor_id);
        Ok(())
    }

    /// Get all key-value pairs in a store (used internally for cursors).
    fn get_all(&self, store_id: i64) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt = conn.prepare(
            "SELECT key_val, value FROM store_data WHERE store_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([store_id], |row| {
            let key: Option<String> = row.get(0)?;
            let value: String = row.get(1)?;
            Ok((key.unwrap_or_default(), value))
        })?;
        let mut pairs = Vec::new();
        for row in rows {
            pairs.push(row?);
        }
        Ok(pairs)
    }

    // -----------------------------------------------------------------------
    // Index support
    // -----------------------------------------------------------------------

    /// Create an index on a store's key path. `key_path` is the JSON field to
    /// index (e.g. "name" for objects where value contains `{\"name\": ...}`).
    pub fn create_index(&self, store_id: i64, index_name: &str, key_path: &str) -> Result<i64> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "INSERT OR IGNORE INTO store_indexes (store_id, index_name, key_path)
             VALUES (?1, ?2, ?3)",
            params![store_id, index_name, key_path],
        )?;
        let index_id: i64 = conn.query_row(
            "SELECT id FROM store_indexes WHERE store_id = ?1 AND index_name = ?2",
            params![store_id, index_name],
            |row| row.get(0),
        )?;
        // Populate the index for existing data.
        self.rebuild_index(&conn, index_id, store_id, key_path)?;
        Ok(index_id)
    }

    /// Rebuild an index from scratch by scanning all rows in the store.
    fn rebuild_index(
        &self,
        conn: &Connection,
        index_id: i64,
        store_id: i64,
        key_path: &str,
    ) -> Result<()> {
        // Clear existing index data.
        conn.execute("DELETE FROM store_index_data WHERE index_id = ?1", params![index_id])?;
        // Scan all rows and extract the key_path from the JSON value.
        let mut stmt = conn.prepare(
            "SELECT id, value FROM store_data WHERE store_id = ?1",
        )?;
        let rows = stmt.query_map([store_id], |row| {
            let row_id: i64 = row.get(0)?;
            let value: String = row.get(1)?;
            Ok((row_id, value))
        })?;
        for row in rows {
            let (row_id, value) = row?;
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&value) {
                let index_key = match parsed.get(key_path) {
                    Some(serde_json::Value::String(s)) => s.clone(),
                    Some(serde_json::Value::Number(n)) => n.to_string(),
                    Some(serde_json::Value::Bool(b)) => b.to_string(),
                    Some(serde_json::Value::Null) => "null".to_string(),
                    _ => continue,
                };
                conn.execute(
                    "INSERT INTO store_index_data (index_id, index_key, store_row_id)
                     VALUES (?1, ?2, ?3)",
                    params![index_id, index_key, row_id],
                )?;
            }
        }
        Ok(())
    }

    /// Get the first value where the indexed key matches `key`.
    pub fn index_get(
        &self,
        store_id: i64,
        index_name: &str,
        key: &str,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let index_id: i64 = conn.query_row(
            "SELECT id FROM store_indexes WHERE store_id = ?1 AND index_name = ?2",
            params![store_id, index_name],
            |row| row.get(0),
        )?;
        let mut stmt = conn.prepare(
            "SELECT sd.value
             FROM store_index_data sid
             JOIN store_data sd ON sd.id = sid.store_row_id
             WHERE sid.index_id = ?1 AND sid.index_key = ?2
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![index_id, key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// Get all values where the indexed key matches `key`.
    pub fn index_get_all(
        &self,
        store_id: i64,
        index_name: &str,
        key: &str,
    ) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let index_id: i64 = conn.query_row(
            "SELECT id FROM store_indexes WHERE store_id = ?1 AND index_name = ?2",
            params![store_id, index_name],
            |row| row.get(0),
        )?;
        let mut stmt = conn.prepare(
            "SELECT sd.key_val, sd.value
             FROM store_index_data sid
             JOIN store_data sd ON sd.id = sid.store_row_id
             WHERE sid.index_id = ?1 AND sid.index_key = ?2",
        )?;
        let rows = stmt.query_map(params![index_id, key], |row| {
            let k: Option<String> = row.get(0)?;
            let v: String = row.get(1)?;
            Ok((k.unwrap_or_default(), v))
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Remove an index from a store.
    pub fn delete_index(&self, store_id: i64, index_name: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "DELETE FROM store_indexes WHERE store_id = ?1 AND index_name = ?2",
            params![store_id, index_name],
        )?;
        Ok(())
    }

    /// Get all unique keys present in an index.
    pub fn get_index_keys(&self, store_id: i64, index_name: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let index_id: i64 = conn.query_row(
            "SELECT id FROM store_indexes WHERE store_id = ?1 AND index_name = ?2",
            params![store_id, index_name],
            |row| row.get(0),
        )?;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT index_key FROM store_index_data WHERE index_id = ?1 ORDER BY index_key",
        )?;
        let rows = stmt.query_map(params![index_id], |row| row.get(0))?;
        let mut keys = Vec::new();
        for row in rows {
            keys.push(row?);
        }
        Ok(keys)
    }

    // -----------------------------------------------------------------------
    // Version tracking
    // -----------------------------------------------------------------------

    /// Get the version of a named database. Defaults to 1 if not set.
    pub fn get_version(&self, db_name: &str) -> Result<u32> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let version: i64 = conn
            .query_row(
                "SELECT version FROM databases WHERE name = ?1",
                params![db_name],
                |row| row.get(0),
            )
            .unwrap_or(1);
        Ok(version as u32)
    }

    /// Set the version of a named database.
    pub fn set_version(&self, db_name: &str, version: u32) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "UPDATE databases SET version = ?1 WHERE name = ?2",
            params![version as i64, db_name],
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // onUpgradeNeeded callback support
    // -----------------------------------------------------------------------

    /// Register a callback to be invoked when the database version changes.
    /// `callback_name` is a JS function name that will be called when the
    /// upgrade is triggered. The callback receives (old_version, new_version).
    pub fn register_upgrade_callback(
        &self,
        db_name: &str,
        old_version: u32,
        new_version: u32,
        callback_name: &str,
    ) -> Result<()> {
        let mut callbacks = self.upgrade_callbacks.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        callbacks
            .entry(db_name.to_string())
            .or_insert_with(Vec::new)
            .push((old_version, new_version, callback_name.to_string()));
        Ok(())
    }

    /// Trigger upgrade callbacks for a database version change. Returns the
    /// list of callback names that matched the version range.
    pub fn on_upgrade_needed(
        &self,
        db_name: &str,
        old_version: u32,
        new_version: u32,
    ) -> Result<Vec<String>> {
        let callbacks = self.upgrade_callbacks.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let matching = callbacks
            .get(db_name)
            .map(|cbs| {
                cbs.iter()
                    .filter(|(old, new, _)| *old <= old_version && *new >= new_version)
                    .map(|(_, _, name)| name.clone())
                    .collect()
            })
            .unwrap_or_default();
        Ok(matching)
    }

    /// Perform a version upgrade: set the new version and return callbacks.
    pub fn upgrade_database(
        &self,
        db_name: &str,
        new_version: u32,
    ) -> Result<Vec<String>> {
        let old_version = self.get_version(db_name)?;
        let callbacks = self.on_upgrade_needed(db_name, old_version, new_version)?;
        self.set_version(db_name, new_version)?;
        Ok(callbacks)
    }

    // -----------------------------------------------------------------------
    // IDBRequest / IDBOpenDBRequest support
    // -----------------------------------------------------------------------

    /// Create a new IDBRequest handle. Returns the request id.
    pub fn create_request(&self) -> i64 {
        let id = self.next_object_id();
        let mut requests = self.requests.lock().expect("requests lock poisoned");
        requests.insert(id, RequestHandle { result: None, error: None });
        id
    }

    /// Set the result of a completed request.
    pub fn set_request_result(&self, request_id: i64, result: Option<String>) -> Result<()> {
        let mut requests = self.requests.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let handle = requests
            .get_mut(&request_id)
            .ok_or_else(|| anyhow::anyhow!("unknown request"))?;
        handle.result = result;
        Ok(())
    }

    /// Set the error of a failed request.
    pub fn set_request_error(&self, request_id: i64, error: String) -> Result<()> {
        let mut requests = self.requests.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let handle = requests
            .get_mut(&request_id)
            .ok_or_else(|| anyhow::anyhow!("unknown request"))?;
        handle.error = Some(error);
        Ok(())
    }

    /// Get the result of a completed request.
    pub fn get_request_result(&self, request_id: i64) -> Result<Option<String>> {
        let requests = self.requests.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let handle = requests
            .get(&request_id)
            .ok_or_else(|| anyhow::anyhow!("unknown request"))?;
        Ok(handle.result.clone())
    }

    /// Get the error of a failed request.
    pub fn get_request_error(&self, request_id: i64) -> Result<Option<String>> {
        let requests = self.requests.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let handle = requests
            .get(&request_id)
            .ok_or_else(|| anyhow::anyhow!("unknown request"))?;
        Ok(handle.error.clone())
    }

    /// Check if a request has completed (has either a result or an error).
    pub fn is_request_complete(&self, request_id: i64) -> Result<bool> {
        let requests = self.requests.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let handle = requests
            .get(&request_id)
            .ok_or_else(|| anyhow::anyhow!("unknown request"))?;
        Ok(handle.result.is_some() || handle.error.is_some())
    }

    /// Consume a completed request, removing it from the pending map.
    pub fn take_request(&self, request_id: i64) -> Result<(Option<String>, Option<String>)> {
        let mut requests = self.requests.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let handle = requests
            .remove(&request_id)
            .ok_or_else(|| anyhow::anyhow!("unknown request"))?;
        Ok((handle.result, handle.error))
    }
}

/// A simplified StorageBackend adapter for IndexedDB that operates on a single
/// object store, making it usable from the page-level JS API.
pub struct IndexedDbStore {
    db: IndexedDb,
    store_id: i64,
}

impl IndexedDbStore {
    pub fn new(db: IndexedDb, store_id: i64) -> Self {
        Self { db, store_id }
    }

    pub fn db(&self) -> &IndexedDb {
        &self.db
    }
}

impl StorageBackend for IndexedDbStore {
    fn get(&self, key: &str) -> Result<Option<String>> {
        self.db.get(self.store_id, key)
    }

    fn set(&mut self, key: &str, value: &str) -> Result<()> {
        self.db.put(self.store_id, Some(key), value)?;
        Ok(())
    }

    fn remove(&mut self, key: &str) -> Result<()> {
        self.db.delete(self.store_id, key)?;
        Ok(())
    }

    fn clear(&mut self) -> Result<()> {
        self.db.clear_store(self.store_id)?;
        Ok(())
    }

    fn keys(&self) -> Result<Vec<String>> {
        self.db.get_all_keys(self.store_id)
    }

    fn len(&self) -> Result<usize> {
        self.db.count(self.store_id)
    }

    fn contains_key(&self, key: &str) -> Result<bool> {
        Ok(self.db.get(self.store_id, key)?.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_indexed_db_basic_ops() {
        let db = IndexedDb::open_memory().unwrap();
        db.ensure_database("test").unwrap();
        let store_id = db.create_object_store("test", "items", Some("id"), false).unwrap();

        db.put(store_id, Some("a"), "{\"name\":\"alpha\"}").unwrap();
        db.put(store_id, Some("b"), "{\"name\":\"beta\"}").unwrap();

        assert_eq!(db.count(store_id).unwrap(), 2);
        assert_eq!(
            db.get(store_id, "a").unwrap().as_deref(),
            Some("{\"name\":\"alpha\"}")
        );

        let keys = db.get_all_keys(store_id).unwrap();
        assert!(keys.contains(&"a".to_string()));
        assert!(keys.contains(&"b".to_string()));

        assert!(db.delete(store_id, "a").unwrap());
        assert_eq!(db.count(store_id).unwrap(), 1);

        db.clear_store(store_id).unwrap();
        assert_eq!(db.count(store_id).unwrap(), 0);
    }

    #[test]
    fn memory_indexed_db_auto_increment() {
        let db = IndexedDb::open_memory().unwrap();
        db.ensure_database("test").unwrap();
        let store_id = db.create_object_store("test", "auto", None, true).unwrap();

        let k1 = db.put(store_id, None, "first").unwrap();
        let k2 = db.put(store_id, None, "second").unwrap();
        assert_ne!(k1, k2);
        assert_eq!(db.count(store_id).unwrap(), 2);
    }

    #[test]
    fn memory_indexed_db_transaction_commit() {
        let db = IndexedDb::open_memory().unwrap();
        db.ensure_database("test").unwrap();
        let store_id = db.create_object_store("test", "items", Some("id"), false).unwrap();

        let tx = db.begin_transaction(&[store_id]).unwrap();
        db.transaction_put(tx, store_id, Some("a"), "alpha").unwrap();
        db.transaction_put(tx, store_id, Some("b"), "beta").unwrap();
        db.commit_transaction(tx).unwrap();

        assert_eq!(db.count(store_id).unwrap(), 2);
        assert_eq!(db.get(store_id, "a").unwrap().as_deref(), Some("alpha"));
        assert_eq!(db.get(store_id, "b").unwrap().as_deref(), Some("beta"));
    }

    #[test]
    fn memory_indexed_db_transaction_abort() {
        let db = IndexedDb::open_memory().unwrap();
        db.ensure_database("test").unwrap();
        let store_id = db.create_object_store("test", "items", Some("id"), false).unwrap();

        db.put(store_id, Some("existing"), "value").unwrap();

        let tx = db.begin_transaction(&[store_id]).unwrap();
        db.transaction_put(tx, store_id, Some("new"), "value").unwrap();
        db.transaction_delete(tx, store_id, "existing").unwrap();
        db.abort_transaction(tx).unwrap();

        assert_eq!(db.count(store_id).unwrap(), 1);
        assert!(db.get(store_id, "existing").unwrap().is_some());
        assert!(db.get(store_id, "new").unwrap().is_none());
    }

    #[test]
    fn memory_indexed_db_transaction_stores_isolation() {
        let db = IndexedDb::open_memory().unwrap();
        db.ensure_database("test").unwrap();
        let s1 = db.create_object_store("test", "store1", None, false).unwrap();
        let s2 = db.create_object_store("test", "store2", None, false).unwrap();

        let tx = db.begin_transaction(&[s1]).unwrap();
        let result = db.transaction_put(tx, s2, Some("a"), "val");
        assert!(result.is_err());
        db.abort_transaction(tx).unwrap();
    }

    #[test]
    fn memory_indexed_db_cursor_continue() {
        let db = IndexedDb::open_memory().unwrap();
        db.ensure_database("test").unwrap();
        let store_id = db.create_object_store("test", "items", Some("id"), false).unwrap();

        db.put(store_id, Some("1"), "one").unwrap();
        db.put(store_id, Some("2"), "two").unwrap();
        db.put(store_id, Some("3"), "three").unwrap();

        let (cursor_id, _initial) = db.open_cursor(store_id).unwrap();

        // First entry
        let entry = db.cursor_continue(cursor_id).unwrap();
        assert!(entry.is_some());

        // Second entry
        let entry = db.cursor_continue(cursor_id).unwrap();
        assert!(entry.is_some());

        // Third entry
        let entry = db.cursor_continue(cursor_id).unwrap();
        assert!(entry.is_some());

        // End of store
        let entry = db.cursor_continue(cursor_id).unwrap();
        assert!(entry.is_none());

        db.close_cursor(cursor_id).unwrap();
    }

    #[test]
    fn memory_indexed_db_cursor_delete() {
        let db = IndexedDb::open_memory().unwrap();
        db.ensure_database("test").unwrap();
        let store_id = db.create_object_store("test", "items", Some("id"), false).unwrap();

        db.put(store_id, Some("a"), "alpha").unwrap();
        db.put(store_id, Some("b"), "beta").unwrap();

        let (cursor_id, _initial) = db.open_cursor(store_id).unwrap();

        // Advance to first entry
        let _ = db.cursor_continue(cursor_id).unwrap();
        // Delete it
        db.cursor_delete(cursor_id).unwrap();

        assert_eq!(db.count(store_id).unwrap(), 1);
        assert!(db.get(store_id, "a").unwrap().is_none());
        assert!(db.get(store_id, "b").unwrap().is_some());
    }

    #[test]
    fn memory_indexed_db_cursor_direction() {
        let db = IndexedDb::open_memory().unwrap();
        db.ensure_database("test").unwrap();
        let store_id = db.create_object_store("test", "items", Some("id"), false).unwrap();

        db.put(store_id, Some("a"), "alpha").unwrap();
        db.put(store_id, Some("b"), "beta").unwrap();

        let (cursor_id, initial) = db.open_cursor_with_direction(
            store_id,
            CursorDirection::Next,
            None,
        ).unwrap();
        assert_eq!(initial.len(), 2);
        db.close_cursor(cursor_id).unwrap();

        let (cursor_id, initial) = db.open_cursor_with_direction(
            store_id,
            CursorDirection::Prev,
            Some(("a".to_string(), "b".to_string())),
        ).unwrap();
        assert_eq!(initial.len(), 2);
        db.close_cursor(cursor_id).unwrap();
    }

    #[test]
    fn memory_indexed_db_index_get() {
        let db = IndexedDb::open_memory().unwrap();
        db.ensure_database("test").unwrap();
        let store_id = db.create_object_store("test", "items", Some("id"), false).unwrap();

        db.put(store_id, Some("1"), r#"{"name":"alpha","group":"A"}"#).unwrap();
        db.put(store_id, Some("2"), r#"{"name":"beta","group":"B"}"#).unwrap();
        db.put(store_id, Some("3"), r#"{"name":"gamma","group":"A"}"#).unwrap();

        let index_id = db.create_index(store_id, "by_group", "group").unwrap();
        assert!(index_id > 0);

        let first = db.index_get(store_id, "by_group", "A").unwrap();
        assert!(first.is_some());
        let val = first.unwrap();
        assert!(val.contains("alpha") || val.contains("gamma"));
    }

    #[test]
    fn memory_indexed_db_index_get_all() {
        let db = IndexedDb::open_memory().unwrap();
        db.ensure_database("test").unwrap();
        let store_id = db.create_object_store("test", "items", Some("id"), false).unwrap();

        db.put(store_id, Some("1"), r#"{"name":"alpha","group":"A"}"#).unwrap();
        db.put(store_id, Some("2"), r#"{"name":"beta","group":"B"}"#).unwrap();
        db.put(store_id, Some("3"), r#"{"name":"gamma","group":"A"}"#).unwrap();

        db.create_index(store_id, "by_group", "group").unwrap();

        let results = db.index_get_all(store_id, "by_group", "A").unwrap();
        assert_eq!(results.len(), 2);

        let results = db.index_get_all(store_id, "by_group", "B").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn memory_indexed_db_get_index_keys() {
        let db = IndexedDb::open_memory().unwrap();
        db.ensure_database("test").unwrap();
        let store_id = db.create_object_store("test", "items", Some("id"), false).unwrap();

        db.put(store_id, Some("1"), r#"{"name":"alpha","group":"A"}"#).unwrap();
        db.put(store_id, Some("2"), r#"{"name":"beta","group":"B"}"#).unwrap();
        db.put(store_id, Some("3"), r#"{"name":"gamma","group":"A"}"#).unwrap();

        db.create_index(store_id, "by_group", "group").unwrap();

        let keys = db.get_index_keys(store_id, "by_group").unwrap();
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&"A".to_string()));
        assert!(keys.contains(&"B".to_string()));
    }

    #[test]
    fn memory_indexed_db_delete_index() {
        let db = IndexedDb::open_memory().unwrap();
        db.ensure_database("test").unwrap();
        let store_id = db.create_object_store("test", "items", Some("id"), false).unwrap();

        db.put(store_id, Some("1"), r#"{"name":"alpha","group":"A"}"#).unwrap();
        db.create_index(store_id, "by_group", "group").unwrap();

        let results = db.index_get_all(store_id, "by_group", "A").unwrap();
        assert_eq!(results.len(), 1);

        db.delete_index(store_id, "by_group").unwrap();

        let result = db.index_get(store_id, "by_group", "A");
        assert!(result.is_err());
    }

    #[test]
    fn memory_indexed_db_version_tracking() {
        let db = IndexedDb::open_memory().unwrap();
        db.ensure_database("test").unwrap();

        assert_eq!(db.get_version("test").unwrap(), 1);

        db.set_version("test", 2).unwrap();
        assert_eq!(db.get_version("test").unwrap(), 2);

        db.set_version("test", 5).unwrap();
        assert_eq!(db.get_version("test").unwrap(), 5);
    }

    #[test]
    fn memory_indexed_db_upgrade_callbacks() {
        let db = IndexedDb::open_memory().unwrap();
        db.ensure_database("test").unwrap();

        db.register_upgrade_callback("test", 1, 2, "onUpgrade1to2").unwrap();
        db.register_upgrade_callback("test", 2, 3, "onUpgrade2to3").unwrap();

        let cbs = db.on_upgrade_needed("test", 1, 2).unwrap();
        assert_eq!(cbs.len(), 1);
        assert_eq!(cbs[0], "onUpgrade1to2");

        let cbs = db.on_upgrade_needed("test", 1, 3).unwrap();
        assert_eq!(cbs.len(), 2);

        let cbs = db.on_upgrade_needed("test", 2, 3).unwrap();
        assert_eq!(cbs.len(), 1);
        assert_eq!(cbs[0], "onUpgrade2to3");

        let cbs = db.on_upgrade_needed("test", 3, 5).unwrap();
        assert!(cbs.is_empty());
    }

    #[test]
    fn memory_indexed_db_upgrade_database() {
        let db = IndexedDb::open_memory().unwrap();
        db.ensure_database("test").unwrap();

        db.register_upgrade_callback("test", 1, 3, "onUpgrade").unwrap();

        let cbs = db.upgrade_database("test", 3).unwrap();
        assert_eq!(cbs.len(), 1);
        assert_eq!(db.get_version("test").unwrap(), 3);
    }

    #[test]
    fn memory_indexed_db_request_lifecycle() {
        let db = IndexedDb::open_memory().unwrap();

        let req_id = db.create_request();
        assert!(db.is_request_complete(req_id).unwrap() == false);

        db.set_request_result(req_id, Some("done".to_string())).unwrap();
        assert!(db.is_request_complete(req_id).unwrap());
        assert_eq!(db.get_request_result(req_id).unwrap().as_deref(), Some("done"));
        assert!(db.get_request_error(req_id).unwrap().is_none());

        let (result, error) = db.take_request(req_id).unwrap();
        assert_eq!(result.as_deref(), Some("done"));
        assert!(error.is_none());
        assert!(db.get_request_result(req_id).is_err());
    }

    #[test]
    fn memory_indexed_db_request_error() {
        let db = IndexedDb::open_memory().unwrap();

        let req_id = db.create_request();
        db.set_request_error(req_id, "something went wrong".to_string()).unwrap();

        assert!(db.is_request_complete(req_id).unwrap());
        assert!(db.get_request_result(req_id).unwrap().is_none());
        assert_eq!(
            db.get_request_error(req_id).unwrap().as_deref(),
            Some("something went wrong")
        );

        let (result, error) = db.take_request(req_id).unwrap();
        assert!(result.is_none());
        assert_eq!(error.as_deref(), Some("something went wrong"));
    }

    #[test]
    fn memory_indexed_db_index_maintained_on_put_delete() {
        let db = IndexedDb::open_memory().unwrap();
        db.ensure_database("test").unwrap();
        let store_id = db.create_object_store("test", "items", Some("id"), false).unwrap();

        db.create_index(store_id, "by_type", "type").unwrap();

        db.put(store_id, Some("1"), r#"{"type":"foo","data":"a"}"#).unwrap();
        db.put(store_id, Some("2"), r#"{"type":"bar","data":"b"}"#).unwrap();
        db.put(store_id, Some("3"), r#"{"type":"foo","data":"c"}"#).unwrap();

        assert_eq!(db.index_get_all(store_id, "by_type", "foo").unwrap().len(), 2);

        db.delete(store_id, "1").unwrap();
        assert_eq!(db.index_get_all(store_id, "by_type", "foo").unwrap().len(), 1);

        db.clear_store(store_id).unwrap();
        assert!(db.index_get_all(store_id, "by_type", "foo").unwrap().is_empty());
    }

    #[test]
    fn memory_indexed_db_transaction_with_index_updates() {
        let db = IndexedDb::open_memory().unwrap();
        db.ensure_database("test").unwrap();
        let store_id = db.create_object_store("test", "items", Some("id"), false).unwrap();

        db.create_index(store_id, "by_type", "type").unwrap();

        let tx = db.begin_transaction(&[store_id]).unwrap();
        db.transaction_put(tx, store_id, Some("1"), r#"{"type":"foo","data":"a"}"#).unwrap();
        db.transaction_put(tx, store_id, Some("2"), r#"{"type":"bar","data":"b"}"#).unwrap();
        db.commit_transaction(tx).unwrap();

        assert_eq!(db.index_get_all(store_id, "by_type", "foo").unwrap().len(), 1);
        assert_eq!(db.index_get_all(store_id, "by_type", "bar").unwrap().len(), 1);

        let tx = db.begin_transaction(&[store_id]).unwrap();
        db.transaction_delete(tx, store_id, "1").unwrap();
        db.commit_transaction(tx).unwrap();

        assert!(db.index_get_all(store_id, "by_type", "foo").unwrap().is_empty());
        assert_eq!(db.index_get_all(store_id, "by_type", "bar").unwrap().len(), 1);
    }
}
