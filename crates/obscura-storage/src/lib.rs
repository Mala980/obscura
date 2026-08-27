pub mod local_storage;
pub mod indexed_db;
pub mod traits;

pub use local_storage::LocalStorage;
pub use indexed_db::IndexedDb;
pub use traits::StorageBackend;
