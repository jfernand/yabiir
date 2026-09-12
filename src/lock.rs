//! Process-level single-writer lock: an OS `flock` on a `.bitcask.lock`
//! file in the datastore directory, held for the lifetime of a
//! `read_write` handle. See `docs/bitcask-implementation-plan.md` §8.1.
//!
//! Only ever acquired for `read_write` opens — read-only opens never touch
//! this and can coexist with each other and with the single writer. This
//! is a best-effort guard for the common local-disk case (matching the
//! paper's single-machine assumption), not a substitute for real mutual
//! exclusion on filesystems with unreliable `flock` semantics — NFS in
//! particular is notorious for this; don't rely on this lock if the
//! directory might ever live on one.

use std::fs::{File, OpenOptions};
use std::path::Path;

use fs2::FileExt;

use crate::error::{Error, Result};

/// Holds the directory's exclusive write lock for as long as it's alive.
/// No explicit `unlock`/`Drop` impl needed: dropping the underlying `File`
/// closes its fd, which releases the OS-level `flock` automatically.
pub struct DirLock(#[allow(dead_code)] File);

impl DirLock {
    /// Acquire the exclusive lock for `dir`, creating `.bitcask.lock` if it
    /// doesn't already exist. Fails with [`Error::AlreadyLocked`] if
    /// another `read_write` handle — in this process or another — already
    /// holds it.
    pub fn acquire(dir: &Path) -> Result<Self> {
        let path = dir.join(".bitcask.lock");
        let file = OpenOptions::new().create(true).write(true).open(&path)?;
        file.try_lock_exclusive().map_err(|_| Error::AlreadyLocked)?;
        Ok(Self(file))
    }
}
