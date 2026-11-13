//! Replaying a hint file: the fast path, which reads no values.

use std::fs;
use std::io;
use std::path::Path;

use crate::format::{self, EntryHeader, HintRead};
use crate::keydir::{Keydir, KeydirEntry};

/// One record of a hint file: a key, and where its value is in the data
/// file.
struct Record {
    header: EntryHeader,
    value_pos: u64,
    key: Vec<u8>,
}

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
) -> io::Result<bool> {
    let data_len = fs::metadata(data_path)?.len();
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
    for record in records {
        apply(keydir, file_id, record);
    }
    Ok(true)
}

/// Parse a hint file's bytes into its records, or say why it can't be
/// trusted. `data_len` is the length of the data file it describes.
fn validate(bytes: &[u8], data_len: u64) -> Result<Vec<Record>, String> {
    let content = format::verify_hint_file(bytes).ok_or("failed its whole-file CRC check")?;
    let mut records = Vec::new();
    let mut cursor = io::Cursor::new(content);
    // Reading from memory can't fail, hence the `expect`.
    while let Some(read) = format::read_hint(&mut cursor).expect("reading from memory") {
        let HintRead::Ok {
            header,
            value_pos,
            key,
        } = read
        else {
            // The CRC covers every byte, so a writer that produced whole
            // records can't lead here. Checked all the same.
            return Err("has a truncated record despite a valid CRC".into());
        };
        let value_end = value_pos.saturating_add(header.value_size as u64);
        if value_end > data_len {
            return Err(format!(
                "points past the end of its data file (value at {value_pos}, {} bytes, in a \
                 file of {data_len})",
                header.value_size
            ));
        }
        records.push(Record {
            header,
            value_pos,
            key,
        });
    }
    Ok(records)
}

fn apply(keydir: &mut Keydir, file_id: u32, record: Record) {
    // Merge writes no tombstones to hint files today: it drops them. The
    // format allows one, so it is honoured.
    if record
        .header
        .tombstone
    {
        keydir.remove(&record.key);
        return;
    }
    let location = KeydirEntry {
        file_id,
        value_size: record
            .header
            .value_size,
        value_pos: record.value_pos,
        timestamp: record
            .header
            .timestamp,
    };
    keydir.insert(&record.key, location);
}
