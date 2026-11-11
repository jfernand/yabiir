use crate::format::Entry;
use crate::keydir::KeydirEntry;
use std::ops::RangeBounds;
use std::vec::Drain;

pub struct PendingQueue(Vec<(Vec<u8>, KeydirEntry, KeydirEntry)>);

impl PendingQueue {
    pub(crate) fn push(&mut self, entry: (Vec<u8>, KeydirEntry, KeydirEntry)) {
        self.0
            .push(entry);
    }
    /// Queues a repointing from an existing entry in file_id to new_file_id, at its new position
    pub(crate) fn queue(
        &mut self,
        entry: Entry,
        file_id: u32,
        new_file_id: u32,
        entry_value_pos: u64,
        new_value_pos: u64,
    ) {
        let timestamp = entry
            .header
            .timestamp;
        let value_size = entry
            .header
            .value_size;
        self.push((
            entry.key,
            KeydirEntry {
                file_id,
                value_size,
                value_pos: entry_value_pos,
                timestamp,
            },
            KeydirEntry {
                file_id: new_file_id,
                value_size,
                value_pos: new_value_pos,
                timestamp,
            },
        ));
    }
}

impl PendingQueue {
    pub(crate) fn new() -> Self {
        Self(Vec::new())
    }

    pub(crate) fn drain<R: RangeBounds<usize>>(
        &mut self,
        range: R,
    ) -> Drain<'_, (Vec<u8>, KeydirEntry, KeydirEntry)> {
        self.0
            .drain(range)
    }

    /// How many repoints are queued right now — observability only (see
    /// `Metrics::record_pending_queue_depth`), not used by merge's own
    /// batching logic (that's driven by `write_live_entry`'s own
    /// `pending_since_flush` counter in `output_writer.rs`, not this).
    pub(crate) fn len(&self) -> usize {
        self.0
            .len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0
            .is_empty()
    }
}
