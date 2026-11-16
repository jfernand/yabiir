//! Recovery: rebuilding the keydir from a store's files at open. See
//! `docs/bitcask-implementation-plan.md` §6.
//!
//! Files are replayed in ascending id order and each file's entries in file
//! order, and every entry simply replaces what the keydir holds for its key
//! (or removes it, for a tombstone). The log's order is the order of writes,
//! so "last write wins" needs nothing more than that.
//!
//! - `data.rs`: replaying a data file, entry by entry.
//! - `hint.rs`: replaying a hint file instead, when there is a valid one.

mod data;
mod hint;
#[cfg(test)]
mod tests;

use std::io;
use std::path::Path;

use crate::datafile::DataFileSet;
use crate::keydir::Keydir;

/// Replay the data files `file_ids` (ascending, as [`DataFileSet::discover`]
/// returns them) of the store in `dir` into `keydir`.
///
/// A file with a hint file is replayed from the hint, which holds the same
/// keys and positions without the values, if the hint passes validation.
/// Otherwise (no hint file, or one that is damaged) the data file is
/// scanned. A hint is trusted as a whole or not at all: nothing from an
/// invalid one reaches `keydir`.
///
/// A hint file without a data file is never looked at, because only the ids
/// of data files are visited.
///
/// Returns the sequence number the store's next write must get: above every
/// sequence number in the files, and at least every file's sequence floor.
/// The floors matter after a merge, which drops tombstones: a deleted key's
/// last version may be in no record any more, and must still never be
/// handed out again.
pub fn recover(dir: &Path, file_ids: &[u32], keydir: &mut Keydir) -> io::Result<u64> {
    let newest = file_ids
        .last()
        .copied();
    let mut next_seq = NextSeq(1);
    for &file_id in file_ids {
        let data_path = DataFileSet::data_path(dir, file_id);
        let hint_path = DataFileSet::hint_path(dir, file_id);
        let is_replayed_from_hint = hint_path.exists()
            && hint::replay(&hint_path, &data_path, file_id, keydir, &mut next_seq)?;
        if !is_replayed_from_hint {
            let is_newest = Some(file_id) == newest;
            data::replay(&data_path, file_id, is_newest, keydir, &mut next_seq)?;
        }
    }
    Ok(next_seq.0)
}

/// The lowest sequence number the next write can get, raised as recovery
/// finds higher ones.
struct NextSeq(u64);

impl NextSeq {
    /// A record with sequence number `seq` was found.
    fn saw(&mut self, seq: u64) {
        self.at_least(seq.saturating_add(1));
    }

    /// A file's header promises that later writes get `floor` or more.
    fn at_least(&mut self, floor: u64) {
        self.0 = self
            .0
            .max(floor);
    }
}
