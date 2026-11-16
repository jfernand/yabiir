//! The file being appended to.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;

use super::{name, pread_exact};
use crate::format::{self, EncodedRecord};

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
    /// Create data file `file_id` in `dir`, in format version 2, and open it
    /// for appending. `seq_floor` goes in its header: the sequence number the
    /// next write will get (see `format::encode_file_header`). The header is
    /// handed to the OS before this returns. If the file already exists,
    /// appends continue from its current end, and it gets no second header.
    pub fn create(dir: &Path, file_id: u32, seq_floor: u64) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(name::data_path(dir, file_id))?;
        let offset = file
            .metadata()?
            .len();
        let mut active = Self {
            file_id,
            writer: BufWriter::new(file),
            offset,
        };
        if offset == 0 {
            active.write_bytes(&format::encode_file_header(seq_floor))?;
            active.flush()?;
        }
        Ok(active)
    }

    pub fn file_id(&self) -> u32 {
        self.file_id
    }

    /// The file's length, counting bytes that are still buffered.
    pub fn len(&self) -> u64 {
        self.offset
    }

    /// Whether the file holds no records: nothing past its header.
    pub fn is_empty(&self) -> bool {
        self.offset <= format::FILE_HEADER_SIZE as u64
    }

    /// Append one record and hand it to the OS (a `write`, not an fsync), so
    /// a read through any other handle sees it. Returns where the record
    /// starts in the file; operation `i`'s value is at that plus
    /// `record.value_offset(i)`, which is what the keydir stores.
    pub fn append(&mut self, record: &EncodedRecord) -> io::Result<u64> {
        let start = self.append_buffered(record)?;
        self.flush()?;
        Ok(start)
    }

    /// [`append`](Self::append) without the flush: the record may stay in
    /// this handle's buffer, invisible to readers, until the next
    /// [`flush`](Self::flush). For merge, which flushes once per batch and
    /// tells nobody where a record is before then, and for the writer, which
    /// appends a group of batches before one flush.
    pub(crate) fn append_buffered(&mut self, record: &EncodedRecord) -> io::Result<u64> {
        self.write_bytes(record.as_bytes())
    }

    /// [`append`](Self::append) a record of one operation, and return where
    /// its value starts.
    #[cfg(test)]
    pub(crate) fn append_value(&mut self, record: &EncodedRecord) -> io::Result<u64> {
        Ok(self.append(record)? + record.value_offset(0))
    }

    /// Buffer `bytes` at the end of the file, and return where they start.
    fn write_bytes(&mut self, bytes: &[u8]) -> io::Result<u64> {
        let start = self.offset;
        self.writer
            .write_all(bytes)?;
        self.offset += bytes.len() as u64;
        Ok(start)
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
