use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;
use std::sync::Mutex;

use crate::traits::StorageBackend;

/// localStorage implementation backed by SQLite (servo-storage parity).
///
/// Each origin gets its own database. The table schema mirrors the
/// Web Storage API: key-value pairs with a unique key constraint.
pub struct LocalStorage {
    conn: Mutex<Connection>,
}

impl LocalStorage {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS storage (
                key   TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS storage (
                key   TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

impl StorageBackend for LocalStorage {
    fn get(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt = conn.prepare("SELECT value FROM storage WHERE key = ?1")?;
        let mut rows = stmt.query([key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    fn set(&mut self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "INSERT OR REPLACE INTO storage (key, value) VALUES (?1, ?2)",
            [key, value],
        )?;
        Ok(())
    }

    fn remove(&mut self, key: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute("DELETE FROM storage WHERE key = ?1", [key])?;
        Ok(())
    }

    fn clear(&mut self) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute("DELETE FROM storage", [])?;
        Ok(())
    }

    fn keys(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt = conn.prepare("SELECT key FROM storage")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        let mut keys = Vec::new();
        for row in rows {
            keys.push(row?);
        }
        Ok(keys)
    }

    fn len(&self) -> Result<usize> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM storage", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    fn contains_key(&self, key: &str) -> Result<bool> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt =
            conn.prepare("SELECT 1 FROM storage WHERE key = ?1 LIMIT 1")?;
        let mut rows = stmt.query([key])?;
        Ok(rows.next()?.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_local_storage_crud() {
        let mut store = LocalStorage::open_memory().unwrap();
        assert!(store.get("missing").unwrap().is_none());
        store.set("hello", "world").unwrap();
        assert_eq!(store.get("hello").unwrap().as_deref(), Some("world"));
        assert!(store.contains_key("hello").unwrap());
        assert_eq!(store.len().unwrap(), 1);
        store.remove("hello").unwrap();
        assert!(store.get("hello").unwrap().is_none());
    }

    #[test]
    fn memory_local_storage_overwrite() {
        let mut store = LocalStorage::open_memory().unwrap();
        store.set("k", "v1").unwrap();
        store.set("k", "v2").unwrap();
        assert_eq!(store.get("k").unwrap().as_deref(), Some("v2"));
    }

    #[test]
    fn memory_local_storage_clear() {
        let mut store = LocalStorage::open_memory().unwrap();
        store.set("a", "1").unwrap();
        store.set("b", "2").unwrap();
        assert_eq!(store.len().unwrap(), 2);
        store.clear().unwrap();
        assert_eq!(store.len().unwrap(), 0);
    }
}
