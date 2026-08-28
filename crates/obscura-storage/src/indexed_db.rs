use anyhow::Result;
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use crate::traits::StorageBackend;

/// A cursor position for iterating over store data.
pub struct Cursor {
    pub store_id: i64,
    pub position: usize,
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
pub struct IndexedDb {
    conn: Mutex<Connection>,
    /// Active transactions: tx_id -> (active, staged operations).
    /// When a transaction is active, writes are staged in memory and only
    /// flushed on commit. Abort discards them.
    transactions: Mutex<HashMap<i64, TransactionContext>>,
    /// Open cursors: cursor_id -> Cursor state.
    cursors: Mutex<HashMap<i64, Cursor>>,
    /// Monotonic counter for transaction and cursor ids.
    next_id: std::sync::atomic::AtomicI64,
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

    pub fn put(
        &self,
        store_id: i64,
        key: Option<&str>,
        value: &str,
    ) -> Result<String> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        if let Some(k) = key {
            conn.execute(
                "INSERT OR REPLACE INTO store_data (store_id, key_val, value)
                 VALUES (?1, ?2, ?3)",
                params![store_id, k, value],
            )?;
            Ok(k.to_string())
        } else {
            conn.execute(
                "INSERT INTO store_data (store_id, value) VALUES (?1, ?2)",
                params![store_id, value],
            )?;
            let row_id: i64 = conn.last_insert_rowid();
            Ok(row_id.to_string())
        }
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
        let affected = conn.execute(
            "DELETE FROM store_data WHERE store_id = ?1 AND key_val = ?2",
            params![store_id, key],
        )?;
        Ok(affected > 0)
    }

    pub fn clear_store(&self, store_id: i64) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
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
    /// either all writes succeed or none do.
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
                        tx.execute(
                            "INSERT OR REPLACE INTO store_data (store_id, key_val, value)
                             VALUES (?1, ?2, ?3)",
                            params![store_id, k, value],
                        )?;
                    } else {
                        tx.execute(
                            "INSERT INTO store_data (store_id, value) VALUES (?1, ?2)",
                            params![store_id, value],
                        )?;
                    }
                }
                StagedOp::Delete { store_id, key } => {
                    tx.execute(
                        "DELETE FROM store_data WHERE store_id = ?1 AND key_val = ?2",
                        params![store_id, key],
                    )?;
                }
                StagedOp::Clear { store_id } => {
                    tx.execute(
                        "DELETE FROM store_data WHERE store_id = ?1",
                        params![store_id],
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
        let all = self.get_all(store_id)?;
        let cursor_id = self.next_object_id();
        let cursor = Cursor { store_id, position: 0 };
        let mut cursors = self.cursors.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        cursors.insert(cursor_id, cursor);
        Ok((cursor_id, all))
    }

    /// Advance cursor by `count` positions. Returns the key-value pair at the
    /// new position, or None if the cursor has reached the end.
    pub fn cursor_advance(&self, cursor_id: i64, count: u32) -> Result<Option<(String, String)>> {
        let all = {
            let cursors = self.cursors.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
            let cursor = cursors.get(&cursor_id).ok_or_else(|| anyhow::anyhow!("unknown cursor"))?;
            self.get_all(cursor.store_id)?
        };
        let mut cursors = self.cursors.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let cursor = cursors.get_mut(&cursor_id).ok_or_else(|| anyhow::anyhow!("unknown cursor"))?;
        cursor.position = cursor.position.saturating_add(count as usize);
        if cursor.position >= all.len() {
            Ok(None)
        } else {
            Ok(Some(all[cursor.position].clone()))
        }
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
}
