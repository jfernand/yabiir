//! Reading a data file from start to end, for recovery and merge.

use std::fs::File;
use std::io::{self, BufReader};
use std::path::Path;

use crate::format::{self, Entry, EntryRead};

/// What a scan found at one position of a data file.
#[derive(Debug)]
pub(crate) enum Scanned {
    /// A complete entry with a valid CRC, starting at `offset`.
    Entry { offset: u64, entry: Entry },
    /// A complete entry at `offset` whose CRC doesn't match. The scan goes on
    /// with the entry after it.
    Corrupt { offset: u64 },
    /// The file ends inside the entry that starts at `offset`: a torn write.
    /// Always the last item of a scan.
    Truncated { offset: u64 },
}

/// Every entry of the data file at `path`, in file order, tombstones
/// included. The file is read through a buffer, one entry at a time, so a
/// scan holds one entry in memory, not the file.
///
/// The file's length is taken when the scan starts. Recovery and merge only
/// scan files nothing is writing to, so it stays put.
pub(crate) fn scan(path: &Path) -> io::Result<Scan> {
    let file = File::open(path)?;
    let len = file
        .metadata()?
        .len();
    Ok(Scan {
        reader: BufReader::with_capacity(SCAN_BUFFER_SIZE, file),
        len,
        offset: 0,
        is_done: false,
    })
}

const SCAN_BUFFER_SIZE: usize = 256 * 1024;

pub(crate) struct Scan {
    reader: BufReader<File>,
    /// The file's length when the scan started.
    len: u64,
    /// Where the next entry starts.
    offset: u64,
    is_done: bool,
}

impl Iterator for Scan {
    type Item = io::Result<Scanned>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.is_done {
            return None;
        }
        let offset = self.offset;
        let available = self
            .len
            .saturating_sub(offset);
        let scanned = match format::read_entry(&mut self.reader, available) {
            Err(e) => {
                self.is_done = true;
                return Some(Err(e));
            }
            Ok(None) => {
                self.is_done = true;
                return None;
            }
            Ok(Some(EntryRead::Truncated)) => {
                self.is_done = true;
                Scanned::Truncated { offset }
            }
            Ok(Some(EntryRead::Ok(entry, total_len))) => {
                self.offset += total_len;
                Scanned::Entry { offset, entry }
            }
            Ok(Some(EntryRead::CrcMismatch { total_len })) => {
                self.offset += total_len;
                Scanned::Corrupt { offset }
            }
        };
        Some(Ok(scanned))
    }
}
