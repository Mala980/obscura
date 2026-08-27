use anyhow::Result;

/// Common storage backend trait shared by localStorage and IndexedDB.
pub trait StorageBackend: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>>;
    fn set(&mut self, key: &str, value: &str) -> Result<()>;
    fn remove(&mut self, key: &str) -> Result<()>;
    fn clear(&mut self) -> Result<()>;
    fn keys(&self) -> Result<Vec<String>>;
    fn len(&self) -> Result<usize>;
    fn contains_key(&self, key: &str) -> Result<bool>;
}
