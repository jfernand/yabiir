//! The keydir behind a lock.

use std::sync::RwLock;

use super::{Keydir, KeydirEntry};
use crate::sync::{read, write};

/// A [`Keydir`] shared between threads: lookups take a read lock, changes a
/// write lock. One `RwLock` around the whole map keeps every change atomic
/// and in one order, which the single-writer design depends on (ROADMAP §3
/// explains why the map isn't sharded).
#[derive(Default)]
pub struct SharedKeydir(RwLock<Keydir>);

impl SharedKeydir {
    pub fn new(keydir: Keydir) -> Self {
        Self(RwLock::new(keydir))
    }

    pub fn get(&self, key: &[u8]) -> Option<KeydirEntry> {
        read(&self.0).get(key)
    }

    pub fn insert(&self, key: &[u8], entry: KeydirEntry) {
        write(&self.0).insert(key, entry);
    }

    pub fn remove(&self, key: &[u8]) -> Option<KeydirEntry> {
        write(&self.0).remove(key)
    }

    pub fn len(&self) -> usize {
        read(&self.0).len()
    }

    pub fn is_empty(&self) -> bool {
        read(&self.0).is_empty()
    }

    /// See [`Keydir::cas_repoint`].
    pub fn cas_repoint(&self, key: &[u8], expected_old: KeydirEntry, new: KeydirEntry) -> bool {
        write(&self.0).cas_repoint(key, expected_old, new)
    }

    /// Run `f` with the keydir under its read lock, and return what it
    /// returns. For a reader that has to act on an entry while the entry is
    /// still current: `get`, `fold` and `scan` take the read handle for an
    /// entry's file here, so merge can't remove the file first (see
    /// `DataFileSet::handle`). `f` must not do disk I/O: the lock blocks
    /// every writer while it runs.
    pub fn with_read<R>(&self, f: impl FnOnce(&Keydir) -> R) -> R {
        f(&read(&self.0))
    }

    /// A copy of every `(key, entry)` pair as of one instant.
    pub fn snapshot(&self) -> Vec<(Box<[u8]>, KeydirEntry)> {
        read(&self.0)
            .iter()
            .map(|(key, entry)| (Box::from(key), *entry))
            .collect()
    }
}
