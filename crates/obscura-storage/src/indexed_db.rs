use anyhow::Result;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;

use crate::traits::StorageBackend;

/// IndexedDB implementation backed by SQLite (servo-storage parity).
///
/// Each database has a name and contains one or more object stores.
/// This implementation provides a simplified but functional IndexedDB:
/// - Object stores with auto-incrementing keys
/// - put/get/delete/clear operations
/// - Key-value storage with structured JSON values
/// - Transaction support via SQLite's ACID guarantees
pub struct IndexedDb {
    conn: Mutex<Connection>,
}

impl IndexedDb {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS databases (
                name TEXT PRIMARY KEY NOT NULL
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
            PRAGMA foreign_keys = ON;",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS databases (
                name TEXT PRIMARY KEY NOT NULL
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
            PRAGMA foreign_keys = ON;",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn ensure_database(&self, name: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "INSERT OR IGNORE INTO databases (name) VALUES (?1)",
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
