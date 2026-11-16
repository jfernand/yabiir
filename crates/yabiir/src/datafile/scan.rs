//! Reading a data file from start to end, for recovery and merge.

use std::fs::File;
use std::io::{self, BufReader, Seek, SeekFrom};
use std::path::Path;

use super::pread_exact;
use crate::format::{
    self, EntryRead, FILE_HEADER_SIZE, FileFormat, HEADER_SIZE, Record, RecordOp, RecordRead,
};

/// What a scan found at one position of a data file.
#[derive(Debug)]
pub(crate) enum Scanned {
    /// A complete record with a valid CRC, starting at `offset`. An entry of
    /// a version 1 file reads as a record of one operation, with sequence
    /// number 1.
    Record { offset: u64, record: Record },
    /// A complete record at `offset` whose CRC doesn't match. The scan goes
    /// on with the record after it.
    Corrupt { offset: u64 },
    /// The file ends inside the record that starts at `offset`: a torn
    /// write. Always the last item of a scan.
    Truncated { offset: u64 },
}

/// Every record of the data file at `path`, in file order, tombstones
/// included, whichever format the file is in. The file is read through a
/// buffer, one record at a time, so a scan holds one record in memory, not
/// the file.
///
/// The file's length is taken when the scan starts. Recovery and merge only
/// scan files nothing is writing to, so it stays put.
pub(crate) fn scan(path: &Path) -> io::Result<Scan> {
    let mut file = File::open(path)?;
    let len = file
        .metadata()?
        .len();
    let prefix = pread_exact(&file, 0, len.min(FILE_HEADER_SIZE as u64) as u32)?;
    let format = format::identify(&prefix);
    // A v1 file's first record starts at 0; a v2 file's after its header,
    // or at its end if the header is torn.
    let offset = match format {
        FileFormat::V1 => 0,
        FileFormat::V2 { .. } => prefix.len() as u64,
    };
    file.seek(SeekFrom::Start(offset))?;
    Ok(Scan {
        reader: BufReader::with_capacity(SCAN_BUFFER_SIZE, file),
        len,
        offset,
        format,
        is_done: false,
    })
}

const SCAN_BUFFER_SIZE: usize = 256 * 1024;

pub(crate) struct Scan {
    reader: BufReader<File>,
    /// The file's length when the scan started.
    len: u64,
    /// Where the next record starts.
    offset: u64,
    format: FileFormat,
    is_done: bool,
}

impl Scan {
    /// The format of the file being scanned.
    pub(crate) fn format(&self) -> FileFormat {
        self.format
    }

    /// Read the next v1 entry, as a one-operation record.
    fn next_v1(&mut self, available: u64) -> io::Result<Option<RecordRead>> {
        Ok(
            format::read_entry(&mut self.reader, available)?.map(|read| match read {
                EntryRead::Ok(entry, total_len) => {
                    let value_offset = HEADER_SIZE as u64
                        + entry
                            .key
                            .len() as u64;
                    let record = Record {
                        seq: 1,
                        timestamp: entry
                            .header
                            .timestamp,
                        ops: vec![RecordOp {
                            tombstone: entry
                                .header
                                .tombstone,
                            key: entry.key,
                            value: entry.value,
                            value_offset,
                        }],
                    };
                    RecordRead::Ok(record, total_len)
                }
                EntryRead::Truncated => RecordRead::Truncated,
                EntryRead::CrcMismatch { total_len } => RecordRead::Corrupt { total_len },
            }),
        )
    }
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
        let read = match self.format {
            FileFormat::V1 => self.next_v1(available),
            FileFormat::V2 { .. } => format::read_record(&mut self.reader, available),
        };
        let scanned = match read {
            Err(e) => {
                self.is_done = true;
                return Some(Err(e));
            }
            Ok(None) => {
                self.is_done = true;
                return None;
            }
            Ok(Some(RecordRead::Truncated)) => {
                self.is_done = true;
                Scanned::Truncated { offset }
            }
            Ok(Some(RecordRead::Ok(record, total_len))) => {
                self.offset += total_len;
                Scanned::Record { offset, record }
            }
            Ok(Some(RecordRead::Corrupt { total_len })) => {
                self.offset += total_len;
                Scanned::Corrupt { offset }
            }
        };
        Some(Ok(scanned))
    }
}
