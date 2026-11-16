//! On-disk entry and hint-file layout for Bitcask data files.
//!
//! This module is split into two submodules along the on-disk artifact each
//! one governs — `entry` for `.bitcask.data` records and `hint` for
//! `.bitcask.hint` records — but both submodules are private and everything
//! public is re-exported here, so callers only ever reach this as
//! `format::...`, never `format::entry::...` or `format::hint::...`.
//!
//! Entry layout (16-byte header, all integers little-endian):
//!
//! ```text
//! offset  size  field
//! 0       4     crc            CRC32 of bytes [4..end) of this entry
//! 4       4     tstamp         unix seconds, internal-only
//! 8       4     ksz_and_flags  bit 31 = tombstone flag, bits 0..30 = key length
//! 12      4     value_sz       0 for tombstones (deletes carry no payload)
//! 16      ksz   key
//! 16+ksz  value_sz  value
//! ```
//!
//! Hint file layout: zero or more 20-byte-header records (no per-record
//! CRC — matching upstream Bitcask's own design), followed by a single
//! 4-byte whole-file CRC32 trailer covering every record's bytes. A
//! corrupt, truncated, or missing hint file falls back to scanning the
//! data file — see `crate::recovery` and `format::hint::verify_hint_file`.
//!
//! ```text
//! offset  size  field
//! 0       4     tstamp
//! 4       4     ksz_and_flags  (same encoding as the data file header)
//! 8       4     value_sz
//! 12      8     value_pos      position of the VALUE bytes in the data file
//! 20      ksz   key
//! ...     ...   (repeated for each record)
//! end-4   4     crc32          CRC32 of every record's bytes, concatenated
//! ```
//!
//! ## Format version 2
//!
//! Files written by yabiir 0.9 and later start with a 24-byte header (see
//! `file.rs`) and hold *records* instead of entries. A record is one or
//! more operations under one CRC and one sequence number, so a write batch
//! is recovered whole or not at all (`record.rs`):
//!
//! ```text
//! data file header
//! 0       8     magic          "ybcdat02"
//! 8       8     seq_floor      every later write has a sequence number >= this
//! 16      4     crc32          of bytes [0..16)
//! 20      4     reserved
//!
//! record
//! 0       4     crc            CRC32 of bytes [4..end) of this record
//! 4       4     body_len       bytes after this 24-byte header
//! 8       8     seq            the write's sequence number: the version of
//!                              every key it puts
//! 16      4     tstamp
//! 20      4     n_ops
//! 24      ...   n_ops times:   ksz_and_flags (4), value_sz (4), key, value
//! ```
//!
//! A version 2 hint file is the magic `"ybchnt02"`, then 28-byte-header
//! records (`seq` 8, `tstamp` 4, `ksz_and_flags` 4, `value_sz` 4,
//! `value_pos` 8, key), then the same whole-file CRC32 trailer, covering
//! everything before it. Version 1 files stay readable, and merge rewrites
//! them as version 2.
//!
//! See `docs/bitcask-implementation-plan.md` §1 for the full rationale,
//! including why the tombstone flag lives in the top bit of `ksz` rather
//! than a dedicated flags byte or a magic length sentinel.

mod entry;
mod file;
mod hint;
mod record;

pub use entry::{
    EncodedEntry, Entry, EntryRead, HEADER_SIZE, decode_entry_header, encode_entry, read_entry,
    verify_crc,
};
pub use file::{
    DATA_MAGIC, FILE_HEADER_SIZE, FileFormat, HINT_MAGIC, encode_file_header, identify,
};
pub use hint::{
    EncodedHint, HINT_HEADER_SIZE, HINT_TRAILER_SIZE, HINT_V2_HEADER_SIZE, HintRead, HintRecord,
    decode_hint_file, decode_hint_header, encode_hint, encode_hint_v2, read_hint, verify_hint_file,
};
pub use record::{
    EncodedRecord, OP_HEADER_SIZE, OpRef, RECORD_HEADER_SIZE, Record, RecordOp, RecordRead,
    body_len, encode_record, read_record,
};

use std::io::{self, Read};

const TOMBSTONE_BIT: u32 = 1 << 31;
const KEYSIZE_MASK: u32 = !TOMBSTONE_BIT;

/// Largest key length representable in the 31 bits left over after stealing
/// the top bit of `ksz` for the tombstone flag.
pub const MAX_KEY_LEN: u32 = KEYSIZE_MASK;

/// Largest value length the header's 32-bit `value_sz` field can hold.
pub const MAX_VALUE_LEN: u32 = u32::MAX;

/// Header fields shared by both the data-file entry format and the
/// hint-file record format (everything except the CRC and, for hints, the
/// `value_pos` that only a hint record carries).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntryHeader {
    pub timestamp: u32,
    pub key_size: u32,
    pub value_size: u32,
    pub tombstone: bool,
}

/// Pack a real key length and the tombstone flag into one `u32`, per §1.2 of
/// the implementation plan: the top bit is the flag, the low 31 bits are the
/// key length. Shared by `entry` and `hint` since both formats encode
/// `ksz` the same way.
fn encode_key_size(key_len: u32, tombstone: bool) -> u32 {
    debug_assert!(
        key_len <= MAX_KEY_LEN,
        "key too large ({key_len} bytes, max {MAX_KEY_LEN})"
    );
    key_len | if tombstone { TOMBSTONE_BIT } else { 0 }
}

/// Inverse of [`encode_key_size`].
fn decode_key_size(raw: u32) -> (u32, bool) {
    (raw & KEYSIZE_MASK, raw & TOMBSTONE_BIT != 0)
}

/// Read up to `buf.len()` bytes, stopping early only at EOF. Returns the
/// number of bytes actually read, which is `buf.len()` unless EOF was hit.
/// Needed because a single `Read::read` call is allowed to return short
/// reads for reasons other than EOF. Shared by `entry::read_entry` and
/// `hint::read_hint`.
fn read_up_to<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        match r.read(&mut buf[total..])? {
            0 => break,
            n => total += n,
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ksz_flag_round_trip() {
        assert_eq!(decode_key_size(encode_key_size(0, false)), (0, false));
        assert_eq!(decode_key_size(encode_key_size(0, true)), (0, true));
        assert_eq!(
            decode_key_size(encode_key_size(MAX_KEY_LEN, false)),
            (MAX_KEY_LEN, false)
        );
        assert_eq!(
            decode_key_size(encode_key_size(MAX_KEY_LEN, true)),
            (MAX_KEY_LEN, true)
        );
    }
}
