//! Merge (compaction): copy the live entries of every data file into new,
//! smaller files, each with a hint file, then remove the old files. See
//! `docs/bitcask-implementation-plan.md` §7.
//!
//! ## The steps
//!
//! 1. **Seal.** On the writer thread, between two writes, close the active file and reserve
//!    the ids the output files will use ([`Writer::seal_for_merge`]). Every
//!    data file there is at that instant is input; writes from then on go to
//!    files with higher ids than the output's.
//! 2. **Copy.** Read each input file in order. An entry is live if the
//!    keydir still points at it; copy those to the output, drop the rest
//!    (replaced values, deleted keys, tombstones).
//! 3. **Repoint.** Once a copy is flushed, point its key at it, but only if
//!    the key still points at the entry that was copied. A `put` or `delete`
//!    of the key since then wins, and the copy is left unreferenced.
//! 4. **Finish.** Fsync the output and the directory.
//! 5. **Remove** the input files, oldest first, up to the first one the
//!    keydir still points into: one whose live entries step 2 couldn't
//!    read, because the file is damaged although the keydir, rebuilt from
//!    its hint file, can still read the values. That file and every newer
//!    input stay, and their entries keep being served.
//!
//! Reads and writes go on throughout; only step 1 waits for the writer.
//!
//! ## Why the order of ids matters
//!
//! Recovery (`crate::recovery`) replays files in id order and lets a later
//! entry replace an earlier one, so id order has to agree with the order of
//! writes. A merge's output holds copies of *old* entries. Its ids are
//! therefore reserved in step 1, below the id of every file written to after
//! the merge started: a key written during the merge is replayed after the
//! merge's copy of it, and stays the last write.
//!
//! ## What a crash leaves
//!
//! Before step 5 the input files are all still there, and whatever output
//! exists holds copies of entries the input also has, in files that sort
//! after it: recovery rebuilds the same keydir, and the next merge cleans
//! up. Step 5 removes the oldest file first, so no file is removed while an
//! older one remains: an older value can never outlive the tombstone or the
//! newer value that replaced it. No write that returned `Ok` depends on a
//! merge completing.

mod hint;
mod output;
mod repoints;
#[cfg(test)]
mod tests;

use std::io;

use crate::datafile::{self, DataFileSet, Scanned};
use crate::error::Result;
use crate::format::{Record, RecordOp};
use crate::keydir::{KeydirEntry, SharedKeydir};
use crate::metrics::Metrics;
use crate::writer::{MergeScope, Writer};
use output::OutputWriter;
use repoints::PendingRepoints;

/// Compact the store. Safe to run while other threads `get`, `put` and
/// `delete`, but not while another merge runs; the engine serializes them.
///
/// `before_repoint` runs with a key just before merge tries to repoint it,
/// after the copy is written and flushed. Tests write to the key there;
/// everything else passes a no-op.
pub(crate) fn merge(
    keydir: &SharedKeydir,
    files: &DataFileSet,
    writer: &Writer,
    metrics: &dyn Metrics,
    before_repoint: impl FnMut(&[u8]),
) -> Result<()> {
    let Some(scope) = seal(writer)? else {
        return Ok(());
    };
    let MergeScope {
        input_ids,
        output_ids,
        seq_floor,
    } = scope;
    let mut copier = Copier {
        keydir,
        files,
        metrics,
        output: OutputWriter::new(files, output_ids, seq_floor, writer.max_file_size()),
        pending: PendingRepoints::default(),
        n_copied: 0,
        before_repoint,
    };
    copier.copy_live_entries(&input_ids)?;
    let n_copied = copier.finish()?;
    remove_input_files(keydir, files, &input_ids)?;
    metrics.record_merge_summary(input_ids.len(), n_copied);
    Ok(())
}

#[cfg_attr(
    feature = "tracing",
    tracing::instrument(skip_all, name = "merge::scan")
)]
fn seal(writer: &Writer) -> io::Result<Option<MergeScope>> {
    writer.seal_for_merge()
}

/// Steps 2 to 4: copies live entries from the input to the output and
/// repoints their keys.
struct Copier<'a, H> {
    keydir: &'a SharedKeydir,
    files: &'a DataFileSet,
    metrics: &'a dyn Metrics,
    output: OutputWriter<'a>,
    pending: PendingRepoints,
    n_copied: usize,
    before_repoint: H,
}

impl<H: FnMut(&[u8])> Copier<'_, H> {
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(skip_all, name = "merge::copy_forward")
    )]
    fn copy_live_entries(&mut self, input_ids: &[u32]) -> io::Result<()> {
        for &file_id in input_ids {
            self.copy_live_entries_of(file_id)?;
        }
        Ok(())
    }

    fn copy_live_entries_of(&mut self, file_id: u32) -> io::Result<()> {
        let path = DataFileSet::data_path(
            self.files
                .dir(),
            file_id,
        );
        for scanned in datafile::scan(&path)? {
            match scanned? {
                Scanned::Record { offset, record } => {
                    self.copy_live_ops(file_id, offset, record)?
                }
                Scanned::Corrupt { offset } => crate::log::warn!(
                    "warning: {} has a corrupt entry at offset {offset} (CRC mismatch); \
                     merge is dropping it",
                    path.display()
                ),
                // A file that was active when the process crashed ends this
                // way. Recovery already stopped there; so does the copy.
                Scanned::Truncated { .. } => {}
            }
        }
        Ok(())
    }

    /// Copy each operation of `record`, found at `offset` of data file
    /// `file_id`, that the keydir still points at. A record of several
    /// operations is a batch that committed, so its live operations can be
    /// copied one by one: each becomes a record of its own, with the
    /// batch's sequence number.
    fn copy_live_ops(&mut self, file_id: u32, offset: u64, record: Record) -> io::Result<()> {
        let Record {
            seq,
            timestamp,
            ops,
        } = record;
        for op in ops {
            self.copy_if_live(file_id, offset, seq, timestamp, op)?;
        }
        Ok(())
    }

    /// Copy `op`, of the record at `offset` of data file `file_id`, if the
    /// keydir points at exactly its value.
    fn copy_if_live(
        &mut self,
        file_id: u32,
        offset: u64,
        seq: u64,
        timestamp: u32,
        op: RecordOp,
    ) -> io::Result<()> {
        if op.tombstone {
            return Ok(());
        }
        let value_pos = offset + op.value_offset;
        let old = match self
            .keydir
            .get(&op.key)
        {
            Some(current) if current.file_id == file_id && current.value_pos == value_pos => {
                current
            }
            _ => return Ok(()),
        };

        let written = self
            .output
            .write(seq, timestamp, &op.key, &op.value)?;
        let new = KeydirEntry {
            file_id: written.file_id,
            value_pos: written.value_pos,
            ..old
        };
        self.pending
            .push(op.key, old, new);
        self.n_copied += 1;
        if written.is_flushed {
            self.apply_pending_repoints();
        }
        Ok(())
    }

    /// Repoint the keys whose copies have been flushed.
    fn apply_pending_repoints(&mut self) {
        if self
            .pending
            .len()
            > 0
        {
            self.metrics
                .record_pending_queue_depth(
                    self.pending
                        .len(),
                );
        }
        self.pending
            .apply(self.keydir, &mut self.before_repoint);
    }

    /// Make the output durable, repoint the last batch, and return how many
    /// entries were copied.
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(skip_all, name = "merge::flush")
    )]
    fn finish(mut self) -> io::Result<usize> {
        // The last batch is repointed only after `finish` has flushed it.
        self.output
            .finish()?;
        self.apply_pending_repoints();
        Ok(self.n_copied)
    }
}

/// Step 5. A key that was live in the input points at the output now, or at
/// a newer write, unless step 2 couldn't read it: then the keydir still
/// points into an input file, which must stay.
///
/// Oldest first, stopping at the first file that stays or fails to go, so
/// that a file is never removed while an older one remains (see the module
/// docs). A kept file is merge input again next time.
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(skip_all, name = "merge::remove_old_input_files")
)]
fn remove_input_files(
    keydir: &SharedKeydir,
    files: &DataFileSet,
    input_ids: &[u32],
) -> io::Result<()> {
    // No write since the seal points into the input, so this can't change
    // while the files are removed.
    let oldest_in_use = keydir.with_read(|keydir| {
        keydir
            .iter()
            .map(|(_, entry)| entry.file_id)
            .filter(|id| {
                input_ids
                    .binary_search(id)
                    .is_ok()
            })
            .min()
    });
    for &file_id in input_ids {
        if let Some(in_use) = oldest_in_use
            && file_id >= in_use
        {
            crate::log::warn!(
                "warning: merge kept {} and every newer input file: the keydir still points \
                 into it, at values merge couldn't read from it",
                DataFileSet::data_path(files.dir(), in_use).display()
            );
            break;
        }
        files.remove(file_id)?;
    }
    files.sync_dir()
}
