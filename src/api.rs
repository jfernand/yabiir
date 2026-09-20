use std::fmt;
use std::path::Path;
use std::sync::Arc;

use crate::error::Result;
use crate::metrics::Metrics;

/// Tuning/mode knobs for opening a [`Bitcask`] datastore.
///
/// See `docs/bitcask-implementation-plan.md` §9 for the rationale behind
/// each field.
#[derive(Clone)]
pub struct Options {
    /// Open for reading and writing (`true`) or read-only (`false`).
    /// Only one `read_write` handle may be open on a given directory at a
    /// time, enforced via a process-level directory lock.
    pub is_read_write: bool,
    /// Fsync the active file after every `put`/`delete`. Safer, slower.
    pub should_sync_on_put: bool,
    /// Size (bytes) at which the active file is rotated and a new one
    /// started.
    pub max_file_size: u64,
    /// Observe `put`/`get`/`delete`/`merge`/`sync` call durations — `None`
    /// (the default) records nothing. See [`Metrics`].
    pub metrics: Option<Arc<dyn Metrics>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            is_read_write: true,
            should_sync_on_put: false,
            max_file_size: 64 * 1024 * 1024,
            metrics: None,
        }
    }
}

// Hand-written: `dyn Metrics` has no reason to require `Debug` of its own
// (it's a handful of duration-recording callbacks, not data to print), so
// `#[derive(Debug)]` doesn't apply here.
impl fmt::Debug for Options {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Options")
            .field("is_read_write", &self.is_read_write)
            .field("should_sync_on_put", &self.should_sync_on_put)
            .field("max_file_size", &self.max_file_size)
            .field(
                "metrics",
                &self
                    .metrics
                    .as_ref()
                    .map_or("None", |_| "Some(..)"),
            )
            .finish()
    }
}

/// The Bitcask log-structured hash table API.
///
/// A `Bitcask` instance is a directory of append-only data files plus an
/// in-memory index (the "keydir") mapping each live key to the location of
/// its newest value. See `docs/papers/bitcask-intro.pdf` and
/// `docs/bitcask-implementation-plan.md` for the full design this trait is
/// derived from.
///
/// This mirrors the paper's Erlang API (`bitcask:open/get/put/delete/
/// list_keys/fold/merge/sync/close`), with one deliberate deviation: `merge`
/// here is a method on an already-open handle (it needs direct access to
/// the live keydir to repoint entries safely) rather than a free function
/// over a bare directory path, since this design has no Erlang-VM-style
/// cross-process keydir sharing to lean on.
pub trait Bitcask: Sized {
    /// Open a new or existing Bitcask datastore at `dir`, creating the
    /// directory if it does not exist.
    ///
    /// If `opts.read_write` is `true`, this acquires an exclusive
    /// process-level lock on the directory and fails with
    /// [`crate::error::Error::AlreadyLocked`] if another `read_write` handle
    /// already holds it. Recovery (rebuilding the keydir from the on-disk
    /// files, preferring hint files where available) happens synchronously
    /// as part of this call.
    fn open(dir: impl AsRef<Path>, opts: Options) -> Result<Self>;

    /// Retrieve the current value for `key`, or `Ok(None)` if the key does
    /// not exist (or has been deleted).
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>>;

    /// Store `value` under `key`, appending a new entry to the active file
    /// and updating the keydir. Overwrites any existing value for `key`.
    ///
    /// Returns [`crate::error::Error::EmptyKey`] if `key` is empty, and
    /// [`crate::error::Error::ReadOnly`] if this handle was opened with
    /// `read_write: false`.
    fn put(&self, key: &[u8], value: &[u8], timestamp: u32) -> Result<()>;

    /// Remove `key`, appending a tombstone entry and evicting `key` from the
    /// keydir. A no-op (not an error) if `key` does not currently exist.
    /// Space used by the deleted value and its tombstone is reclaimed by a
    /// subsequent [`Bitcask::merge`].
    fn delete(&self, key: &[u8], timestamp: u32) -> Result<()>;

    /// Return a snapshot of every currently-live key.
    fn list_keys(&self) -> Result<Vec<Vec<u8>>>;

    /// Fold `f` over every currently-live `(key, value)` pair, starting
    /// from `init`. `f` is applied over a point-in-time snapshot of the
    /// keydir taken at the start of the call; concurrent writes made during
    /// the fold are not guaranteed to be observed.
    fn fold<A>(&self, f: impl FnMut(&[u8], &[u8], A) -> A, init: A) -> Result<A>;

    /// Compact this datastore: copy forward only the live entries from all
    /// non-active data files into a fresh, smaller set of data files (each
    /// with an accompanying hint file for fast future recovery), then
    /// remove the old files. Dead entries — superseded values and
    /// tombstones — are dropped. Safe to call concurrently with ongoing
    /// `put`/`get`/`delete` calls; a key written concurrently with the
    /// merge is never lost.
    fn merge(&self) -> Result<()>;

    /// Force any buffered writes to be durably flushed to disk.
    fn sync(&self) -> Result<()>;

    /// Close this handle, flushing pending writes and releasing the
    /// directory lock (if held). Equivalent to dropping the handle, but
    /// lets close-time I/O errors be observed instead of ignored.
    fn close(self) -> Result<()>;
}
