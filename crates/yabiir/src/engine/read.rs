//! The read path: `get`, `fold`, `list_keys` and `scan`.
//!
//! A read is two steps: look the key up in the keydir, then read the value
//! from the file the entry names. No lock is held across the second step, so
//! a merge can run in between, repoint the key and remove that file. Every
//! read here therefore takes the file's read handle during the first step,
//! under the keydir's read lock; an open handle keeps the file readable (see
//! [`DataFileSet::handle`]).

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs::File;
use std::ops::Bound;
use std::sync::Arc;
use std::time::Instant;

use super::Engine;
use crate::datafile::{DataFileSet, pread_exact};
use crate::error::{Error, Result};
use crate::keydir::KeydirEntry;

/// Keydir entries copied out under the keydir's read lock, with a read
/// handle for every data file they point into.
struct Pinned {
    entries: Vec<(Box<[u8]>, KeydirEntry)>,
    files: HashMap<u32, Arc<File>>,
}

impl Pinned {
    /// Copy `entries` out and take a handle for each file they point into.
    /// Call it under the keydir's read lock. It does no disk I/O: every
    /// handle is already cached.
    fn pin<'k>(
        files: &DataFileSet,
        entries: impl Iterator<Item = (&'k [u8], &'k KeydirEntry)>,
    ) -> Result<Self> {
        let mut pinned = Self {
            entries: Vec::new(),
            files: HashMap::new(),
        };
        for (key, entry) in entries {
            if let Entry::Vacant(slot) = pinned
                .files
                .entry(entry.file_id)
            {
                slot.insert(files.handle(entry.file_id)?);
            }
            pinned
                .entries
                .push((Box::from(key), *entry));
        }
        Ok(pinned)
    }

    /// Read each pinned entry's value, in the order the entries were pinned.
    fn read(self) -> impl Iterator<Item = Result<(Box<[u8]>, Vec<u8>)>> {
        let files = self.files;
        self.entries
            .into_iter()
            .map(move |(key, entry)| {
                let value = read_value(&files[&entry.file_id], entry)?;
                Ok((key, value))
            })
    }
}

/// Read the value `entry` points at through `file`, a handle taken for
/// `entry.file_id` while `entry` was current. It reads exactly the value's
/// bytes: no header, and no CRC check on the read path.
fn read_value(file: &File, entry: KeydirEntry) -> Result<Vec<u8>> {
    Ok(pread_exact(file, entry.value_pos, entry.value_size)?)
}

impl Engine {
    /// [`Bitcask::get`](crate::Bitcask::get), with `between_lookup_and_read`
    /// run after the keydir lookup and before the value is read. Production
    /// passes a no-op; tests run a whole merge there.
    pub(super) fn get_with_hook(
        &self,
        key: &[u8],
        between_lookup_and_read: impl FnOnce(KeydirEntry),
    ) -> Result<Option<Vec<u8>>> {
        let start = Instant::now();
        let found = self
            .keydir
            .with_read(|keydir| {
                keydir
                    .get(key)
                    .map(|entry| {
                        self.files
                            .handle(entry.file_id)
                            .map(|file| (entry, file))
                    })
                    .transpose()
            })?;
        let value = match found {
            Some((entry, file)) => {
                between_lookup_and_read(entry);
                Some(read_value(&file, entry)?)
            }
            None => None,
        };
        self.metrics
            .record_get(start.elapsed());
        Ok(value)
    }

    /// [`Bitcask::fold`](crate::Bitcask::fold), with `after_snapshot` run
    /// once the keydir snapshot is taken and before any value is read: the
    /// same window as [`Engine::get_with_hook`], held open for the whole
    /// fold.
    pub(super) fn fold_with_hook<A>(
        &self,
        mut f: impl FnMut(&[u8], &[u8], A) -> A,
        init: A,
        after_snapshot: impl FnOnce(),
    ) -> Result<A> {
        let pinned = self
            .keydir
            .with_read(|keydir| Pinned::pin(&self.files, keydir.iter()))?;
        after_snapshot();
        let mut acc = init;
        for pair in pinned.read() {
            let (key, value) = pair?;
            acc = f(&key, &value, acc);
        }
        Ok(acc)
    }

    /// Every live key, as of one instant.
    pub(super) fn keys(&self) -> Vec<Vec<u8>> {
        self.keydir
            .with_read(|keydir| {
                keydir
                    .iter()
                    .map(|(key, _)| key.to_vec())
                    .collect()
            })
    }

    /// Every live `(key, value)` pair whose key lies between `start` and
    /// `end`, in key order: one descent into the keydir and a walk, instead
    /// of a `fold` over every key. Needs a sorted keydir, that is, a store
    /// opened with [`KeyOrder::Sorted`](crate::KeyOrder::Sorted); on a hashed
    /// one it returns [`Error::Unordered`]. Like `fold`, it reads the keys
    /// in range as of one instant, and is safe against a concurrent merge.
    pub fn scan(&self, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let pinned = self
            .keydir
            .with_read(|keydir| match keydir.range(start, end) {
                Some(range) => Pinned::pin(&self.files, range),
                None => Err(Error::Unordered),
            })?;
        pinned
            .read()
            .map(|pair| pair.map(|(key, value)| (key.into_vec(), value)))
            .collect()
    }
}
