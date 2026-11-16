//! The write path: `write_batch`, and `put` and `delete` as batches of one.

use std::time::Instant;

use super::Engine;
use crate::batch::{Commit, Op};
use crate::error::{Error, Result};
use crate::format::{MAX_KEY_LEN, MAX_VALUE_LEN, OP_HEADER_SIZE};
use crate::writer::Batch;

impl Engine {
    /// Write `ops` as one atomic batch: either every put and delete in it
    /// takes effect, or none does, in this process and after a crash alike.
    ///
    /// Every [`Op::Check`] is evaluated first, against the store as it is
    /// just before the batch (every batch committed or queued before it
    /// counts). If one doesn't hold, nothing is written and the batch fails
    /// with [`Error::ConditionFailed`], naming the check's position. The
    /// puts and deletes then apply in order, so a later operation on a key
    /// wins over an earlier one in the same batch.
    ///
    /// The returned [`Commit`] resolves once the batch is in the log and
    /// visible to readers, and, with
    /// [`Options::should_sync_on_put`](crate::Options::should_sync_on_put),
    /// durable. Readers never see a batch before then, nor part of one. It
    /// resolves to the version the batch gave every key it put (see
    /// [`Engine::get_versioned`]), or to `None` if the batch wrote nothing.
    /// Batches from many threads are written together, sharing one `write`
    /// and one fsync.
    ///
    /// The batch's record takes the current time as its timestamp. An empty
    /// key, a key or value too large for the file format, or a batch too
    /// large for one record fails it at once, as does a read-only handle.
    pub fn write_batch(&self, ops: Vec<Op>) -> Commit {
        self.submit(ops, crate::now_unix(), None)
    }

    /// [`Bitcask::put`](crate::Bitcask::put), with
    /// `between_append_and_publish` run on the writer thread after the
    /// record is written and before the keydir points at it. Production
    /// passes `None`; tests try to run a merge or another write there, and
    /// check that it has to wait until the publish is done.
    pub(super) fn put_with_hook(
        &self,
        key: &[u8],
        value: &[u8],
        timestamp: u32,
        between_append_and_publish: Option<Hook>,
    ) -> Result<()> {
        let start = Instant::now();
        let op = Op::Put {
            key: key.to_vec(),
            value: value.to_vec(),
        };
        self.submit(vec![op], timestamp, between_append_and_publish)
            .wait()?;
        self.metrics
            .record_put(start.elapsed());
        Ok(())
    }

    /// [`Bitcask::delete`](crate::Bitcask::delete), with the same hook as
    /// [`Engine::put_with_hook`]. A key that isn't there needs no tombstone,
    /// and gets none.
    pub(super) fn delete_with_hook(
        &self,
        key: &[u8],
        timestamp: u32,
        between_append_and_publish: Option<Hook>,
    ) -> Result<()> {
        let start = Instant::now();
        let op = Op::Delete { key: key.to_vec() };
        self.submit(vec![op], timestamp, between_append_and_publish)
            .wait()?;
        self.metrics
            .record_delete(start.elapsed());
        Ok(())
    }

    /// Validate `ops` and queue them for the writer.
    fn submit(&self, ops: Vec<Op>, timestamp: u32, hook: Option<Hook>) -> Commit {
        let writer = match self.writer() {
            Ok(writer) => writer,
            Err(e) => return Commit::ready(Err(e)),
        };
        if let Err(e) = check_ops(&ops) {
            return Commit::ready(Err(e));
        }
        let (commit, done) = Commit::pending();
        #[cfg(not(test))]
        let _ = hook;
        writer.submit(Batch {
            ops,
            timestamp,
            done,
            #[cfg(test)]
            between_append_and_publish: hook,
        });
        commit
    }
}

/// A test hook run on the writer thread: see [`Engine::put_with_hook`].
pub(super) type Hook = Box<dyn FnOnce() + Send>;

/// Check every key and value of a batch against the file format's limits,
/// and the batch as a whole against what one record can hold.
fn check_ops(ops: &[Op]) -> Result<()> {
    let mut body_len = 0u64;
    for op in ops {
        let (key, value): (&[u8], &[u8]) = match op {
            Op::Put { key, value } => (key, value),
            Op::Delete { key } | Op::Check { key, .. } => (key, &[]),
        };
        check_key_len(key.len())?;
        check_value_len(value.len())?;
        if !matches!(op, Op::Check { .. }) {
            body_len += OP_HEADER_SIZE as u64 + key.len() as u64 + value.len() as u64;
        }
    }
    if body_len > u32::MAX as u64 {
        return Err(Error::BatchTooLarge);
    }
    Ok(())
}

/// The entry header stores the key's length in 31 bits.
pub(crate) fn check_key_len(len: usize) -> Result<()> {
    if len == 0 {
        Err(Error::EmptyKey)
    } else if len as u64 > MAX_KEY_LEN as u64 {
        Err(Error::KeyTooLarge)
    } else {
        Ok(())
    }
}

/// The entry header stores the value's length in 32 bits.
pub(crate) fn check_value_len(len: usize) -> Result<()> {
    if len as u64 > MAX_VALUE_LEN as u64 {
        Err(Error::ValueTooLarge)
    } else {
        Ok(())
    }
}
