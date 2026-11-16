//! Shared by the unit tests.

use std::path::Path;

pub(crate) use yabiir_testutil::TempDir;

use crate::datafile::{Scanned, scan};
use crate::format::{self, EncodedRecord, OpRef, RecordOp};

/// Every operation of every intact record of the data file at `path`, with
/// the offset its record starts at. Corrupt and truncated records are left
/// out.
pub(crate) fn entries_of(path: &Path) -> Vec<(u64, RecordOp)> {
    scan(path)
        .unwrap()
        .filter_map(|scanned| match scanned.unwrap() {
            Scanned::Record { offset, record } => Some((offset, record.ops)),
            _ => None,
        })
        .flat_map(|(offset, ops)| {
            ops.into_iter()
                .map(move |op| (offset, op))
        })
        .collect()
}

/// A record of one operation, with sequence number 1.
pub(crate) fn one_op(key: &[u8], value: &[u8], tombstone: bool, tstamp: u32) -> EncodedRecord {
    format::encode_record(
        1,
        tstamp,
        &[OpRef {
            key,
            value,
            tombstone,
        }],
    )
}
