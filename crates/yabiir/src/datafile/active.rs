//! The file being appended to.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;

use super::{name, pread_exact};
use crate::format::EncodedEntry;

/// A data file open for appending. It tracks the end of the file itself, so
/// it knows where each entry lands without asking the OS.
pub struct ActiveFile {
    file_id: u32,
    writer: BufWriter<File>,
    /// Where the next entry will start: every byte accepted so far, flushed
    /// or still buffered.
    offset: u64,
}

impl ActiveFile {
    /// Create data file `file_id` in `dir` and open it for appending. If the
    /// file already exists, appends continue from its current end.
    pub fn create(dir: &Path, file_id: u32) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(name::data_path(dir, file_id))?;
        let offset = file
            .metadata()?
            .len();
        Ok(Self {
            file_id,
            writer: BufWriter::new(file),
            offset,
        })
    }

    pub fn file_id(&self) -> u32 {
        self.file_id
    }

    /// The file's length, counting bytes that are still buffered.
    pub fn len(&self) -> u64 {
        self.offset
    }

    pub fn is_empty(&self) -> bool {
        self.offset == 0
    }

    /// Append one entry and hand it to the OS (a `write`, not an fsync), so a
    /// read through any other handle sees it. Returns where the entry's
    /// *value* starts in the file, which is what the keydir stores.
    pub fn append(&mut self, encoded: &EncodedEntry) -> io::Result<u64> {
        let value_pos = self.append_buffered(encoded)?;
        self.flush()?;
        Ok(value_pos)
    }

    /// [`append`](Self::append) without the flush: the entry may stay in this
    /// handle's buffer, invisible to readers, until the next
    /// [`flush`](Self::flush). For merge, which flushes once per batch and
    /// tells nobody where an entry is before then.
    pub(crate) fn append_buffered(&mut self, encoded: &EncodedEntry) -> io::Result<u64> {
        let bytes = encoded.as_bytes();
        self.writer
            .write_all(bytes)?;
        let entry_len = bytes.len() as u64;
        let value_pos = self.offset + entry_len - encoded.value_len() as u64;
        self.offset += entry_len;
        Ok(value_pos)
    }

    /// Hand every buffered byte to the OS.
    pub(crate) fn flush(&mut self) -> io::Result<()> {
        self.writer
            .flush()
    }

    /// Flush, then fsync the file's data. `sync_data` is enough: the bytes
    /// and the length have to survive, the modification time doesn't.
    pub fn sync(&mut self) -> io::Result<()> {
        self.flush()?;
        self.writer
            .get_ref()
            .sync_data()
    }

    /// Flush, then return another handle onto the same file. Syncing the
    /// returned handle makes durable everything appended before this call,
    /// and can be done without holding whatever lock guards this
    /// `ActiveFile`; group commit (`crate::commit`) does that.
    pub(crate) fn sync_handle(&mut self) -> io::Result<File> {
        self.flush()?;
        self.reader()
    }

    /// Read `len` bytes at `pos`. Only flushed bytes are there to read.
    pub fn read_at(&self, pos: u64, len: u32) -> io::Result<Vec<u8>> {
        pread_exact(
            self.writer
                .get_ref(),
            pos,
            len,
        )
    }

    /// Another handle onto this file: a `dup` of the descriptor, with no
    /// path lookup.
    pub(super) fn reader(&self) -> io::Result<File> {
        self.writer
            .get_ref()
            .try_clone()
    }
}
