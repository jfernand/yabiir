//! Replaying a data file: the slow path, which reads every value.

use std::io;
use std::path::Path;

use super::NextSeq;
use crate::datafile::{self, Scanned};
use crate::format::{FileFormat, Record};
use crate::keydir::{Keydir, KeydirEntry};

/// Apply every record of the data file at `path` to `keydir`, in file
/// order, and raise `next_seq` past every sequence number found.
///
/// A record is applied whole or not at all. The scan stops at a truncated
/// record. That is what a crash in the middle of an append leaves at the end
/// of the newest file, so there it is expected (`is_newest`) and silent; in
/// an older file it gets a warning. A record whose CRC doesn't match is
/// skipped, with a warning, and the scan goes on.
pub(super) fn replay(
    path: &Path,
    file_id: u32,
    is_newest: bool,
    keydir: &mut Keydir,
    next_seq: &mut NextSeq,
) -> io::Result<()> {
    let scan = datafile::scan(path)?;
    if let FileFormat::V2 { seq_floor } = scan.format() {
        next_seq.at_least(seq_floor);
    }
    for scanned in scan {
        match scanned? {
            Scanned::Record { offset, record } => {
                next_seq.saw(record.seq);
                apply(keydir, file_id, offset, record);
            }
            Scanned::Corrupt { offset } => crate::log::warn!(
                "warning: {} has a corrupt entry at offset {offset} (CRC mismatch), skipping it",
                path.display()
            ),
            Scanned::Truncated { offset } if !is_newest => crate::log::warn!(
                "warning: {} ends in a truncated entry at offset {offset}, though it is not \
                 the newest file",
                path.display()
            ),
            Scanned::Truncated { .. } => {}
        }
    }
    Ok(())
}

/// Apply one record, found at `offset` of data file `file_id`, operation by
/// operation: a tombstone removes its key, a put becomes the key's entry,
/// with the record's sequence number for its version.
fn apply(keydir: &mut Keydir, file_id: u32, offset: u64, record: Record) {
    for op in record.ops {
        if op.tombstone {
            keydir.remove(&op.key);
            continue;
        }
        let location = KeydirEntry {
            file_id,
            value_size: op
                .value
                .len() as u32,
            value_pos: offset + op.value_offset,
            timestamp: record.timestamp,
            version: record.seq,
        };
        keydir.insert(&op.key, location);
    }
}
