use std::fmt;
use std::path::Path;
use std::sync::Arc;

use crate::error::Result;
use crate::metrics::Metrics;

/// How the keydir — the in-memory index from each key to its newest value —
/// keeps its keys. Chosen per open: nothing about it is stored on disk, so the
/// same store can be opened either way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KeyOrder {
    /// An aHash `HashMap`: the fastest point lookups (`get`, `put`, `delete`),
    /// keys in no particular order, and no [`crate::Engine::scan`].
    #[default]
    Hashed,
    /// A `BTreeMap`: keys in byte order, so [`crate::Engine::scan`] answers a
    /// range of keys with one descent and a walk, and `fold` / `list_keys`
    /// return keys in order. Point lookups compare whole keys on the way down,
    /// so they cost more than with [`KeyOrder::Hashed`] (`benches/keydir_order.rs`
    /// measures by how much), and so does rebuilding the keydir at open.
    Sorted,
}

/// How to open a store. See `docs/bitcask-implementation-plan.md` §9 for the
/// reasoning behind each field.
#[derive(Clone)]
pub struct Options {
    /// Open for reading and writing (`true`) or read-only (`false`). Only
    /// one read-write handle can be open on a directory at a time; a lock
    /// file in the directory enforces it.
    pub is_read_write: bool,
    /// Whether `put` and `delete` wait until the entry is fsynced before
    /// they return. Safer and slower. Concurrent writers share fsyncs
    /// (group commit). Without it, a write survives a crash of the process
    /// but not necessarily one of the machine.
    pub should_sync_on_put: bool,
    /// The size, in bytes, at which the active file is closed and a new one
    /// started. A file can exceed it by one entry.
    pub max_file_size: u64,
    /// Observe `put`/`get`/`delete`/`merge`/`sync` call durations — `None`
    /// (the default) records nothing. See [`Metrics`].
    pub metrics: Option<Arc<dyn Metrics>>,
    /// How the keydir keeps its keys — see [`KeyOrder`]. Defaults to
    /// [`KeyOrder::Hashed`].
    pub key_order: KeyOrder,
}

impl Options {
    /// These options, for a read-only handle.
    pub fn read_only(self) -> Self {
        Self {
            is_read_write: false,
            ..self
        }
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            is_read_write: true,
            should_sync_on_put: false,
            max_file_size: 64 * 1024 * 1024,
            metrics: None,
            key_order: KeyOrder::Hashed,
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
            .field("key_order", &self.key_order)
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
/// its newest value. See `refs/bitcask-intro.pdf` and
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
    /// Open the store in `dir`, rebuilding the keydir from its files (from
    /// hint files where there are valid ones) before returning.
    ///
    /// A read-write open creates the directory if it is missing and takes
    /// an exclusive lock on it; it fails with [`Error::AlreadyLocked`] if
    /// another read-write handle, in this process or another, holds the
    /// lock. A read-only open ([`Options::read_only`]) takes no lock and
    /// creates nothing. It sees the store as it was when it was opened, not
    /// later writes, and shouldn't be used while a writer merges, which
    /// removes files under it.
    ///
    /// [`Error::AlreadyLocked`]: crate::Error::AlreadyLocked
    fn open(dir: impl AsRef<Path>, opts: Options) -> Result<Self>;

    /// The current value of `key`, or `Ok(None)` if it has none.
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>>;

    /// Store `value` under `key`, replacing any value it had.
    ///
    /// Fails with [`Error::EmptyKey`] for an empty key,
    /// [`Error::KeyTooLarge`] or [`Error::ValueTooLarge`] for a key or
    /// value the file format can't record, and [`Error::ReadOnly`] on a
    /// read-only handle. On an I/O error the write may or may not have
    /// taken effect.
    ///
    /// [`Error::EmptyKey`]: crate::Error::EmptyKey
    /// [`Error::KeyTooLarge`]: crate::Error::KeyTooLarge
    /// [`Error::ValueTooLarge`]: crate::Error::ValueTooLarge
    /// [`Error::ReadOnly`]: crate::Error::ReadOnly
    fn put(&self, key: &[u8], value: &[u8], timestamp: u32) -> Result<()>;

    /// Remove `key`. Deleting a key that has no value is not an error. The
    /// space the value took is reclaimed by the next [`Bitcask::merge`].
    fn delete(&self, key: &[u8], timestamp: u32) -> Result<()>;

    /// Every live key, as of one instant.
    fn list_keys(&self) -> Result<Vec<Vec<u8>>>;

    /// Fold `f` over every live `(key, value)` pair, starting from `init`.
    /// The keys and the values' locations are those of one instant, at the
    /// start of the call; writes made during the fold aren't seen.
    fn fold<A>(&self, f: impl FnMut(&[u8], &[u8], A) -> A, init: A) -> Result<A>;

    /// Compact the store: copy the live entries of every data file into
    /// new files, each with a hint file that speeds up the next open, and
    /// remove the old files. Replaced values, deleted keys and their
    /// tombstones are dropped.
    ///
    /// Safe to call while other threads read and write. A write made
    /// during a merge wins over the merge's copy of the same key, in this
    /// session and after a reopen. Concurrent calls run one after another.
    fn merge(&self) -> Result<()>;

    /// Make every write so far durable: flush and fsync the active file. A
    /// no-op on a read-only handle.
    fn sync(&self) -> Result<()>;

    /// Close this handle: make every write durable, as [`Bitcask::sync`]
    /// does, and release the directory's lock. Dropping a handle releases
    /// the lock too, but doesn't fsync and can't report an error.
    fn close(self) -> Result<()>;
}
