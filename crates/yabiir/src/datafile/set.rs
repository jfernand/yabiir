//! Read access to every data file of a store.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use super::{ActiveFile, name, pread_exact};
use crate::sync::{read, write};

/// One cached read handle per data file, the active file included.
///
/// A handle is cached *before* any keydir entry can point into its file:
/// files that exist at open are [`register`](Self::register)ed then, and
/// every file created later goes through
/// [`create_active`](Self::create_active), which caches the handle as it
/// creates the file. So [`handle`](Self::handle) normally opens nothing, and
/// a reader can take a handle while it holds the keydir's lock without doing
/// I/O under it. Handles stay cached until a merge
/// [`forget`](Self::forget)s the files it removed.
pub struct DataFileSet {
    dir: PathBuf,
    /// Read-locked by every `get`, write-locked only when a file is created
    /// or removed.
    readers: RwLock<HashMap<u32, Arc<File>>>,
}

impl DataFileSet {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            readers: RwLock::new(HashMap::new()),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn data_path(dir: &Path, file_id: u32) -> PathBuf {
        name::data_path(dir, file_id)
    }

    pub fn hint_path(dir: &Path, file_id: u32) -> PathBuf {
        name::hint_path(dir, file_id)
    }

    /// The id of every data file in `dir`, ascending.
    pub fn discover(dir: &Path) -> io::Result<Vec<u32>> {
        name::discover(dir)
    }

    /// Create data file `file_id`, open it for appending, and cache a read
    /// handle for it before anything is written to it. Every file that will
    /// receive entries (each active file, each merge output) is created here.
    ///
    /// A hint file left over under the same id, from a file that is gone,
    /// is removed first: it would describe the wrong data.
    pub fn create_active(&self, file_id: u32) -> io::Result<ActiveFile> {
        remove_if_present(&name::hint_path(&self.dir, file_id))?;
        let active = ActiveFile::create(&self.dir, file_id)?;
        let reader = Arc::new(active.reader()?);
        write(&self.readers).insert(file_id, reader);
        Ok(active)
    }

    /// Open and cache read handles for files that already exist. Called once
    /// at open, before the keydir is shared.
    pub fn register(&self, file_ids: &[u32]) -> io::Result<()> {
        let mut readers = write(&self.readers);
        for &file_id in file_ids {
            if let Entry::Vacant(slot) = readers.entry(file_id) {
                slot.insert(Arc::new(self.open(file_id)?));
            }
        }
        Ok(())
    }

    /// The read handle for `file_id`.
    ///
    /// **Take it while the keydir entry that points into `file_id` is still
    /// current**, that is, under the keydir's read lock. Merge removes a file
    /// only after it has repointed every key that pointed into it, and
    /// repointing takes the keydir's write lock, so a handle taken under the
    /// read lock is taken before the file can be removed. An open descriptor
    /// keeps a file's contents readable after `unlink`, so the read that
    /// follows succeeds even if merge removes the file in between.
    ///
    /// A file whose handle isn't cached is opened here. The registration
    /// rules above make that the exception.
    pub fn handle(&self, file_id: u32) -> io::Result<Arc<File>> {
        if let Some(file) = read(&self.readers).get(&file_id) {
            return Ok(Arc::clone(file));
        }
        let file = Arc::new(self.open(file_id)?);
        Ok(Arc::clone(
            write(&self.readers)
                .entry(file_id)
                .or_insert(file),
        ))
    }

    /// Read `len` bytes at `pos` of data file `file_id`. The cache's lock is
    /// held only to fetch the handle, never across the read. Not safe against
    /// a concurrent merge on its own: see [`handle`](Self::handle).
    pub fn read_at(&self, file_id: u32, pos: u64, len: u32) -> io::Result<Vec<u8>> {
        pread_exact(&*self.handle(file_id)?, pos, len)
    }

    /// The size of data file `file_id` on disk.
    pub(crate) fn len_of(&self, file_id: u32) -> io::Result<u64> {
        Ok(fs::metadata(name::data_path(&self.dir, file_id))?.len())
    }

    /// Remove data file `file_id` and its hint file, and drop the cached read
    /// handle. The file's blocks are freed once the last reader that took a
    /// handle earlier drops it. A file that is already gone is not an error.
    pub(crate) fn remove(&self, file_id: u32) -> io::Result<()> {
        // The hint first, so that a failure in between can't leave a hint
        // file behind without its data file.
        remove_if_present(&name::hint_path(&self.dir, file_id))?;
        remove_if_present(&name::data_path(&self.dir, file_id))?;
        self.forget(file_id);
        Ok(())
    }

    /// Drop the cached read handle for `file_id`, if there is one.
    pub fn forget(&self, file_id: u32) {
        write(&self.readers).remove(&file_id);
    }

    /// Make the directory's list of files durable. See `name::sync_dir`.
    pub(crate) fn sync_dir(&self) -> io::Result<()> {
        name::sync_dir(&self.dir)
    }

    fn open(&self, file_id: u32) -> io::Result<File> {
        File::open(name::data_path(&self.dir, file_id))
    }
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}
