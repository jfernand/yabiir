//! Where a merge writes the entries it keeps.

use std::io;
use std::ops::Range;

use super::hint::HintWriter;
use crate::datafile::{ActiveFile, DataFileSet};
use crate::format::{self, Entry};

/// How many entries are written between flushes, unless a file fills up
/// first.
const FLUSH_BATCH_SIZE: usize = 256;

/// Writes a merge's output: data files, each with a hint file, taking their
/// ids from the range reserved for this merge and starting a new file when
/// one reaches `max_file_size`. Nothing is created until there is an entry
/// to write.
///
/// Entries are buffered and flushed once per [`FLUSH_BATCH_SIZE`], instead
/// of once each like the write path does: a flush is a `write` syscall, and
/// one per copied entry was most of a merge's time. The caller must not
/// tell the keydir about an entry before the flush that covers it; each
/// [`write`](Self::write) reports whether it flushed.
pub(super) struct OutputWriter<'a> {
    files: &'a DataFileSet,
    unused_ids: Range<u32>,
    max_file_size: u64,
    current: Option<OutputFile>,
    n_unflushed: usize,
}

struct OutputFile {
    data: ActiveFile,
    hint: HintWriter,
}

/// Where [`OutputWriter::write`] put an entry.
pub(super) struct Written {
    pub(super) file_id: u32,
    pub(super) value_pos: u64,
    /// Whether this entry, and every entry written before it, has been
    /// flushed and can be read.
    pub(super) is_flushed: bool,
}

impl<'a> OutputWriter<'a> {
    pub(super) fn new(files: &'a DataFileSet, ids: Range<u32>, max_file_size: u64) -> Self {
        Self {
            files,
            unused_ids: ids,
            max_file_size,
            current: None,
            n_unflushed: 0,
        }
    }

    /// Append a copy of `entry`, a live entry, and its hint record.
    pub(super) fn write(&mut self, entry: &Entry) -> io::Result<Written> {
        let encoded = format::encode_entry(
            &entry.key,
            &entry.value,
            false,
            entry
                .header
                .timestamp,
        );
        let output = self.current_file()?;
        let file_id = output
            .data
            .file_id();
        let value_pos = output
            .data
            .append_buffered(&encoded)?;
        output
            .hint
            .write(entry, value_pos)?;
        let is_full = output
            .data
            .len()
            >= self.max_file_size;
        self.n_unflushed += 1;

        let is_flushed = if is_full {
            self.close_current_file()?;
            true
        } else if self.n_unflushed >= FLUSH_BATCH_SIZE {
            self.flush()?;
            true
        } else {
            false
        };
        Ok(Written {
            file_id,
            value_pos,
            is_flushed,
        })
    }

    /// Flush and fsync the last output file, and make the new files' names
    /// durable. After this the merge's output survives a crash, and its
    /// input can be removed.
    pub(super) fn finish(&mut self) -> io::Result<()> {
        self.close_current_file()?;
        self.files
            .sync_dir()
    }

    /// The file being written, created first if there is none.
    fn current_file(&mut self) -> io::Result<&mut OutputFile> {
        if self
            .current
            .is_none()
        {
            let file_id = self
                .unused_ids
                .next()
                .ok_or_else(|| io::Error::other("merge ran out of its reserved file ids"))?;
            let data = self
                .files
                .create_active(file_id)?;
            let hint = HintWriter::create(&DataFileSet::hint_path(
                self.files
                    .dir(),
                file_id,
            ))?;
            self.current = Some(OutputFile { data, hint });
        }
        Ok(self
            .current
            .as_mut()
            .expect("just created if it was missing"))
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(output) = &mut self.current {
            output
                .data
                .flush()?;
        }
        self.n_unflushed = 0;
        Ok(())
    }

    /// Fsync the current data file, finish its hint file, and close both.
    fn close_current_file(&mut self) -> io::Result<()> {
        if let Some(mut output) = self
            .current
            .take()
        {
            output
                .data
                .sync()?;
            output
                .hint
                .finish()?;
        }
        self.n_unflushed = 0;
        Ok(())
    }
}
