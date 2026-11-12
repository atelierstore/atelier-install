use std::{collections::HashMap, sync::Mutex};

use base64::prelude::*;
use rootcause::prelude::*;

/// A trait for storing and retrieving sideloading related data, such as anisette state and certificates.
pub trait SideloadingStorage: Send + Sync {
    fn store(&self, key: &str, value: &str) -> Result<(), Report>;
    fn retrieve(&self, key: &str) -> Result<Option<String>, Report>;

    fn store_data(&self, key: &str, value: &[u8]) -> Result<(), Report> {
        self.store(key, &BASE64_STANDARD.encode(value))
    }

    fn retrieve_data(&self, key: &str) -> Result<Option<Vec<u8>>, Report> {
        if let Some(value) = self.retrieve(key)? {
            Ok(Some(BASE64_STANDARD.decode(value)?))
        } else {
            Ok(None)
        }
    }

    fn delete(&self, key: &str) -> Result<(), Report> {
        self.store(key, "")
    }
}

/// Factory function to create a new storage instance based on enabled features: `fs-storage`, or else an in-memory storage. (This copy has no keyring storage.)
pub fn new_storage() -> impl SideloadingStorage {
    #[cfg(feature = "fs-storage")]
    {
        return crate::util::fs_storage::FsStorage::default();
    }
    #[cfg(not(feature = "fs-storage"))]
    {
        tracing::warn!(
            "Fs storage not enabled, falling back to in-memory storage. This means that the anisette state and certificates will not be saved across runs. Enable the 'fs-storage' feature for persistance."
        );
        return InMemoryStorage::new();
    }
}

pub struct InMemoryStorage {
    storage: Mutex<HashMap<String, String>>,
}

impl Default for InMemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryStorage {
    pub fn new() -> Self {
        InMemoryStorage {
            storage: Mutex::new(HashMap::new()),
        }
    }
}

impl SideloadingStorage for InMemoryStorage {
    fn store(&self, key: &str, value: &str) -> Result<(), Report> {
        let mut storage = self
            .storage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        storage.insert(key.to_string(), value.to_string());
        Ok(())
    }

    fn retrieve(&self, key: &str) -> Result<Option<String>, Report> {
        let storage = self
            .storage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Ok(storage.get(key).cloned())
    }

    fn delete(&self, key: &str) -> Result<(), Report> {
        let mut storage = self
            .storage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        storage.remove(key);
        Ok(())
    }
}
