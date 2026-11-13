//! Replaying a data file: the slow path, which reads every value.

use std::io;
use std::path::Path;

use crate::datafile::{self, Scanned};
use crate::format::Entry;
use crate::keydir::{Keydir, KeydirEntry};

/// Apply every entry of the data file at `path` to `keydir`, in file order.
///
/// The scan stops at a truncated entry. That is what a crash in the middle
/// of an append leaves at the end of the newest file, so there it is
/// expected (`is_newest`) and silent; in an older file it gets a warning. An
/// entry whose CRC doesn't match is skipped, with a warning, and the scan
/// goes on.
pub(super) fn replay(
    path: &Path,
    file_id: u32,
    is_newest: bool,
    keydir: &mut Keydir,
) -> io::Result<()> {
    for scanned in datafile::scan(path)? {
        match scanned? {
            Scanned::Entry { offset, entry } => apply(keydir, file_id, offset, entry),
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

/// Apply one entry, found at `offset` of data file `file_id`: a tombstone
/// removes its key, anything else becomes the key's entry.
fn apply(keydir: &mut Keydir, file_id: u32, offset: u64, entry: Entry) {
    if entry.is_tombstone() {
        keydir.remove(&entry.key);
        return;
    }
    let location = KeydirEntry {
        file_id,
        value_size: entry
            .header
            .value_size,
        value_pos: offset + entry.value_offset(),
        timestamp: entry
            .header
            .timestamp,
    };
    keydir.insert(&entry.key, location);
}
