//! Replaying a hint file: the fast path, which reads no values.

use std::fs;
use std::io;
use std::path::Path;

use super::NextSeq;
use crate::datafile::pread_exact;
use crate::format::{self, FILE_HEADER_SIZE, FileFormat, HintRecord};
use crate::keydir::{Keydir, KeydirEntry};

/// Apply the hint file at `hint_path`, which describes data file `file_id`
/// at `data_path`, to `keydir`. Returns whether it did.
///
/// It returns `false`, with a warning and with `keydir` untouched, if the
/// hint can't be read, fails its whole-file CRC, ends inside a record, or
/// points outside the data file. A hint is only a shortcut, so no fault in
/// one stops the store from opening. The caller then scans the data file instead. Every record
/// is checked before any is applied.
pub(super) fn replay(
    hint_path: &Path,
    data_path: &Path,
    file_id: u32,
    keydir: &mut Keydir,
    next_seq: &mut NextSeq,
) -> io::Result<bool> {
    let data_file = fs::File::open(data_path)?;
    let data_len = data_file
        .metadata()?
        .len();
    let records = fs::read(hint_path)
        .map_err(|e| format!("can't be read ({e})"))
        .and_then(|bytes| validate(&bytes, data_len));
    let records = match records {
        Ok(records) => records,
        Err(reason) => {
            crate::log::warn!(
                "warning: {} {reason}; scanning the data file instead",
                hint_path.display()
            );
            return Ok(false);
        }
    };
    // The data file's header holds its sequence floor; the hint doesn't.
    let prefix = pread_exact(&data_file, 0, data_len.min(FILE_HEADER_SIZE as u64) as u32)?;
    if let FileFormat::V2 { seq_floor } = format::identify(&prefix) {
        next_seq.at_least(seq_floor);
    }
    for record in records {
        next_seq.saw(record.seq);
        apply(keydir, file_id, record);
    }
    Ok(true)
}

/// Parse a hint file's bytes into its records, or say why it can't be
/// trusted. `data_len` is the length of the data file it describes.
fn validate(bytes: &[u8], data_len: u64) -> Result<Vec<HintRecord>, String> {
    let records = format::decode_hint_file(bytes)?;
    for record in &records {
        let value_end = record
            .value_pos
            .saturating_add(record.value_size as u64);
        if value_end > data_len {
            return Err(format!(
                "points past the end of its data file (value at {}, {} bytes, in a file of \
                 {data_len})",
                record.value_pos, record.value_size
            ));
        }
    }
    Ok(records)
}

fn apply(keydir: &mut Keydir, file_id: u32, record: HintRecord) {
    // Merge writes no tombstones to hint files today: it drops them. The
    // format allows one, so it is honoured.
    if record.tombstone {
        keydir.remove(&record.key);
        return;
    }
    let location = KeydirEntry {
        file_id,
        value_size: record.value_size,
        value_pos: record.value_pos,
        timestamp: record.timestamp,
        version: record.seq,
    };
    keydir.insert(&record.key, location);
}
