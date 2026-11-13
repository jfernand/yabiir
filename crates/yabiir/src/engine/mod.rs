//! The engine: a keydir and a directory of data files behind the
//! [`Bitcask`] API. See `docs/bitcask-implementation-plan.md` §4.
//!
//! - `mod.rs`: [`Engine`], opening one, and the [`Bitcask`] impl.
//! - `write.rs`: `put` and `delete`.
//! - `read.rs`: `get`, `fold`, `list_keys` and [`Engine::scan`].
//!
//! Appending is the [`Writer`]'s job, compaction `crate::merge`'s, and
//! rebuilding the keydir at open `crate::recovery`'s.

mod read;
#[cfg(test)]
mod tests;
pub(crate) mod write;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::api::{Bitcask, KeyOrder, Options};
use crate::datafile::DataFileSet;
use crate::error::{Error, Result};
use crate::keydir::{Keydir, SharedKeydir};
use crate::lock::DirLock;
use crate::merge;
use crate::metrics::{Metrics, NoopMetrics};
use crate::recovery;
use crate::sync::lock;
use crate::writer::Writer;

/// A Bitcask store open in this process.
pub struct Engine {
    keydir: SharedKeydir,
    files: Arc<DataFileSet>,
    /// `None` for a read-only handle, which never appends.
    writer: Option<Writer>,
    /// Held by a merge for as long as it runs: one merge at a time. A second
    /// one would take the first's half-written output for input.
    merge_lock: Mutex<()>,
    metrics: Arc<dyn Metrics>,
    /// Held for the life of a read-write handle; dropping it releases the
    /// directory's lock. `None` for a read-only handle, which takes no lock.
    _dir_lock: Option<DirLock>,
}

impl Engine {
    /// The writer, or [`Error::ReadOnly`] for a read-only handle.
    fn writer(&self) -> Result<&Writer> {
        self.writer
            .as_ref()
            .ok_or(Error::ReadOnly)
    }

    /// How this store's keydir keeps its keys, as opened: a
    /// [`KeyOrder::Sorted`] store can answer [`Engine::scan`], a hashed one
    /// can't.
    pub fn key_order(&self) -> KeyOrder {
        self.keydir
            .with_read(Keydir::order)
    }

    /// How many fsyncs group commit has run, to check that writers share them.
    #[cfg(test)]
    fn fsync_call_count(&self) -> usize {
        self.writer
            .as_ref()
            .map_or(0, Writer::fsync_call_count)
    }

    /// [`Bitcask::merge`], with `before_repoint` run for each live entry
    /// after merge has copied it and before it repoints the key. Production
    /// passes a no-op; tests write to the key there.
    fn merge_with_hook(&self, before_repoint: impl FnMut(&[u8])) -> Result<()> {
        let writer = self.writer()?;
        let _one_merge_at_a_time = lock(&self.merge_lock);
        let start = Instant::now();
        merge::merge(
            &self.keydir,
            &self.files,
            writer,
            self.metrics
                .as_ref(),
            before_repoint,
        )?;
        self.metrics
            .record_merge(start.elapsed());
        Ok(())
    }
}

impl Bitcask for Engine {
    fn open(dir: impl AsRef<Path>, opts: Options) -> Result<Self> {
        let dir = dir.as_ref();

        // The lock comes before anything else: a second writer must fail
        // fast, without doing recovery's work first. A read-only handle
        // takes no lock and creates nothing, the directory included.
        let dir_lock = if opts.is_read_write {
            std::fs::create_dir_all(dir)?;
            Some(DirLock::acquire(dir)?)
        } else {
            None
        };

        let file_ids = DataFileSet::discover(dir)?;
        let mut keydir = Keydir::with_order(opts.key_order);
        recovery::recover(dir, &file_ids, &mut keydir)?;

        // Every recovered file gets its read handle now, before the keydir
        // is shared (see `DataFileSet::handle`).
        let files = Arc::new(DataFileSet::new(dir));
        files.register(&file_ids)?;

        // No existing file is appended to again, not even the one that was
        // active when the store was last closed or crashed: the writer
        // starts a new file, above every id on disk, at its first append.
        let writer = if opts.is_read_write {
            let next_file_id = match file_ids.last() {
                Some(&id) => id
                    .checked_add(1)
                    .ok_or_else(|| {
                        std::io::Error::other("the store has used every data file id")
                    })?,
                None => 0,
            };
            Some(Writer::new(
                Arc::clone(&files),
                next_file_id,
                opts.max_file_size,
                opts.should_sync_on_put,
            ))
        } else {
            None
        };

        Ok(Self {
            keydir: SharedKeydir::new(keydir),
            files,
            writer,
            merge_lock: Mutex::new(()),
            metrics: opts
                .metrics
                .unwrap_or_else(|| Arc::new(NoopMetrics)),
            _dir_lock: dir_lock,
        })
    }

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.get_with_hook(key, |_| {})
    }

    fn put(&self, key: &[u8], value: &[u8], timestamp: u32) -> Result<()> {
        self.put_with_hook(key, value, timestamp, || {})
    }

    fn delete(&self, key: &[u8], timestamp: u32) -> Result<()> {
        self.delete_with_hook(key, timestamp, || {})
    }

    fn list_keys(&self) -> Result<Vec<Vec<u8>>> {
        Ok(self.keys())
    }

    fn fold<A>(&self, f: impl FnMut(&[u8], &[u8], A) -> A, init: A) -> Result<A> {
        self.fold_with_hook(f, init, || {})
    }

    fn merge(&self) -> Result<()> {
        self.merge_with_hook(|_| {})
    }

    fn sync(&self) -> Result<()> {
        let Some(writer) = &self.writer else {
            return Ok(());
        };
        let start = Instant::now();
        writer.sync()?;
        self.metrics
            .record_sync(start.elapsed());
        Ok(())
    }

    fn close(self) -> Result<()> {
        if let Some(writer) = self.writer {
            writer.close()?;
        }
        Ok(())
    }
}
