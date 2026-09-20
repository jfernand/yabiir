use crate::datafile::{ActiveFile, DataFileSet};
use crate::format;
use crate::format::Entry;
use std::fs::File;
use std::io;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

/// How many live entries `MergeOutputWriter` writes before flushing, absent
/// an earlier rotation boundary (which already flushes+fsyncs). See this
/// module's doc comment on batched flushing for why this is safe.
const MERGE_FLUSH_BATCH_SIZE: usize = 256;

/// Writes merge output: a data file plus its companion hint file, rotating
/// to `output_id, output_id+1, ...` on the same `max_file_size` threshold
/// normal active files use. Reuses `ActiveFile` for the data-file side;
/// doesn't touch the keydir itself — `merge` handles keydir updates,
/// batched to line up with this writer's own flush batches (plan §7.4 and
/// this module's doc comment on batched flushing).
pub struct MergeOutputWriter<'a> {
    dir: &'a Path,
    next_file_id: &'a AtomicU32,
    max_file_size: u64,
    current_data: ActiveFile,
    current_hint: BufWriter<File>,
    /// Live entries written since the last flush (via `write_unflushed`) —
    /// reset on every flush, whether from hitting `MERGE_FLUSH_BATCH_SIZE`
    /// or from a rotation boundary.
    pending_since_flush: usize,
}

impl<'a> MergeOutputWriter<'a> {
    pub(crate) fn new(
        dir: &'a Path,
        next_file_id: &'a AtomicU32,
        max_file_size: u64,
    ) -> io::Result<Self> {
        let (current_data, current_hint) = Self::open_output(dir, next_file_id)?;
        Ok(Self {
            dir,
            next_file_id,
            max_file_size,
            current_data,
            current_hint,
            pending_since_flush: 0,
        })
    }

    fn open_output(
        dir: &Path,
        next_file_id: &AtomicU32,
    ) -> io::Result<(ActiveFile, BufWriter<File>)> {
        // Claims an id from the *same* counter the engine's write path uses
        // for active-file rotation, so merge-output ids never collide with
        // a concurrently-rotated active file.
        let file_id = next_file_id.fetch_add(1, Ordering::SeqCst);
        let data = ActiveFile::create(dir, file_id)?;
        let hint = BufWriter::new(File::create(DataFileSet::hint_path(dir, file_id))?);
        Ok((data, hint))
    }

    pub(crate) fn current_file_id(&self) -> u32 {
        self.current_data
            .file_id()
    }

    /// Writes one live entry (unflushed) plus its hint record, and rotates
    /// or batch-flushes as needed. Returns `(file_id, value_pos, flushed)`
    /// — `flushed` tells the caller whether this entry's bytes (and every
    /// other still-pending entry's) are now safe to repoint in the keydir.
    pub(crate) fn write_live_entry(&mut self, entry: &Entry) -> io::Result<(u32, u64, bool)> {
        let encoded = format::encode_entry(
            &entry.key,
            &entry.value,
            false,
            entry
                .header
                .timestamp,
        );
        let (file_id, value_pos, _total_len) = self
            .current_data
            .append_buffered(&encoded)?;
        let hint = format::encode_hint(&entry.key, &entry.header, value_pos);
        self.current_hint
            .write_all(hint.as_bytes())?;
        self.pending_since_flush += 1;

        let flushed = self.rotate_or_flush()?;

        Ok((file_id, value_pos, flushed))
    }

    fn rotate_or_flush(&mut self) -> Result<bool, io::Error> {
        if self.is_ready_to_rotate() {
            self.rotate()?; // fsyncs — a strictly stronger guarantee than a flush
            Ok(true)
        } else if self.is_ready_to_flush() {
            self.flush_batch()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn is_ready_to_flush(&self) -> bool {
        self.pending_since_flush >= MERGE_FLUSH_BATCH_SIZE
    }

    fn is_ready_to_rotate(&self) -> bool {
        self.current_data
            .len()
            >= self.max_file_size
    }

    /// A plain flush (not fsync) — same durability step `rotate`/`finish`
    /// do, just without also syncing to disk, since a batch boundary only
    /// needs to make bytes visible to a fresh read, not survive a crash any
    /// more than the rest of this design already promises without
    /// `sync_on_put`.
    fn flush_batch(&mut self) -> io::Result<()> {
        self.current_data
            .flush_only()?;
        self.pending_since_flush = 0;
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.current_data
            .sync()?;
        self.current_hint
            .flush()?; // XXX why not sync?
        let (data, hint) = Self::open_output(self.dir, self.next_file_id)?;
        self.current_data = data;
        self.current_hint = hint;
        self.pending_since_flush = 0;
        Ok(())
    }

    pub(crate) fn finish(mut self) -> io::Result<()> {
        self.current_data
            .sync()?;
        self.current_hint
            .flush()?;
        Ok(())
    }
}
